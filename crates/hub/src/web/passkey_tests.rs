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
    world_owned_by(None).await
}

/// A hub whose config names `owner`, as the Texas hub's names its Nomen subject.
async fn world_owned_by(owner: Option<&str>) -> World {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let config = Arc::new(
        HubConfig::parse(
            &{
                let mut value = json!({
                    "listen": "127.0.0.1:1",
                    "public_url": url,
                    "auth": { "mode": "hub" },
                    "brokers": [{ "name": "wi1", "link": true }],
                    "grants": []
                });
                if let Some(owner) = owner {
                    value["owner"] = owner.into();
                }
                value
            }
            .to_string(),
        )
        .unwrap(),
    );
    let accounts = Arc::new(
        Accounts::open(
            crate::store::Store::open(&crate::store::sqlite_url(&dir.path().join("accounts.sqlite3")))
                .await
                .unwrap(),
        )
        .await
        .unwrap(),
    );
    let devices = Arc::new(Devices::open(&dir.path().join("devices.sqlite3")).unwrap());
    let audit = Arc::new(Audit::open(&dir.path().join("audit.sqlite3")).unwrap());
    let links = Arc::new(Links::new(config.clone(), devices.clone(), audit.clone()));
    let push = Arc::new(crate::push::Push::open(dir.path(), &url, devices).unwrap());
    let state = AppState {
        verifier: Arc::new(
            Verifier::from_hub_config(&config, dir.path(), Some(accounts.clone()))
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
    let owner = world.accounts.ensure_owner(None, "Jesse", [1; 16]).await.unwrap();
    let link = world
        .accounts
        .issue_link(LinkPurpose::Owner, &owner.id)
        .await
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
    let owner = world.accounts.ensure_owner(None, "Jesse", [1; 16]).await.unwrap();
    let link = world
        .accounts
        .issue_link(LinkPurpose::Owner, &owner.id)
        .await
        .unwrap();
    let mut phone = world.phone();
    world.make_passkey(&phone, &link).await;

    // Another site, no Origin at all, or a cross-site fetch: every ceremony endpoint
    // refuses before anything is made.
    for path in [
        "/passkey/sign-in/options",
        "/passkey/sign-in",
        "/passkey/register/options",
        "/passkey/register",
    ] {
        for (origin, site) in [
            (Some("https://evil.example"), None),
            (None, None),
            (Some(world.url.as_str()), Some("cross-site")),
        ] {
            let mut request = world
                .http
                .post(format!("{}{path}", world.url))
                .json(&json!({ "link": link }));
            if let Some(origin) = origin {
                request = request.header(header::ORIGIN, origin);
            }
            if let Some(site) = site {
                request = request.header("sec-fetch-site", site);
            }
            let status = request.send().await.unwrap().status();
            assert_eq!(status, 403, "{path} {origin:?} {site:?}");
        }
    }

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
    // With it, once. A fresh answer to the same challenge (its counter moved on, so only
    // the challenge being spent can refuse it) is refused as expired.
    assert_eq!(
        world
            .post("/passkey/sign-in", Some(&jar), body.clone())
            .await
            .status(),
        200
    );
    let again = World::assertion(&mut phone, &challenge, Some(&owner.id));
    let replayed = world.post("/passkey/sign-in", Some(&jar), again).await;
    assert_eq!(replayed.status(), 400);
    assert_eq!(replayed.json::<Value>().await.unwrap()["error"], "expired");

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

#[tokio::test]
async fn the_owner_takes_the_account_back_from_a_passkey_it_did_not_make() {
    // Someone used a leaked owner link first. `hub owner --replace` gives the owner a link
    // whose use removes every other passkey of the account and ends its sessions.
    let world = world().await;
    let owner = world.accounts.ensure_owner(None, "Jesse", [1; 16]).await.unwrap();
    let leaked = world
        .accounts
        .issue_link(LinkPurpose::Owner, &owner.id)
        .await
        .unwrap();
    let mut intruder = world.phone();
    let theirs = world.make_passkey(&intruder, &leaked).await;
    let replacing = world
        .accounts
        .issue_replacing_link(LinkPurpose::Owner, &owner.id)
        .await
        .unwrap();
    let mine = world.make_passkey(&world.phone(), &replacing).await;
    assert_eq!(world.accounts.passkeys_of(&owner.id).await.unwrap().len(), 1);
    assert_eq!(world.get("/account", Some(&theirs)).await.status(), 303);
    assert_eq!(world.get("/account", Some(&mine)).await.status(), 200);
    let (challenge, jar, _) = world.options("/passkey/sign-in/options", json!({})).await;
    let body = World::assertion(&mut intruder, &challenge, Some(&owner.id));
    assert_eq!(
        world
            .post("/passkey/sign-in", Some(&jar), body)
            .await
            .status(),
        400
    );
}

#[tokio::test]
async fn using_an_owner_link_ends_the_owners_other_sessions() {
    let world = world().await;
    let owner = world.accounts.ensure_owner(None, "Jesse", [1; 16]).await.unwrap();
    let first = world
        .accounts
        .issue_link(LinkPurpose::Owner, &owner.id)
        .await
        .unwrap();
    let laptop = world.make_passkey(&world.phone(), &first).await;
    let second = world
        .accounts
        .issue_link(LinkPurpose::Owner, &owner.id)
        .await
        .unwrap();
    let phone = world.make_passkey(&world.phone(), &second).await;
    assert_eq!(world.get("/account", Some(&laptop)).await.status(), 303);
    assert_eq!(world.get("/account", Some(&phone)).await.status(), 200);
    // Without --replace, the laptop's passkey stays and signs in again.
    assert_eq!(world.accounts.passkeys_of(&owner.id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn signing_out_takes_the_hubs_own_page() {
    let world = world().await;
    let owner = world.accounts.ensure_owner(None, "Jesse", [1; 16]).await.unwrap();
    let link = world
        .accounts
        .issue_link(LinkPurpose::Owner, &owner.id)
        .await
        .unwrap();
    let session = world.make_passkey(&world.phone(), &link).await;
    let out = world
        .http
        .post(format!("{}/auth/logout", world.url))
        .header(header::ORIGIN, "https://evil.example")
        .header("sec-fetch-site", "cross-site")
        .header(header::COOKIE, &session)
        .send()
        .await
        .unwrap();
    assert_eq!(out.status(), 403);
    assert!(
        cookie(&out, "dg_session").is_none(),
        "the cookie is left alone"
    );
    assert_eq!(world.get("/account", Some(&session)).await.status(), 200);
}

#[tokio::test]
async fn a_link_for_another_owner_than_the_configs_is_refused_before_it_is_spent() {
    // The config names the Texas owner, but the store's owner was made without it.
    let world = world_owned_by(Some("395290028818825218")).await;
    let stray = world.accounts.ensure_owner(None, "Owner", [1; 16]).await.unwrap();
    let link = world
        .accounts
        .issue_link(LinkPurpose::Owner, &stray.id)
        .await
        .unwrap();
    let options = world
        .post("/passkey/register/options", None, json!({ "link": link }))
        .await;
    assert_eq!(options.status(), 409);
    // A registration answer that would otherwise reach the store, with a challenge cookie.
    let (challenge, jar, _) = world.options("/passkey/sign-in/options", json!({})).await;
    let sent = world.phone().register(&challenge);
    let register = world
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
    assert_eq!(register.status(), 409);
    assert_eq!(
        world
            .accounts
            .check_link(LinkPurpose::Owner, &link)
            .await
            .unwrap(),
        stray.id
    );
    assert!(world.accounts.passkeys_of(&stray.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_copied_passkey_ends_the_sessions_it_opened() {
    let world = world().await;
    let owner = world.accounts.ensure_owner(None, "Jesse", [1; 16]).await.unwrap();
    let link = world
        .accounts
        .issue_link(LinkPurpose::Owner, &owner.id)
        .await
        .unwrap();
    let mut phone = Authenticator::for_site(Alg::Es256, "127.0.0.1", &world.url);
    world.make_passkey(&phone, &link).await;
    let (challenge, jar, _) = world.options("/passkey/sign-in/options", json!({})).await;
    let body = World::assertion(&mut phone, &challenge, Some(&owner.id));
    let signed_in = world.post("/passkey/sign-in", Some(&jar), body).await;
    let session = cookie(&signed_in, "dg_session").unwrap();
    // A second copy of the key, its counter behind.
    phone.count = 0;
    let (challenge, jar, _) = world.options("/passkey/sign-in/options", json!({})).await;
    let body = World::assertion(&mut phone, &challenge, Some(&owner.id));
    assert_eq!(
        world
            .post("/passkey/sign-in", Some(&jar), body)
            .await
            .status(),
        400
    );
    assert_eq!(world.get("/account", Some(&session)).await.status(), 303);
}

#[tokio::test]
async fn on_https_the_cookies_are_the_hosts_alone() {
    let dir = tempfile::tempdir().unwrap();
    let config = Arc::new(
        HubConfig::parse(
            &json!({
                "listen": "127.0.0.1:1",
                "public_url": "https://app.devagotchi.com",
                "auth": { "mode": "hub", "rp_id": "devagotchi.com" },
                "brokers": [],
                "grants": []
            })
            .to_string(),
        )
        .unwrap(),
    );
    let accounts = Arc::new(
        Accounts::open(
            crate::store::Store::open(&crate::store::sqlite_url(&dir.path().join("accounts.sqlite3")))
                .await
                .unwrap(),
        )
        .await
        .unwrap(),
    );
    let hub = HubSignIn::new(config, accounts).unwrap();
    assert_eq!(hub.session_cookie(), "__Host-dg_session");
    let set = hub.cookie(hub.session_cookie(), "dgs_x", 60);
    for part in ["Path=/", "HttpOnly", "SameSite=Lax", "Secure", "Max-Age=60"] {
        assert!(set.contains(part), "{set}");
    }
    assert!(!set.to_ascii_lowercase().contains("domain"), "{set}");
}

#[tokio::test]
async fn a_replacing_link_lets_the_device_that_holds_the_old_passkey_make_the_new_one() {
    // The owner's usual phone already holds a passkey for this hub: listing it in
    // excludeCredentials would make the phone refuse to make another.
    let world = world().await;
    let owner = world.accounts.ensure_owner(None, "Jesse", [1; 16]).await.unwrap();
    let link = world
        .accounts
        .issue_link(LinkPurpose::Owner, &owner.id)
        .await
        .unwrap();
    world.make_passkey(&world.phone(), &link).await;
    let plain = world
        .accounts
        .issue_link(LinkPurpose::Owner, &owner.id)
        .await
        .unwrap();
    let (_, _, options) = world
        .options("/passkey/register/options", json!({ "link": plain }))
        .await;
    assert_eq!(options["excludeCredentials"].as_array().unwrap().len(), 1);
    let replacing = world
        .accounts
        .issue_replacing_link(LinkPurpose::Owner, &owner.id)
        .await
        .unwrap();
    let (_, _, options) = world
        .options("/passkey/register/options", json!({ "link": replacing }))
        .await;
    assert_eq!(options["excludeCredentials"], json!([]));
}

#[tokio::test]
async fn an_old_answer_replayed_signs_nobody_out() {
    // A captured sign-in from a key that keeps a counter, replayed after the counter has
    // moved on: refused, and the sessions opened since stay.
    let world = world().await;
    let owner = world.accounts.ensure_owner(None, "Jesse", [1; 16]).await.unwrap();
    let link = world
        .accounts
        .issue_link(LinkPurpose::Owner, &owner.id)
        .await
        .unwrap();
    let mut phone = world.phone();
    world.make_passkey(&phone, &link).await;
    let (challenge, first_jar, _) = world.options("/passkey/sign-in/options", json!({})).await;
    let first = World::assertion(&mut phone, &challenge, Some(&owner.id));
    assert_eq!(
        world
            .post("/passkey/sign-in", Some(&first_jar), first.clone())
            .await
            .status(),
        200
    );
    let (challenge, jar, _) = world.options("/passkey/sign-in/options", json!({})).await;
    let second = World::assertion(&mut phone, &challenge, Some(&owner.id));
    let signed_in = world.post("/passkey/sign-in", Some(&jar), second).await;
    let session = cookie(&signed_in, "dg_session").unwrap();
    let replayed = world
        .post("/passkey/sign-in", Some(&first_jar), first)
        .await;
    assert_eq!(replayed.status(), 400);
    assert_eq!(world.get("/account", Some(&session)).await.status(), 200);
}

#[tokio::test]
async fn ceremony_bodies_are_json_only() {
    let world = world().await;
    let plain = world
        .http
        .post(format!("{}/passkey/sign-in/options", world.url))
        .header(header::ORIGIN, &world.url)
        .header(header::CONTENT_TYPE, "text/plain")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(plain.status(), 415);
    assert!(cookie(&plain, "dg_challenge").is_none());
}

/// `key`'s value in a URL's query, decoded.
fn query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| {
            percent_encoding::percent_decode_str(v)
                .decode_utf8_lossy()
                .into_owned()
        })
    })
}

#[tokio::test]
async fn an_ai_client_registers_the_owner_approves_it_and_the_hub_checks_its_tokens() {
    // What Claude or ChatGPT does against `<hub>/mcp`, with nothing but the hub deployed.
    let world = world().await;
    let owner = world.accounts.ensure_owner(None, "Jesse", [1; 16]).await.unwrap();
    let link = world
        .accounts
        .issue_link(LinkPurpose::Owner, &owner.id)
        .await
        .unwrap();
    let session = world.make_passkey(&world.phone(), &link).await;

    // Discovery: the hub is its own authorization server.
    let metadata: Value = world
        .get("/.well-known/oauth-authorization-server", None)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(metadata["issuer"], world.url.as_str());
    assert_eq!(metadata["code_challenge_methods_supported"], json!(["S256"]));
    assert_eq!(
        metadata["token_endpoint"],
        format!("{}/oauth/token", world.url)
    );

    // Registration (RFC 7591).
    let redirect = "http://127.0.0.1:9/callback";
    let registered = world
        .http
        .post(format!("{}/oauth/register", world.url))
        .json(&json!({ "client_name": "Test AI", "redirect_uris": [redirect] }))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), 201);
    let client: Value = registered.json().await.unwrap();
    let client_id = client["client_id"].as_str().unwrap().to_string();
    let refused = world
        .http
        .post(format!("{}/oauth/register", world.url))
        .json(&json!({ "redirect_uris": ["http://evil.example/cb"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 400);

    // The authorization request; not signed in, it goes to sign-in first and back.
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let authorize = format!(
        "/oauth/authorize?response_type=code&client_id={client_id}&redirect_uri={}&code_challenge={}&code_challenge_method=S256&state=st+1%2F2&scope=mcp+offline_access&resource={}",
        super::encode(redirect),
        crate::accounts::oauth::s256(verifier),
        super::encode(&format!("{}/mcp", world.url)),
    );
    let signed_out = world.get(&authorize, None).await;
    assert_eq!(signed_out.status(), 303);
    let to_login = signed_out.headers()[header::LOCATION].to_str().unwrap().to_string();
    assert!(to_login.starts_with("/auth/login?return_to="), "{to_login}");
    let back = query_param(&to_login, "return_to").unwrap();
    assert_eq!(super::safe_return_to(&back), back, "sign-in may return to it");
    assert_eq!(query_param(&back, "client_id").as_deref(), Some(client_id.as_str()));
    assert_eq!(query_param(&back, "state").as_deref(), Some("st 1/2"));

    // Signed in: the approval page, whose form may lead to the client and nowhere else.
    let page = world.get(&back, Some(&session)).await;
    assert_eq!(page.status(), 200);
    let csp = page.headers()[header::CONTENT_SECURITY_POLICY]
        .to_str()
        .unwrap()
        .to_string();
    assert!(csp.contains("form-action 'self' http://127.0.0.1:9;"), "{csp}");
    let html = page.text().await.unwrap();
    assert!(html.contains("Connect Test AI?") && html.contains("Jesse"), "{html}");

    let approve = |decision: &'static str, origin: Option<String>| {
        let mut request = world
            .http
            .post(format!("{}/oauth/authorize", world.url))
            .header(header::COOKIE, &session)
            .form(&[
                ("client_id", client_id.as_str()),
                ("redirect_uri", redirect),
                ("code_challenge", &crate::accounts::oauth::s256(verifier)),
                ("resource", &format!("{}/mcp", world.url)),
                ("scope", "mcp offline_access"),
                ("state", "st 1/2"),
                ("decision", decision),
            ]);
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        request.send()
    };
    // Another site, or no Origin at all: not approved.
    assert_eq!(approve("approve", None).await.unwrap().status(), 403);
    assert_eq!(
        approve("approve", Some("https://evil.example".into()))
            .await
            .unwrap()
            .status(),
        403
    );
    // Cancel: back to the client with access_denied.
    let denied = approve("deny", Some(world.url.clone())).await.unwrap();
    let to = denied.headers()[header::LOCATION].to_str().unwrap().to_string();
    assert_eq!(query_param(&to, "error").as_deref(), Some("access_denied"));
    // Connect: back with a code, the state and the issuer (RFC 9207).
    let approved = approve("approve", Some(world.url.clone())).await.unwrap();
    assert_eq!(approved.status(), 303);
    let to = approved.headers()[header::LOCATION].to_str().unwrap().to_string();
    assert!(to.starts_with(redirect), "{to}");
    assert_eq!(query_param(&to, "state").as_deref(), Some("st 1/2"));
    assert_eq!(query_param(&to, "iss").as_deref(), Some(world.url.as_str()));
    let code = query_param(&to, "code").unwrap();

    // The token request.
    let token = |form: Vec<(&'static str, String)>| {
        world
            .http
            .post(format!("{}/oauth/token", world.url))
            .form(&form)
            .send()
    };
    let wrong = token(vec![
        ("grant_type", "authorization_code".into()),
        ("code", code.clone()),
        ("redirect_uri", redirect.into()),
        ("client_id", client_id.clone()),
        ("code_verifier", "x".repeat(43)),
    ])
    .await
    .unwrap();
    assert_eq!(wrong.status(), 400);
    let error: Value = wrong.json().await.unwrap();
    assert_eq!(error["error"], "invalid_grant");
    // That code was spent by the wrong try: a new approval for a new one.
    let approved = approve("approve", Some(world.url.clone())).await.unwrap();
    let to = approved.headers()[header::LOCATION].to_str().unwrap().to_string();
    let code = query_param(&to, "code").unwrap();
    let granted = token(vec![
        ("grant_type", "authorization_code".into()),
        ("code", code),
        ("redirect_uri", redirect.into()),
        ("client_id", client_id.clone()),
        ("code_verifier", verifier.into()),
        ("resource", format!("{}/mcp", world.url)),
    ])
    .await
    .unwrap();
    assert_eq!(granted.status(), 200);
    assert_eq!(granted.headers()[header::CACHE_CONTROL], "no-store");
    let tokens: Value = granted.json().await.unwrap();
    assert_eq!(tokens["token_type"], "Bearer");
    assert_eq!(tokens["scope"], "mcp offline_access");
    let access = tokens["access_token"].as_str().unwrap().to_string();
    let refresh = tokens["refresh_token"].as_str().unwrap().to_string();

    // `/mcp` takes it: the account, as an AI the hub connected, never as the person.
    let verifier_for_mcp =
        Verifier::from_hub_config(&world.config, world._dir.path(), Some(world.accounts.clone()))
            .await
            .unwrap();
    let subject = verifier_for_mcp.verify(&access).await.unwrap();
    assert_eq!((subject.id.as_str(), subject.via), (owner.id.as_str(), "hub"));
    assert_eq!(verifier_for_mcp.issuer(), Some(world.url.as_str()));
    assert!(verifier_for_mcp.verify("dgh_forged").await.is_err());
    assert!(verifier_for_mcp.verify(&refresh).await.is_err());

    // Refresh rotates; revoking ends the family.
    let rotated: Value = token(vec![
        ("grant_type", "refresh_token".into()),
        ("refresh_token", refresh),
        ("client_id", client_id.clone()),
    ])
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let access2 = rotated["access_token"].as_str().unwrap().to_string();
    assert!(verifier_for_mcp.verify(&access).await.is_err(), "rotated out");
    assert!(verifier_for_mcp.verify(&access2).await.is_ok());
    let revoked = world
        .http
        .post(format!("{}/oauth/revoke", world.url))
        .form(&[("token", access2.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), 200);
    assert!(verifier_for_mcp.verify(&access2).await.is_err());
}

#[tokio::test]
async fn an_authorization_request_for_an_unknown_client_or_redirect_goes_nowhere() {
    // Redirecting an error to an address nobody registered would hand it to a stranger.
    let world = world().await;
    let owner = world.accounts.ensure_owner(None, "Jesse", [1; 16]).await.unwrap();
    let link = world
        .accounts
        .issue_link(LinkPurpose::Owner, &owner.id)
        .await
        .unwrap();
    let session = world.make_passkey(&world.phone(), &link).await;
    let client = world
        .accounts
        .register_client("Test AI", &["https://ai.example.net/cb".to_string()])
        .await
        .unwrap();
    let challenge = crate::accounts::oauth::s256(&"v".repeat(43));
    for (client_id, redirect) in [
        ("dgc_nobody", "https://ai.example.net/cb"),
        (client.id.as_str(), "https://evil.example/cb"),
    ] {
        let response = world
            .get(
                &format!(
                    "/oauth/authorize?response_type=code&client_id={client_id}&redirect_uri={}&code_challenge={challenge}&code_challenge_method=S256",
                    super::encode(redirect)
                ),
                Some(&session),
            )
            .await;
        assert_eq!(response.status(), 400, "{client_id} {redirect}");
        assert!(response.headers().get(header::LOCATION).is_none());
    }
    // A known client without PKCE: the error goes back to it.
    let response = world
        .get(
            &format!(
                "/oauth/authorize?response_type=code&client_id={}&redirect_uri={}&state=s",
                client.id,
                super::encode("https://ai.example.net/cb")
            ),
            Some(&session),
        )
        .await;
    assert_eq!(response.status(), 303);
    let to = response.headers()[header::LOCATION].to_str().unwrap();
    assert_eq!(query_param(to, "error").as_deref(), Some("invalid_request"));
    assert_eq!(query_param(to, "state").as_deref(), Some("s"));
    // A resource that is not this hub's.
    let response = world
        .get(
            &format!(
                "/oauth/authorize?response_type=code&client_id={}&redirect_uri={}&code_challenge={challenge}&code_challenge_method=S256&resource={}",
                client.id,
                super::encode("https://ai.example.net/cb"),
                super::encode("https://other.example/mcp")
            ),
            Some(&session),
        )
        .await;
    let to = response.headers()[header::LOCATION].to_str().unwrap();
    assert_eq!(query_param(to, "error").as_deref(), Some("invalid_target"));
}
