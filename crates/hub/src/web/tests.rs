//! The browser sign-in end to end against a fake Nomen token endpoint: the hub's real
//! routes, cookies and redirects, with the authorization step played by the test.
//! (`tests/web_probe.py` runs the same flow against the real binary with Nomen introspection.)

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU16, Ordering::SeqCst},
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
    axum::Json(json!({"access_token": grant.token, "token_type": "Bearer", "expires_in": grant.expires_in, "id_token": "unused"}))
        .into_response()
}

struct World {
    fake: Arc<FakeNomen>,
    url: String,
    http: reqwest::Client,
    _dir: tempfile::TempDir,
}

async fn world() -> World {
    let nomen = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fake = Arc::new(FakeNomen {
        issuer: format!("http://{}", nomen.local_addr().unwrap()),
        redirect_uri: Mutex::new(String::new()),
        codes: Mutex::new(HashMap::new()),
        status: AtomicU16::new(200),
    });
    let app = axum::Router::new()
        .route("/oauth/v2/token", post(token))
        .with_state(fake.clone());
    tokio::spawn(async move { axum::serve(nomen, app).await.unwrap() });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    *fake.redirect_uri.lock().unwrap() = format!("{url}/auth/callback");
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
                "grants": [{"subject": OTHER, "brokers": ["gus"]}]
            })
            .to_string(),
        )
        .unwrap(),
    );
    let dir = tempfile::tempdir().unwrap();
    let devices = Arc::new(Devices::open(&dir.path().join("devices.sqlite3")).unwrap());
    for name in ["gus", "old"] {
        let code = devices
            .issue_code(name, crate::link::random::<16>())
            .unwrap();
        devices.enroll(name, &code, "test-key").unwrap();
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
    let web = WebLogin::new(&fake.issuer, &["hub-project".to_string()], WEB_CLIENT, &url).unwrap();
    let state = AppState {
        verifier: Arc::new(verifier),
        hub: Hub {
            boards: Arc::new(crate::board::mcp::Boards::from_config(&config)),
            config,
            audit,
            links,
        },
        web: Some(Arc::new(web)),
    };
    let app = router().with_state(state);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    World {
        fake,
        url,
        http,
        _dir: dir,
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
            "openid profile urn:zitadel:iam:org:project:id:hub-project:aud"
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
    let public = WebLogin::new(
        "https://id.example",
        &["p".into()],
        "web",
        "https://hub.example",
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
