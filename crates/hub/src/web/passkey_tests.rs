//! A hub with its own accounts end to end over HTTP: the owner's link makes a passkey,
//! the passkey signs in, and the pages, the session and `/api/v1/me` follow. The browser
//! is played by the test with the software authenticator (`accounts/passkey/soft.rs`).

use std::sync::Arc;

use axum::http::header;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};

use super::*;
use crate::{
    AppState,
    accounts::{
        Accounts, LinkPurpose,
        passkey::{Alg, soft::Authenticator},
    },
    audit::Audit,
    auth::Verifier,
    config::HubConfig,
    devices::Devices,
    hub::Hub,
    link::Links,
};

struct World {
    url: String,
    http: reqwest::Client,
    accounts: Arc<Accounts>,
    config: Arc<HubConfig>,
    _dir: tempfile::TempDir,
}

async fn world() -> World {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let config = Arc::new(
        HubConfig::parse(
            &json!({
                "listen": "127.0.0.1:1",
                "public_url": url,
                "auth": { "mode": "hub" },
                "brokers": [{ "name": "wi1", "link": true }],
                "grants": []
            })
            .to_string(),
        )
        .unwrap(),
    );
    let accounts = Arc::new(Accounts::open(&dir.path().join("accounts.sqlite3")).unwrap());
    let devices = Arc::new(Devices::open(&dir.path().join("devices.sqlite3")).unwrap());
    let audit = Arc::new(Audit::open(&dir.path().join("audit.sqlite3")).unwrap());
    let links = Arc::new(Links::new(config.clone(), devices.clone(), audit.clone()));
    let push = Arc::new(crate::push::Push::open(dir.path(), &url, devices).unwrap());
    let state = AppState {
        verifier: Arc::new(
            Verifier::from_hub_config(&config, dir.path())
                .await
                .unwrap(),
        ),
        hub: Hub {
            downloads: None,
            push: Some(push),
            boards: Arc::new(crate::board::mcp::Boards::from_config(&config, dir.path())),
            config: config.clone(),
            audit,
            links,
        },
        web: None,
        passkeys: HubSignIn::new(config.clone(), accounts.clone()).map(Arc::new),
    };
    assert!(state.passkeys.is_some());
    let app = router()
        .layer(axum::middleware::from_fn_with_state(state.clone(), bar))
        .with_state(state);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    World {
        url,
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        accounts,
        config,
        _dir: dir,
    }
}

/// `name=value` from the response's Set-Cookie for `name`.
fn cookie(response: &reqwest::Response, name: &str) -> Option<String> {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with(&format!("{name}=")))
        .map(|v| v.split(';').next().unwrap().to_string())
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

impl World {
    fn phone(&self) -> Authenticator {
        Authenticator::for_site(Alg::Es256, "127.0.0.1", &self.url)
    }

    async fn get(&self, path: &str, cookie: Option<&str>) -> reqwest::Response {
        let mut request = self.http.get(format!("{}{path}", self.url));
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        request.send().await.unwrap()
    }

    /// A POST as the hub's own page makes it: same origin, JSON.
    async fn post(&self, path: &str, cookie: Option<&str>, body: Value) -> reqwest::Response {
        let mut request = self
            .http
            .post(format!("{}{path}", self.url))
            .header(header::ORIGIN, &self.url)
            .json(&body);
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        request.send().await.unwrap()
    }

    /// Ask for a ceremony's options: its challenge, and the cookie that carries it.
    async fn options(&self, path: &str, body: Value) -> (Vec<u8>, String, Value) {
        let response = self.post(path, None, body).await;
        assert_eq!(response.status(), 200, "{path}");
        let jar = cookie(&response, "dg_challenge").expect("the challenge cookie");
        let options: Value = response.json().await.unwrap();
        let challenge = URL_SAFE_NO_PAD
            .decode(options["challenge"].as_str().unwrap())
            .unwrap();
        (challenge, jar, options)
    }

    /// The owner's link, used: returns the session cookie.
    async fn make_passkey(&self, phone: &Authenticator, link: &str) -> String {
        let (challenge, jar, options) = self
            .options("/passkey/register/options", json!({ "link": link }))
            .await;
        assert_eq!(options["rp"]["id"], "127.0.0.1");
        assert_eq!(
            options["authenticatorSelection"]["userVerification"],
            "required"
        );
        let sent = phone.register(&challenge);
        let response = self
            .post(
                "/passkey/register",
                Some(&jar),
                json!({
                    "link": link,
                    "id": b64(&sent.credential_id),
                    "clientDataJSON": b64(&sent.client_data_json),
                    "authenticatorData": b64(&sent.authenticator_data),
                    "publicKey": b64(&sent.public_key),
                    "publicKeyAlgorithm": sent.alg,
                }),
            )
            .await;
        assert_eq!(response.status(), 200);
        let session = cookie(&response, "dg_session").expect("a session");
        let done: Value = response.json().await.unwrap();
        assert_eq!(done["next"], "/account");
        session
    }

    /// A sign-in body for `phone`, answering `challenge`.
    fn assertion(phone: &mut Authenticator, challenge: &[u8], handle: Option<&str>) -> Value {
        let sent = phone.assert(challenge);
        json!({
            "id": b64(&sent.credential_id),
            "clientDataJSON": b64(&sent.client_data_json),
            "authenticatorData": b64(&sent.authenticator_data),
            "signature": b64(&sent.signature),
            "userHandle": handle.map(|h| b64(h.as_bytes())),
            "returnTo": "/approvals",
        })
    }
}

#[tokio::test]
async fn the_owner_makes_a_passkey_from_the_link_and_signs_in_with_it() {
    let world = world().await;
    let owner = world.accounts.ensure_owner(None, "Jesse", [1; 16]).unwrap();
    let link = world
        .accounts
        .issue_link(LinkPurpose::Owner, &owner.id)
        .unwrap();
    assert!(
        !world.config.is_owner(&owner.id),
        "not known until the link is used"
    );

    // The link is `/owner#<token>`: the page the browser asks for carries no token.
    let page = world.get("/owner", None).await;
    assert_eq!(page.status(), 200);
    let csp = page.headers()[header::CONTENT_SECURITY_POLICY]
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        csp.contains("script-src 'self'") && !csp.contains("unsafe"),
        "{csp}"
    );
    let html = page.text().await.unwrap();
    assert!(html.contains("/assets/passkey.js") && html.contains("passkey-register"));
    let (_, _, options) = world
        .options("/passkey/register/options", json!({ "link": link }))
        .await;
    assert_eq!(options["user"]["displayName"], "Jesse");
    assert_eq!(options["user"]["id"], b64(owner.id.as_bytes()));

    let mut phone = world.phone();
    let session = world.make_passkey(&phone, &link).await;
    assert!(
        world.config.is_owner(&owner.id),
        "the hub learns its owner at once"
    );
    let account = world.get("/account", Some(&session)).await;
    assert_eq!(account.status(), 200);
    let html = account.text().await.unwrap();
    assert!(
        html.contains("Signed in as Jesse") && html.contains("You own this hub"),
        "{html}"
    );
    assert!(
        !html.contains("Shippin"),
        "a hub account is not a Shippin account"
    );
    // The link worked once.
    let spent = world
        .post("/passkey/register/options", None, json!({ "link": link }))
        .await;
    assert_eq!(spent.status(), 400);

    // Sign out: the session ends on the hub, not just in the browser.
    let out = world.post("/auth/logout", Some(&session), json!({})).await;
    assert_eq!(out.status(), 303);
    let again = world.get("/account", Some(&session)).await;
    assert_eq!(again.status(), 303);

    // Sign in with the passkey.
    let login = world.get("/auth/login?return_to=/approvals", None).await;
    assert_eq!(login.status(), 200);
    assert!(login.text().await.unwrap().contains("passkey-sign-in"));
    let (challenge, jar, options) = world.options("/passkey/sign-in/options", json!({})).await;
    assert_eq!(options["rpId"], "127.0.0.1");
    assert_eq!(options["allowCredentials"], json!([]));
    let body = World::assertion(&mut phone, &challenge, Some(&owner.id));
    let response = world.post("/passkey/sign-in", Some(&jar), body).await;
    assert_eq!(response.status(), 200);
    let session = cookie(&response, "dg_session").unwrap();
    assert_eq!(
        response.json::<Value>().await.unwrap()["next"],
        "/approvals"
    );
    let me: Value = world
        .get("/api/v1/me", Some(&session))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        (
            me["subject"].as_str(),
            me["via"].as_str(),
            me["owner"].as_bool()
        ),
        (Some(owner.id.as_str()), Some("passkey"), Some(true))
    );
}

#[tokio::test]
async fn ceremonies_answer_only_the_hubs_own_pages_and_work_once() {
    let world = world().await;
    let owner = world.accounts.ensure_owner(None, "Jesse", [1; 16]).unwrap();
    let link = world
        .accounts
        .issue_link(LinkPurpose::Owner, &owner.id)
        .unwrap();
    let mut phone = world.phone();
    world.make_passkey(&phone, &link).await;

    // Another site, or no Origin at all: refused before anything is made.
    for origin in [Some("https://evil.example"), None] {
        let mut request = world
            .http
            .post(format!("{}/passkey/sign-in/options", world.url))
            .json(&json!({}));
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        assert_eq!(request.send().await.unwrap().status(), 403, "{origin:?}");
    }
    let fetched = world
        .http
        .post(format!("{}/passkey/sign-in/options", world.url))
        .header(header::ORIGIN, &world.url)
        .header("sec-fetch-site", "cross-site")
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(fetched.status(), 403);

    // No challenge cookie: the answer is not for this browser.
    let (challenge, jar, _) = world.options("/passkey/sign-in/options", json!({})).await;
    let body = World::assertion(&mut phone, &challenge, Some(&owner.id));
    assert_eq!(
        world
            .post("/passkey/sign-in", None, body.clone())
            .await
            .status(),
        400
    );
    // With it, once; the same request again is refused.
    assert_eq!(
        world
            .post("/passkey/sign-in", Some(&jar), body.clone())
            .await
            .status(),
        200
    );
    assert_eq!(
        world
            .post("/passkey/sign-in", Some(&jar), body)
            .await
            .status(),
        400
    );

    // Login CSRF: another site's own valid sign-in, sent with this browser's challenge.
    let (_, victim_jar, _) = world.options("/passkey/sign-in/options", json!({})).await;
    let (attacker_challenge, _, _) = world.options("/passkey/sign-in/options", json!({})).await;
    let forged = World::assertion(&mut phone, &attacker_challenge, Some(&owner.id));
    let response = world
        .post("/passkey/sign-in", Some(&victim_jar), forged)
        .await;
    assert_eq!(response.status(), 400);
    assert!(cookie(&response, "dg_session").is_none());

    // A passkey this hub never registered.
    let (challenge, jar, _) = world.options("/passkey/sign-in/options", json!({})).await;
    let mut stranger = world.phone();
    let body = World::assertion(&mut stranger, &challenge, Some(&owner.id));
    assert_eq!(
        world
            .post("/passkey/sign-in", Some(&jar), body)
            .await
            .status(),
        400
    );

    // An answer without the user handle a discoverable passkey always returns.
    let (challenge, jar, _) = world.options("/passkey/sign-in/options", json!({})).await;
    let body = World::assertion(&mut phone, &challenge, None);
    let response = world.post("/passkey/sign-in", Some(&jar), body).await;
    assert_eq!(response.status(), 400);
    assert!(cookie(&response, "dg_session").is_none());
}

#[tokio::test]
async fn a_hub_alone_signs_people_in_with_no_identity_provider() {
    let world = world().await;
    let home = world.get("/", None).await;
    assert_eq!(home.status(), 200);
    let html = home.text().await.unwrap();
    assert!(
        html.contains(">Sign in<") && !html.contains("Shippin"),
        "{html}"
    );
    // Signed out, pages send you to the hub's own sign-in, never to another service.
    let account = world.get("/account", None).await;
    assert_eq!(account.status(), 303);
    assert_eq!(
        account.headers()[header::LOCATION],
        "/auth/login?return_to=/account"
    );
    let me = world.get("/api/v1/me", None).await;
    assert_eq!(me.status(), 401);
    let bearer = world
        .http
        .get(format!("{}/api/v1/me", world.url))
        .bearer_auth("not-a-token")
        .send()
        .await
        .unwrap();
    assert_eq!(
        bearer.status(),
        401,
        "no bearer tokens until the hub issues its own"
    );
    // A link nobody issued.
    let made_up = world
        .post(
            "/passkey/register/options",
            None,
            json!({ "link": "dgl_nothing" }),
        )
        .await;
    assert_eq!(made_up.status(), 400);
}
