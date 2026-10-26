//! The browser sign-in end to end against a fake Nomen token endpoint: the hub's real
//! routes, cookies and redirects, with the authorization step played by the test.
//! (`tests/web_probe.py` runs the same flow against the real binary with Nomen introspection.)

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering::SeqCst},
    },
};

use axum::{Form, extract::State, http::StatusCode, response::IntoResponse, routing::post};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::*;
use crate::{
    AppState, audit::Audit, auth::Verifier, config::HubConfig, devices::Devices, hub::Hub,
    link::Links,
};

const OWNER: &str = "300000000000000001";
const OTHER: &str = "300000000000000002";
const WEB_CLIENT: &str = "devagotchi-web";
#[cfg(unix)]
const KEY_FILE: &str = "/nonexistent/hub-key.json";
#[cfg(windows)]
const KEY_FILE: &str = r"C:\nonexistent\hub-key.json";
#[cfg(unix)]
const SOCKET: &str = "/nonexistent/devagotchi-broker.sock";
#[cfg(windows)]
const SOCKET: &str = r"\\.\pipe\devagotchi-web-test-absent";

/// A code the "authorization" step issued: the PKCE challenge it was bound to and the
/// access token it buys.
struct Grant {
    challenge: String,
    token: String,
    expires_in: u64,
}

struct FakeNomen {
    issuer: String,
    redirect_uri: Mutex<String>,
    codes: Mutex<HashMap<String, Grant>>,
    status: AtomicU16,
    /// Codes also buy a refresh token (the app has the refresh grant and the hub asked
    /// for `offline_access`).
    refresh_on_code: AtomicBool,
    /// Live refresh tokens and the access token each buys; spent on use (rotation).
    refreshes: Mutex<HashMap<String, String>>,
    refreshed: AtomicUsize,
    revoked: Mutex<Vec<String>>,
}

impl FakeNomen {
    fn mint_refresh(&self, access: &str) -> String {
        let mut refreshes = self.refreshes.lock().unwrap();
        let token = format!(
            "refresh-token-{}",
            self.refreshed.load(SeqCst) + refreshes.len()
        );
        refreshes.insert(token.clone(), access.to_string());
        token
    }
}

async fn revoke(
    State(fake): State<Arc<FakeNomen>>,
    Form(form): Form<HashMap<String, String>>,
) -> StatusCode {
    let token = form.get("token").cloned().unwrap_or_default();
    if form.get("client_id").map(String::as_str) == Some(WEB_CLIENT) {
        fake.refreshes.lock().unwrap().remove(&token);
        fake.revoked.lock().unwrap().push(token);
    }
    StatusCode::OK
}

async fn token(
    State(fake): State<Arc<FakeNomen>>,
    Form(form): Form<HashMap<String, String>>,
) -> axum::response::Response {
    let status = fake.status.load(SeqCst);
    if status != 200 {
        return StatusCode::from_u16(status).unwrap().into_response();
    }
    let bad = (
        StatusCode::BAD_REQUEST,
        axum::Json(json!({"error": "invalid_grant"})),
    )
        .into_response();
    let field = |key: &str| form.get(key).cloned().unwrap_or_default();
    if field("grant_type") == "refresh_token" && field("client_id") == WEB_CLIENT {
        let Some(access) = fake
            .refreshes
            .lock()
            .unwrap()
            .remove(&field("refresh_token"))
        else {
            return bad;
        };
        let rotated = fake.mint_refresh(&access);
        fake.refreshed.fetch_add(1, SeqCst);
        return axum::Json(json!({"access_token": access, "token_type": "Bearer", "expires_in": 3600, "refresh_token": rotated}))
            .into_response();
    }
    if field("grant_type") != "authorization_code"
        || field("client_id") != WEB_CLIENT
        || field("redirect_uri") != *fake.redirect_uri.lock().unwrap()
    {
        return bad;
    }
    // Codes are single-use, and only the verifier whose hash was in the request redeems one.
    let Some(grant) = fake.codes.lock().unwrap().remove(&field("code")) else {
        return bad;
    };
    if URL_SAFE_NO_PAD.encode(Sha256::digest(field("code_verifier").as_bytes())) != grant.challenge
    {
        return bad;
    }
    let mut answer = json!({"access_token": grant.token, "token_type": "Bearer", "expires_in": grant.expires_in, "id_token": "unused"});
    if fake.refresh_on_code.load(SeqCst) {
        answer["refresh_token"] = json!(fake.mint_refresh(&grant.token));
    }
    axum::Json(answer).into_response()
}

struct World {
    fake: Arc<FakeNomen>,
    url: String,
    http: reqwest::Client,
    /// The `estate` board's project directory (an initialised board the owner reaches).
    board_dir: std::path::PathBuf,
    hub: Hub,
    dir: tempfile::TempDir,
}

async fn world() -> World {
    world_with(None).await
}

/// The world, handing out `downloads` to joining machines (`/join`, `/dist`).
async fn world_with(downloads: Option<Arc<crate::downloads::Downloads>>) -> World {
    let nomen = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fake = Arc::new(FakeNomen {
        issuer: format!("http://{}", nomen.local_addr().unwrap()),
        redirect_uri: Mutex::new(String::new()),
        codes: Mutex::new(HashMap::new()),
        status: AtomicU16::new(200),
        refresh_on_code: AtomicBool::new(false),
        refreshes: Mutex::new(HashMap::new()),
        refreshed: AtomicUsize::new(0),
        revoked: Mutex::new(Vec::new()),
    });
    let app = axum::Router::new()
        .route("/oauth/v2/token", post(token))
        .route("/oauth/v2/revoke", post(revoke))
        .with_state(fake.clone());
    tokio::spawn(async move { axum::serve(nomen, app).await.unwrap() });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    *fake.redirect_uri.lock().unwrap() = format!("{url}/auth/callback");
    let dir = tempfile::tempdir().unwrap();
    let board_dir = dir.path().join("estate");
    let store = board_dir.join(crate::board::BOARD_DIR);
    std::fs::create_dir_all(&store).unwrap();
    crate::board::Board::open(&store.join(crate::board::BOARD_FILE), "estate").unwrap();
    std::fs::write(
        store.join("config.json"),
        json!({"project": "estate", "prefix": "esta", "created_at": 0}).to_string(),
    )
    .unwrap();
    let config = Arc::new(
        HubConfig::parse(
            &json!({
                "listen": "127.0.0.1:1",
                "public_url": url,
                "auth": {"mode": "nomen", "issuer": fake.issuer, "audiences": ["hub-project"],
                         "clients": ["shippin-mcp", WEB_CLIENT], "key_file": KEY_FILE},
                "owner": OWNER,
                "web": {"client_id": WEB_CLIENT},
                "brokers": [{"name": "gold-ws", "socket": SOCKET}, {"name": "gus", "link": true},
                            {"name": "lab", "link": true}, {"name": "old", "link": true}],
                "grants": [{"subject": OTHER, "brokers": ["gus"]}],
                "boards": [{"project": "estate", "dir": board_dir.to_str().unwrap()}]
            })
            .to_string(),
        )
        .unwrap(),
    );
    let devices = Arc::new(Devices::open(&dir.path().join("devices.sqlite3")).unwrap());
    for name in ["gus", "old"] {
        let code = devices
            .issue_code(name, crate::link::random::<16>())
            .unwrap();
        devices.enroll(name, &code, "test-key", true).unwrap();
    }
    devices.revoke("old").unwrap();
    let audit = Arc::new(Audit::open(&dir.path().join("audit.sqlite3")).unwrap());
    let links = Arc::new(Links::new(config.clone(), devices.clone(), audit.clone()));
    // The token check itself (Nomen introspection) has its own tests; here tokens map to
    // subjects directly so the flow around them is what is tested.
    let verifier = Verifier::Dev(HashMap::from([
        ("tok-owner".to_string(), OWNER.to_string()),
        ("tok-other".to_string(), OTHER.to_string()),
    ]));
    let web = WebLogin::new(
        &fake.issuer,
        &["hub-project".to_string()],
        WEB_CLIENT,
        &url,
        dir.path(),
    )
    .unwrap();
    let push = Arc::new(crate::push::Push::open(dir.path(), &url, devices.clone()).unwrap());
    let state = AppState {
        verifier: Arc::new(verifier),
        hub: Hub {
            downloads,
            push: Some(push),
            boards: Arc::new(crate::board::mcp::Boards::from_config(&config, dir.path())),
            config,
            audit,
            links,
        },
        web: Some(Arc::new(web)),
    };
    let hub = state.hub.clone();
    let app = router()
        .layer(axum::middleware::from_fn_with_state(state.clone(), bar))
        .merge(crate::join::routes())
        .with_state(state);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    World {
        fake,
        url,
        http,
        board_dir,
        hub,
        dir,
    }
}

fn location(response: &reqwest::Response) -> String {
    response.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_string()
}

/// `name=value` of the Set-Cookie whose name is `name`, with its attributes.
fn set_cookie(response: &reqwest::Response, name: &str) -> Option<String> {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .find(|v| v.starts_with(&format!("{name}=")))
}

fn cookie_pair(set_cookie: &str) -> String {
    set_cookie.split(';').next().unwrap().to_string()
}

fn query(url: &str) -> HashMap<String, String> {
    let query = url.split_once('?').map(|(_, q)| q).unwrap_or_default();
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (k.to_string(), percent_decode(v)))
        .collect()
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            out.push(u8::from_str_radix(&value[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

impl World {
    async fn get(&self, path: &str, cookie: Option<&str>) -> reqwest::Response {
        let mut request = self.http.get(format!("{}{path}", self.url));
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        request.send().await.unwrap()
    }

    /// Start a sign-in, then play Nomen's authorization step: issue a code bound to the
    /// challenge the hub sent. Returns the login cookie, the state and the code.
    async fn authorize(
        &self,
        return_to: &str,
        token: &str,
        expires_in: u64,
    ) -> (String, String, String) {
        let start = self
            .get(&format!("/auth/login?return_to={return_to}"), None)
            .await;
        assert_eq!(start.status(), StatusCode::SEE_OTHER);
        let to = location(&start);
        assert!(
            to.starts_with(&format!("{}/oauth/v2/authorize?", self.fake.issuer)),
            "{to}"
        );
        let params = query(&to);
        assert_eq!(params["client_id"], WEB_CLIENT);
        assert_eq!(
            params["redirect_uri"],
            format!("{}/auth/callback", self.url)
        );
        assert_eq!(params["response_type"], "code");
        assert_eq!(params["code_challenge_method"], "S256");
        assert_eq!(
            params["scope"],
            "openid profile offline_access urn:zitadel:iam:user:resourceowner urn:zitadel:iam:org:project:id:hub-project:aud"
        );
        let login = set_cookie(&start, "dg_login").expect("login cookie");
        assert!(
            login.contains("HttpOnly")
                && login.contains("SameSite=Lax")
                && login.contains("Max-Age=600"),
            "{login}"
        );
        let code = format!("code-{}", self.fake.codes.lock().unwrap().len());
        self.fake.codes.lock().unwrap().insert(
            code.clone(),
            Grant {
                challenge: params["code_challenge"].clone(),
                token: token.into(),
                expires_in,
            },
        );
        (cookie_pair(&login), params["state"].clone(), code)
    }

    async fn sign_in(&self, token: &str) -> String {
        let (login, state, code) = self.authorize("/account", token, 3600).await;
        let back = self
            .get(
                &format!("/auth/callback?code={code}&state={state}"),
                Some(&login),
            )
            .await;
        assert_eq!(back.status(), StatusCode::SEE_OTHER);
        cookie_pair(&set_cookie(&back, "dg_session").expect("session cookie"))
    }
}

#[tokio::test]
async fn signing_in_shows_the_account_and_machines_and_signing_out_ends_it() {
    let w = world().await;
    let first = w.get("/account", None).await;
    assert_eq!(first.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&first), "/auth/login?return_to=/account");

    let (login, state, code) = w.authorize("/account", "tok-owner", 3600).await;
    let back = w
        .get(
            &format!("/auth/callback?code={code}&state={state}"),
            Some(&login),
        )
        .await;
    assert_eq!(back.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&back), "/account");
    let session = set_cookie(&back, "dg_session").unwrap();
    assert!(
        session.contains("HttpOnly")
            && session.contains("SameSite=Lax")
            && session.contains("Max-Age=3600"),
        "{session}"
    );
    assert!(
        set_cookie(&back, "dg_login").unwrap().contains("Max-Age=0"),
        "the login cookie is cleared"
    );
    let session = cookie_pair(&session);
    assert!(
        !session.contains("tok-owner"),
        "the browser never holds the token"
    );

    let page = w.get("/account", Some(&session)).await;
    assert_eq!(page.status(), StatusCode::OK);
    let csp = page.headers()[header::CONTENT_SECURITY_POLICY]
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        csp.starts_with("default-src 'none'") && csp.contains("frame-ancestors 'none'"),
        "{csp}"
    );
    let html = page.text().await.unwrap();
    assert!(
        html.contains(OWNER) && html.contains("You own this hub"),
        "{html}"
    );
    for (machine, status) in [
        ("gold-ws", "offline"),
        ("gus", "offline"),
        ("lab", "not_enrolled"),
        ("old", "revoked"),
    ] {
        assert!(
            html.contains(&format!("<td>{machine}</td>")),
            "{machine} listed"
        );
        assert!(
            html.contains(&format!("status {status}")),
            "{machine} is {status}"
        );
    }

    let me: Value = w
        .get("/api/v1/me", Some(&session))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(me["subject"], OWNER);
    assert_eq!(me["owner"], true);
    let statuses: Vec<(String, String, String)> = me["machines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["name"].as_str().unwrap().into(),
                m["kind"].as_str().unwrap().into(),
                m["status"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        statuses,
        [
            ("gold-ws", "local", "offline"),
            ("gus", "linked", "offline"),
            ("lab", "linked", "not_enrolled"),
            ("old", "linked", "revoked")
        ]
        .map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string()))
    );
    assert!(me["machines"][1]["enrolled_at"].as_u64().is_some());

    let out = w
        .http
        .post(format!("{}/auth/logout", w.url))
        .header(header::COOKIE, &session)
        .send()
        .await
        .unwrap();
    assert_eq!(out.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&out), "/signed-out");
    assert!(
        set_cookie(&out, "dg_session")
            .unwrap()
            .contains("Max-Age=0")
    );
    let after = w.get("/account", Some(&session)).await;
    assert_eq!(
        after.status(),
        StatusCode::SEE_OTHER,
        "the old session id no longer works"
    );
    let me = w.get("/api/v1/me", Some(&session)).await;
    assert_eq!(me.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_person_sees_only_their_granted_machines_by_session_or_bearer() {
    let w = world().await;
    let session = w.sign_in("tok-other").await;
    let me: Value = w
        .get("/api/v1/me", Some(&session))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(me["owner"], false);
    assert_eq!(
        me["machines"],
        json!([{"name": "gus", "kind": "linked", "status": "offline", "enrolled_at": me["machines"][0]["enrolled_at"]}])
    );
    let html = w
        .get("/account", Some(&session))
        .await
        .text()
        .await
        .unwrap();
    assert!(
        html.contains("<td>gus</td>")
            && !html.contains("gold-ws")
            && !html.contains("You own this hub")
    );

    let by_bearer: Value = w
        .http
        .get(format!("{}/api/v1/me", w.url))
        .bearer_auth("tok-other")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(by_bearer["subject"], OTHER);
    assert_eq!(by_bearer["via"], "dev");
    let bad = w
        .http
        .get(format!("{}/api/v1/me", w.url))
        .bearer_auth("nope")
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        w.get("/api/v1/me", None).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn the_callback_needs_this_browsers_state_once_and_the_right_verifier() {
    let w = world().await;
    let (login, state, code) = w.authorize("/account", "tok-owner", 3600).await;
    // No login cookie, or another browser's: refused, and the sign-in stays usable.
    let bare = w
        .get(&format!("/auth/callback?code={code}&state={state}"), None)
        .await;
    assert_eq!(bare.status(), StatusCode::BAD_REQUEST);
    assert!(set_cookie(&bare, "dg_session").is_none());
    let (other_login, _, _) = w.authorize("/account", "tok-owner", 3600).await;
    let crossed = w
        .get(
            &format!("/auth/callback?code={code}&state={state}"),
            Some(&other_login),
        )
        .await;
    assert_eq!(crossed.status(), StatusCode::BAD_REQUEST);
    // The right browser finishes; replaying the same callback does not sign in again.
    let ok = w
        .get(
            &format!("/auth/callback?code={code}&state={state}"),
            Some(&login),
        )
        .await;
    assert_eq!(ok.status(), StatusCode::SEE_OTHER);
    let replay = w
        .get(
            &format!("/auth/callback?code={code}&state={state}"),
            Some(&login),
        )
        .await;
    assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
    assert!(set_cookie(&replay, "dg_session").is_none());

    // A code bound to another challenge (someone else's PKCE) is refused by Nomen.
    let (login, state, code) = w.authorize("/account", "tok-owner", 3600).await;
    w.fake
        .codes
        .lock()
        .unwrap()
        .get_mut(&code)
        .unwrap()
        .challenge = "not-this-one".into();
    let wrong = w
        .get(
            &format!("/auth/callback?code={code}&state={state}"),
            Some(&login),
        )
        .await;
    assert_eq!(wrong.status(), StatusCode::BAD_REQUEST);
    assert!(set_cookie(&wrong, "dg_session").is_none());
}

#[tokio::test]
async fn refusals_outages_and_cancelled_sign_ins_never_open_a_session() {
    let w = world().await;
    // Nomen issued a token the hub does not accept (another client, another project...).
    let (login, state, code) = w.authorize("/account", "tok-unknown", 3600).await;
    let refused = w
        .get(
            &format!("/auth/callback?code={code}&state={state}"),
            Some(&login),
        )
        .await;
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    assert!(set_cookie(&refused, "dg_session").is_none());

    // Nomen's token endpoint fails: an outage, not a refusal.
    let (login, state, code) = w.authorize("/account", "tok-owner", 3600).await;
    w.fake.status.store(502, SeqCst);
    let down = w
        .get(
            &format!("/auth/callback?code={code}&state={state}"),
            Some(&login),
        )
        .await;
    assert_eq!(down.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(set_cookie(&down, "dg_session").is_none());
    w.fake.status.store(200, SeqCst);

    // The person cancels at Nomen; whatever comes back in `error` is shown inert.
    let (login, state, _) = w.authorize("/account", "tok-owner", 3600).await;
    let cancelled = w
        .get(
            &format!("/auth/callback?error=access_denied%3Cscript%3E&state={state}"),
            Some(&login),
        )
        .await;
    assert_eq!(cancelled.status(), StatusCode::BAD_REQUEST);
    let html = cancelled.text().await.unwrap();
    assert!(
        html.contains("access_deniedscript") && !html.contains("<script>"),
        "{html}"
    );
}

#[tokio::test]
async fn sessions_end_with_the_token_and_return_paths_stay_on_the_hub() {
    let w = world().await;
    let (login, state, code) = w.authorize("//evil.example/steal", "tok-owner", 1).await;
    let back = w
        .get(
            &format!("/auth/callback?code={code}&state={state}"),
            Some(&login),
        )
        .await;
    assert_eq!(
        location(&back),
        "/account",
        "only a path on this hub is a return target"
    );
    let session = cookie_pair(&set_cookie(&back, "dg_session").unwrap());
    assert_eq!(
        w.get("/account", Some(&session)).await.status(),
        StatusCode::OK
    );
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let expired = w.get("/account", Some(&session)).await;
    assert_eq!(
        expired.status(),
        StatusCode::SEE_OTHER,
        "the session ended with its one-second token"
    );
}

#[test]
fn return_paths_cookies_dates_and_escaping() {
    for (asked, kept) in [
        ("/account", "/account"),
        ("/account?tab=machines", "/account?tab=machines"),
        ("//evil.example", "/account"),
        ("https://evil.example", "/account"),
        ("/\\evil.example", "/account"),
        ("/a\"><script>", "/account"),
        ("", "/account"),
    ] {
        assert_eq!(safe_return_to(asked), kept, "{asked}");
    }
    let state = tempfile::tempdir().unwrap();
    let public = WebLogin::new(
        "https://id.example",
        &["p".into()],
        "web",
        "https://hub.example",
        state.path(),
    )
    .unwrap();
    assert_eq!(public.session_cookie(), "__Host-dg_session");
    let cookie = public.cookie(public.session_cookie(), "abc", 60);
    assert!(
        cookie.ends_with("; Secure") && cookie.contains("Path=/") && !cookie.contains("Domain"),
        "{cookie}"
    );
    assert_eq!(pages::utc(951_782_400), "2000-02-29 00:00 UTC");
    assert_eq!(pages::utc(1_792_713_600 + 3_723), "2026-10-23 01:02 UTC");
    assert_eq!(
        pages::escape("<img src=x onerror='a'>&\""),
        "&lt;img src=x onerror=&#39;a&#39;&gt;&amp;&quot;"
    );
}

#[tokio::test]
async fn names_from_nomen_are_escaped_on_the_page() {
    let response = pages::account(
        &pages::Viewer {
            subject: OWNER,
            name: Some("<img src=x onerror=alert(1)>"),
            owner: false,
        },
        &[],
    );
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        html.contains("&lt;img src=x onerror=alert(1)&gt;") && !html.contains("<img"),
        "{html}"
    );
    assert!(html.contains("No machines yet"));
}

#[tokio::test]
async fn the_public_pages_need_no_sign_in_and_name_the_operator() {
    let w = world().await;
    for (path, heading) in [
        ("/docs", "<h1>Devagotchi</h1>"),
        ("/privacy", "<h1>Privacy policy</h1>"),
        ("/terms", "<h1>Terms of service</h1>"),
        ("/support", "<h1>Support</h1>"),
    ] {
        let response = w.get(path, None).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert!(
            response
                .headers()
                .get(header::CONTENT_SECURITY_POLICY)
                .is_some_and(|csp| csp.to_str().unwrap().starts_with("default-src 'none'")),
            "{path} keeps the strict policy"
        );
        let body = response.text().await.unwrap();
        assert!(body.contains(heading), "{path}");
        assert!(!body.contains("<script"), "{path} has no script");
        for link in ["/docs", "/privacy", "/terms", "/support"] {
            assert!(
                body.contains(&format!("href=\"{link}\"")),
                "{path} links {link}"
            );
        }
    }
    let privacy = w.get("/privacy", None).await.text().await.unwrap();
    assert!(privacy.contains("AngryVibes LLC"));
    let docs = w.get("/docs", None).await.text().await.unwrap();
    assert!(docs.contains("https://app.devagotchi.com/mcp"));
}

#[tokio::test]
async fn a_person_approves_on_the_phone_page_and_the_agent_sees_it() {
    use crate::board::{Actor, open_project};
    let w = world().await;
    let (_, board) = open_project(&w.board_dir).unwrap();
    let asked = board
        .request_approval(
            &Actor::agent("claude"),
            "Restart the hub on Texas",
            "systemctl restart devagotchi-hub",
            "new build",
            "start the old build",
        )
        .unwrap();

    let owner = w.sign_in("tok-owner").await;
    let page = w.get("/approvals", Some(&owner)).await;
    assert_eq!(page.status(), StatusCode::OK);
    let body = page.text().await.unwrap();
    assert!(body.contains("Restart the hub on Texas"));
    assert!(body.contains(&format!("value=\"{}\"", asked.id)));
    assert_eq!(
        body.matches("<script").count(),
        1,
        "one script, the hub's own file; nothing inline"
    );
    assert!(body.contains("<script src=\"/assets/push.js\" defer></script>"));

    let form = [
        ("project", "estate".to_string()),
        ("id", asked.id.clone()),
        ("version", asked.version.to_string()),
        ("verdict", "approved".to_string()),
        ("note", "go".to_string()),
    ];
    let post = |site: &'static str| {
        w.http
            .post(format!("{}/approvals/decide", w.url))
            .header(header::COOKIE, &owner)
            .header("sec-fetch-site", site)
            .form(&form)
    };
    let refused = post("cross-site").send().await.unwrap();
    assert_eq!(
        refused.status(),
        StatusCode::FORBIDDEN,
        "another site's form is refused"
    );
    assert_eq!(board.approval_status(&asked.id).unwrap().status, "pending");

    let decided = post("same-origin").send().await.unwrap();
    assert_eq!(decided.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&decided), format!("/approvals?done={}", asked.id));
    let status = board.approval_status(&asked.id).unwrap();
    assert_eq!(status.status, "approved");
    assert_eq!(status.decided_by.as_deref(), Some(OWNER));
    assert_eq!(status.note.as_deref(), Some("go"));
    let after = w
        .get("/approvals", Some(&owner))
        .await
        .text()
        .await
        .unwrap();
    assert!(after.contains("Nothing is waiting for you."));

    let twice = post("same-origin").send().await.unwrap();
    assert_eq!(
        twice.status(),
        StatusCode::CONFLICT,
        "a decided approval stays decided"
    );

    let other = w.sign_in("tok-other").await;
    let page = w.get("/approvals", Some(&other)).await;
    assert_eq!(page.status(), StatusCode::FORBIDDEN, "only people approve");
    let signed_out = w.get("/approvals", None).await;
    assert_eq!(signed_out.status(), StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn notify_this_phone_then_an_approval_buzzes_it_encrypted() {
    use crate::board::{Actor, open_project};
    use aws_lc_rs::{
        agreement::{ECDH_P256, PrivateKey},
        rand::{SecureRandom, SystemRandom},
    };

    // A fake push service: records what it is sent; `/gone/*` answers 410.
    type Seen = Arc<Mutex<Vec<(axum::http::HeaderMap, Vec<u8>)>>>;
    let seen: Seen = Arc::default();
    async fn receive(
        State(seen): State<Seen>,
        axum::extract::Path(id): axum::extract::Path<String>,
        headers: axum::http::HeaderMap,
        body: axum::body::Bytes,
    ) -> StatusCode {
        seen.lock().unwrap().push((headers, body.to_vec()));
        if id.starts_with("gone") {
            StatusCode::GONE
        } else {
            StatusCode::CREATED
        }
    }
    let service = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", service.local_addr().unwrap());
    let app = axum::Router::new()
        .route("/push/{id}", post(receive))
        .with_state(seen.clone());
    tokio::spawn(async move { axum::serve(service, app).await.unwrap() });

    let w = world().await;
    let page = {
        let owner = w.sign_in("tok-owner").await;
        let body = w.get("/approvals", Some(&owner)).await;
        let csp = body.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .to_string();
        assert!(csp.contains("script-src 'self'") && csp.contains("worker-src 'self'"));
        (owner, body.text().await.unwrap())
    };
    let (owner, html) = page;
    let key = w.hub.push.as_ref().unwrap().public_key.clone();
    assert!(
        html.contains(&format!("data-key=\"{key}\"")),
        "the page carries the public key only"
    );
    assert!(html.contains("/assets/push.js") && html.contains("/manifest.webmanifest"));
    for (path, kind) in [
        ("/sw.js", "text/javascript"),
        ("/assets/push.js", "text/javascript"),
        ("/manifest.webmanifest", "application/manifest+json"),
        ("/assets/icon.svg", "image/svg+xml"),
    ] {
        let r = w.get(path, None).await;
        assert_eq!(r.status(), StatusCode::OK, "{path}");
        assert!(
            r.headers()[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with(kind),
            "{path}"
        );
    }

    let phone = PrivateKey::generate(&ECDH_P256).unwrap();
    let mut auth = [0u8; 16];
    SystemRandom::new().fill(&mut auth).unwrap();
    let b64 = |b: &[u8]| URL_SAFE_NO_PAD.encode(b);
    let subscribe = |endpoint: String, site: &'static str, cookie: String| {
        let body = json!({"endpoint": endpoint, "keys": {"p256dh": b64(phone.compute_public_key().unwrap().as_ref()), "auth": b64(&auth)}});
        w.http
            .post(format!("{}/push/subscribe", w.url))
            .header(header::COOKIE, cookie)
            .header("sec-fetch-site", site)
            .json(&body)
    };
    let refused = subscribe(format!("{base}/push/phone"), "cross-site", owner.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    let other = w.sign_in("tok-other").await;
    let refused = subscribe(format!("{base}/push/phone"), "same-origin", other)
        .send()
        .await
        .unwrap();
    assert_eq!(
        refused.status(),
        StatusCode::FORBIDDEN,
        "only people get approval pushes"
    );
    for endpoint in [
        format!("{base}/push/phone"),
        format!("{base}/push/gone-tablet"),
    ] {
        let ok = subscribe(endpoint, "same-origin", owner.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::NO_CONTENT);
    }

    let (_, board) = open_project(&w.board_dir).unwrap();
    let asked = board
        .request_approval(
            &Actor::agent("claude"),
            "Restart the hub on Texas",
            "systemctl restart devagotchi-hub",
            "",
            "",
        )
        .unwrap();
    assert_eq!(
        w.hub.announce_approval("estate", &asked),
        2,
        "the owner's two phones are sent the push"
    );
    for _ in 0..100 {
        if seen.lock().unwrap().len() >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "both subscriptions were tried");
    let (headers, body) = &seen[0];
    assert_eq!(headers["content-encoding"], "aes128gcm");
    assert_eq!(headers["ttl"], "86400");
    let authorization = headers["authorization"].to_str().unwrap();
    assert!(
        authorization.starts_with("vapid t=") && authorization.ends_with(&format!(", k={key}"))
    );
    let payload: Value =
        serde_json::from_slice(&crate::push::decrypt(&phone, &auth, body)).unwrap();
    assert_eq!(payload["title"], "Approval needed");
    assert_eq!(payload["body"], "Restart the hub on Texas");
    assert_eq!(payload["url"], format!("/approvals#estate-{}", asked.id));
    assert!(
        !payload.to_string().contains("systemctl"),
        "the command never travels in a push"
    );
    let left = w
        .hub
        .push
        .as_ref()
        .unwrap()
        .devices()
        .push_for(OWNER)
        .unwrap();
    assert_eq!(
        left.len(),
        1,
        "the subscription the service called gone is dropped"
    );
    assert!(left[0].endpoint.ends_with("/push/phone"));
}

#[tokio::test]
async fn the_phone_stays_signed_in_across_a_restart_and_through_refreshes() {
    let w = world().await;
    w.fake.refresh_on_code.store(true, SeqCst);
    let (login, state, code) = w.authorize("/approvals", "tok-owner", 1).await;
    let back = w
        .get(
            &format!("/auth/callback?code={code}&state={state}"),
            Some(&login),
        )
        .await;
    let set = set_cookie(&back, "dg_session").unwrap();
    assert!(
        set.contains(&format!("Max-Age={}", 90 * 24 * 3600)),
        "with a refresh token the phone keeps its session for 90 days: {set}"
    );
    let session = cookie_pair(&set);

    // The one-second access token is renewed through Nomen on the next page; the
    // refresh token it was renewed with is spent and replaced.
    assert_eq!(
        w.get("/approvals", Some(&session)).await.status(),
        StatusCode::OK
    );
    assert_eq!(w.fake.refreshed.load(SeqCst), 1);
    let live: Vec<String> = w.fake.refreshes.lock().unwrap().keys().cloned().collect();
    assert_eq!(live.len(), 1, "rotation leaves one live refresh token");

    // Neither token is on disk in the clear.
    for name in ["sessions.sqlite3", "sessions.sqlite3-wal"] {
        let bytes = std::fs::read(w.dir.path().join(name)).unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains("tok-owner") && !text.contains("refresh-token-"),
            "{name} holds a token in the clear"
        );
    }

    // The hub restarts: a new sign-in service over the same state directory finds the
    // session, with no new sign-in and no extra refresh.
    let restarted = WebLogin::new(
        &w.fake.issuer,
        &["hub-project".to_string()],
        WEB_CLIENT,
        &w.url,
        w.dir.path(),
    )
    .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(header::COOKIE, session.parse().unwrap());
    assert!(matches!(restarted.token(&headers).await, SessionToken::Token(t) if t == "tok-owner"));
    assert_eq!(w.fake.refreshed.load(SeqCst), 1);

    // Signing out ends the session and revokes its refresh token at Nomen.
    let out = w
        .http
        .post(format!("{}/auth/logout", w.url))
        .header(header::COOKIE, &session)
        .send()
        .await
        .unwrap();
    assert_eq!(out.status(), StatusCode::SEE_OTHER);
    for _ in 0..50 {
        if !w.fake.revoked.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(*w.fake.revoked.lock().unwrap(), live);
    assert_eq!(
        w.get("/approvals", Some(&session)).await.status(),
        StatusCode::SEE_OTHER
    );
    assert!(matches!(
        restarted.token(&headers).await,
        SessionToken::None
    ));

    // Nomen refusing a refresh (account disabled, token revoked) signs the phone out.
    let (login, state, code) = w.authorize("/approvals", "tok-owner", 1).await;
    let back = w
        .get(
            &format!("/auth/callback?code={code}&state={state}"),
            Some(&login),
        )
        .await;
    let due = cookie_pair(&set_cookie(&back, "dg_session").unwrap());
    w.fake.refreshes.lock().unwrap().clear();
    let refused = w.get("/approvals", Some(&due)).await;
    assert_eq!(refused.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&refused), "/auth/login?return_to=/approvals");
    assert_eq!(
        w.get("/approvals", Some(&due)).await.status(),
        StatusCode::SEE_OTHER,
        "and the session is gone, not retried"
    );
}

#[tokio::test]
async fn every_page_says_whether_you_are_signed_in_with_one_tap_in_or_out() {
    let w = world().await;
    // Signed out: `/` explains and offers one way in; every page offers Sign in back to it.
    let home = w.get("/", None).await;
    assert_eq!(
        home.status(),
        StatusCode::OK,
        "no bounce to the identity provider"
    );
    let html = home.text().await.unwrap();
    assert!(html.contains("Sign in with Shippin"), "{html}");
    let docs = w.get("/docs", None).await.text().await.unwrap();
    assert!(
        docs.contains("href=\"/auth/login?return_to=%2Fdocs\">Sign in</a>"),
        "{docs}"
    );
    assert!(!docs.contains("Sign out"));

    // Signed in: `/` is your machines, and every page has Approvals, Machines and Sign out.
    let session = w.sign_in("tok-owner").await;
    let home = w.get("/", Some(&session)).await;
    assert_eq!(home.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&home), "/account");
    for path in ["/docs", "/approvals", "/account"] {
        let html = w.get(path, Some(&session)).await.text().await.unwrap();
        assert!(
            html.contains("<form method=\"post\" action=\"/auth/logout\"><button type=\"submit\">Sign out</button></form>")
                && html.contains("<a href=\"/approvals\">Approvals</a>")
                && !html.contains(">Sign in</a>"),
            "{path}: {html}"
        );
    }

    // One tap out, and the bar says so straight away.
    let out = w
        .http
        .post(format!("{}/auth/logout", w.url))
        .header(header::COOKIE, &session)
        .send()
        .await
        .unwrap();
    assert_eq!(location(&out), "/signed-out");
    let after = w.get("/docs", Some(&session)).await.text().await.unwrap();
    assert!(
        after.contains(">Sign in</a>") && !after.contains("Sign out"),
        "{after}"
    );
}

#[tokio::test]
async fn a_new_board_says_how_to_start_and_one_tap_tests_the_phone() {
    use crate::board::{Actor, Filter, open_project};
    use aws_lc_rs::{
        agreement::{ECDH_P256, PrivateKey},
        rand::{SecureRandom, SystemRandom},
    };

    type Seen = Arc<Mutex<Vec<Vec<u8>>>>;
    let seen: Seen = Arc::default();
    async fn receive(State(seen): State<Seen>, body: axum::body::Bytes) -> StatusCode {
        seen.lock().unwrap().push(body.to_vec());
        StatusCode::CREATED
    }
    let service = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", service.local_addr().unwrap());
    let app = axum::Router::new()
        .route("/push/phone", post(receive))
        .with_state(seen.clone());
    tokio::spawn(async move { axum::serve(service, app).await.unwrap() });

    let w = world().await;
    let owner = w.sign_in("tok-owner").await;
    let html = w
        .get("/approvals", Some(&owner))
        .await
        .text()
        .await
        .unwrap();
    assert!(
        html.contains("action=\"/approvals/test\"") && html.contains("Send me a test approval"),
        "{html}"
    );
    // Opening the never-used board put the getting-started steps on it.
    let (_, board) = open_project(&w.board_dir).unwrap();
    let tickets = board.list(&Filter::default()).unwrap();
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0].title, crate::board::welcome::TITLE);
    assert!(
        tickets[0]
            .description
            .contains(&format!("{}/approvals", w.url))
    );

    let post_test = |cookie: &str, site: &'static str| {
        w.http
            .post(format!("{}/approvals/test", w.url))
            .header(header::COOKIE, cookie.to_string())
            .header("sec-fetch-site", site)
            .send()
    };
    let no_phone = post_test(&owner, "same-origin").await.unwrap();
    assert_eq!(no_phone.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&no_phone), "/approvals?test=no-phone");
    let html = w
        .get("/approvals?test=no-phone", Some(&owner))
        .await
        .text()
        .await
        .unwrap();
    assert!(html.contains("Tap Notify this phone first"), "{html}");
    assert_eq!(
        post_test(&owner, "cross-site").await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let other = w.sign_in("tok-other").await;
    assert_eq!(
        post_test(&other, "same-origin").await.unwrap().status(),
        StatusCode::FORBIDDEN,
        "an account with no board it decides on cannot send one"
    );

    let phone = PrivateKey::generate(&ECDH_P256).unwrap();
    let mut auth = [0u8; 16];
    SystemRandom::new().fill(&mut auth).unwrap();
    let b64 = |b: &[u8]| URL_SAFE_NO_PAD.encode(b);
    let subscribed = w
        .http
        .post(format!("{}/push/subscribe", w.url))
        .header(header::COOKIE, &owner)
        .header("sec-fetch-site", "same-origin")
        .json(&json!({"endpoint": format!("{base}/push/phone"),
                      "keys": {"p256dh": b64(phone.compute_public_key().unwrap().as_ref()), "auth": b64(&auth)}}))
        .send()
        .await
        .unwrap();
    assert_eq!(subscribed.status(), StatusCode::NO_CONTENT);

    let sent = post_test(&owner, "same-origin").await.unwrap();
    assert_eq!(location(&sent), "/approvals?test=sent");
    for _ in 0..100 {
        if !seen.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let pushes = seen.lock().unwrap().clone();
    assert_eq!(
        pushes.len(),
        1,
        "the test goes to the person who asked, once"
    );
    let payload: Value =
        serde_json::from_slice(&crate::push::decrypt(&phone, &auth, &pushes[0])).unwrap();
    assert_eq!(payload["title"], "Approval needed");
    assert_eq!(payload["body"], "Test: tap Approve");

    // The owner's AI asks for something real with the owner's own account.
    board
        .request_approval(
            &Actor::agent(OWNER),
            "Restart nginx",
            "systemctl restart nginx",
            "",
            "",
        )
        .unwrap();
    let html = w
        .get("/approvals?test=sent", Some(&owner))
        .await
        .text()
        .await
        .unwrap();
    assert!(html.contains("Test sent."), "{html}");
    assert!(
        html.contains("<h2>Test: tap Approve</h2><p class=\"meta\">Devagotchi asks"),
        "{html}"
    );
    assert!(
        html.contains("<h2>Restart nginx</h2><p class=\"meta\">Your AI asks"),
        "{html}"
    );
}

#[tokio::test]
async fn a_person_adds_a_machine_of_their_own_and_revokes_it() {
    let w = world().await;
    let other = w.sign_in("tok-other").await;
    let owner = w.sign_in("tok-owner").await;
    let post = |path: &'static str, cookie: &str, site: &'static str, name: &'static str| {
        w.http
            .post(format!("{}{path}", w.url))
            .header(header::COOKIE, cookie.to_string())
            .header("sec-fetch-site", site)
            .form(&[("name", name)])
    };
    let refused = post("/machines/add", &other, "cross-site", "platinum")
        .send()
        .await
        .unwrap();
    assert_eq!(
        refused.status(),
        StatusCode::FORBIDDEN,
        "another site cannot mint codes"
    );

    let page = post("/machines/add", &other, "same-origin", "Platinum")
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    assert_eq!(page.headers()[header::CACHE_CONTROL], "no-store");
    let body = page.text().await.unwrap();
    let code = body
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .find(|word| word.starts_with("dvg-") && word.len() == 36)
        .expect("the code is on the page")
        .to_string();
    assert!(body.contains("Add platinum") && body.contains("--device platinum"));
    // A configured broker's name and an impossible name get no code.
    for name in ["gus", "no spaces"] {
        let taken = post("/machines/add", &other, "same-origin", name)
            .send()
            .await
            .unwrap();
        assert_eq!(taken.status(), StatusCode::CONFLICT, "{name}");
    }

    // The machine enrolls with the code (the link's own tests cover the handshake).
    let devices = Devices::open(&w.dir.path().join("devices.sqlite3")).unwrap();
    devices
        .enroll("platinum", &code, "platinum-key", false)
        .unwrap();
    let mine: Value = w
        .get("/api/v1/me", Some(&other))
        .await
        .json()
        .await
        .unwrap();
    assert!(
        mine["machines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["name"] == "platinum" && m["owned"] == true && m["status"] == "offline"),
        "{mine}"
    );
    // It is that account's alone: not even the hub's owner sees or reaches it.
    let theirs: Value = w
        .get("/api/v1/me", Some(&owner))
        .await
        .json()
        .await
        .unwrap();
    assert!(!theirs.to_string().contains("platinum"), "{theirs}");
    let subject = |id: &str| crate::auth::Subject {
        id: id.into(),
        via: "dev",
        name: None,
        org: None,
    };
    assert!(matches!(
        w.hub.route(&subject(OTHER), "platinum"),
        Ok(crate::hub::Route::Link(name)) if name == "platinum"
    ));
    assert_eq!(
        w.hub
            .route(&subject(OWNER), "platinum")
            .err()
            .unwrap()
            .0
            .code,
        "unknown_broker"
    );

    // Only its owner revokes it, and only from this hub's page.
    let not_theirs = post("/machines/revoke", &owner, "same-origin", "platinum")
        .send()
        .await
        .unwrap();
    assert_eq!(not_theirs.status(), StatusCode::CONFLICT);
    let cross = post("/machines/revoke", &other, "cross-site", "platinum")
        .send()
        .await
        .unwrap();
    assert_eq!(cross.status(), StatusCode::FORBIDDEN);
    let revoked = post("/machines/revoke", &other, "same-origin", "platinum")
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::SEE_OTHER);
    assert!(devices.is_revoked("platinum").unwrap());
    let html = w.get("/account", Some(&other)).await.text().await.unwrap();
    assert!(
        html.contains("<tr id=\"platinum\">") && html.contains("Revoked"),
        "{html}"
    );
}

#[tokio::test]
async fn the_one_line_joins_once_and_downloads_only_what_the_hub_vouches_for() {
    use sha2::{Digest, Sha256};
    let dist = tempfile::tempdir().unwrap();
    let mut sums = String::new();
    for (name, bytes) in [
        ("install-devagotchi.sh", b"#!/usr/bin/env bash\n".as_slice()),
        ("install-devagotchi.ps1", b"param()\n".as_slice()),
        ("devagotchi-x86_64-unknown-linux-gnu.tar.xz", b"linux build".as_slice()),
    ] {
        std::fs::write(dist.path().join(name), bytes).unwrap();
        sums.push_str(&format!("{}  {name}\n", hex::encode(Sha256::digest(bytes))));
    }
    std::fs::write(dist.path().join("SHA256SUMS"), sums).unwrap();
    std::fs::write(dist.path().join("VERSION"), "main-test123\n").unwrap();
    std::fs::write(dist.path().join("not-listed.txt"), "secret").unwrap();
    let downloads = Arc::new(crate::downloads::Downloads::open(dist.path()).unwrap());
    let w = world_with(Some(downloads.clone())).await;
    let other = w.sign_in("tok-other").await;
    let page = w
        .http
        .post(format!("{}/machines/add", w.url))
        .header(header::COOKIE, &other)
        .header("sec-fetch-site", "same-origin")
        .form(&[("name", "platinum")])
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let code = page
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .find(|word| word.starts_with("dvg-") && word.len() == 36)
        .unwrap()
        .to_string();
    assert!(page.contains(&format!("curl -fsSL {}/join/{code} | sudo sh", w.url)), "{page}");
    assert!(page.contains(&format!("irm {}/join/{code}.ps1 | iex", w.url)), "{page}");

    // Fetching the script, any number of times, does not spend the code.
    for _ in 0..2 {
        let script = w.get(&format!("/join/{code}"), None).await;
        assert_eq!(script.status(), StatusCode::OK);
        assert_eq!(script.headers()[header::CACHE_CONTROL], "no-store");
        let text = script.text().await.unwrap();
        assert!(text.contains("device='platinum'") && text.contains("version='main-test123'"));
        assert!(text.contains(downloads.sha256("devagotchi-x86_64-unknown-linux-gnu.tar.xz").unwrap()));
    }
    let ps = w.get(&format!("/join/{code}.ps1"), None).await.text().await.unwrap();
    assert!(ps.contains("$device = 'platinum'"), "{ps}");

    // Only what SHA256SUMS lists is served.
    let file = w.get("/dist/devagotchi-x86_64-unknown-linux-gnu.tar.xz", None).await;
    assert_eq!(file.status(), StatusCode::OK);
    assert_eq!(file.bytes().await.unwrap().as_ref(), b"linux build");
    for name in ["not-listed.txt", "SHA256SUMS", "..%2FSHA256SUMS"] {
        assert_eq!(w.get(&format!("/dist/{name}"), None).await.status(), StatusCode::NOT_FOUND, "{name}");
    }

    // Enrolling spends it; the same line then only says so.
    let devices = Devices::open(&w.dir.path().join("devices.sqlite3")).unwrap();
    devices.enroll("platinum", &code, "platinum-key", false).unwrap();
    let spent = w.get(&format!("/join/{code}"), None).await.text().await.unwrap();
    assert!(spent.contains("already used, has expired or is unknown") && !spent.contains("device="), "{spent}");
    let unknown = w.get("/join/dvg-00000000000000000000000000000000", None).await.text().await.unwrap();
    assert!(unknown.contains("exit 1"));
}
