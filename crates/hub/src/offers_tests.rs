//! The site's offers end to end over HTTP on a hosted hub, the whole router as it serves
//! (`offers.rs`): on by default in the built-in site, offered once with the site's question,
//! and a yes kept in the person's own words, with the time, on the tenant's chain. Linking
//! with no click against a fake Nomen user API (the user found, made, or found unverified,
//! and Nomen down), and the one-tap link when the hub cannot: it signs in with the network
//! and the AI's `offers` then says linked. Off (or with no offerings), and on the neutral
//! site, nothing of it shows: no tool, no section, no word, no page. On SQLite, and on
//! Postgres when the test server is set (CI).

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use axum::{Json, extract::State, http::header, routing::post};
use serde_json::{Value, json};

use crate::{
    accounts::identities::Proven,
    store,
    tenant_tests::{Hosted, Person, hosted_with, refused, set_cookie},
};

async fn stores() -> Vec<Option<String>> {
    let mut out = vec![None];
    if let Some(url) = store::testing::empty_postgres().await {
        out.push(Some(url));
    }
    out
}

async fn structured(hub: &Hosted, person: &mut Person, tool: &str, arguments: Value) -> Value {
    let answer = hub.call(person, tool, arguments).await;
    assert_ne!(answer["isError"], true, "{tool}: {answer}");
    answer["structuredContent"].clone()
}

async fn who(hub: &Hosted, person: &mut Person) -> (String, String) {
    let who = structured(hub, person, "whoami", json!({})).await;
    (
        who["subject"].as_str().unwrap().to_string(),
        who["tenant"].as_str().unwrap().to_string(),
    )
}

async fn tool_names(hub: &Hosted, person: &mut Person) -> Vec<String> {
    hub.mcp(person, "tools/list", json!({})).await["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect()
}

/// What every AI client is told when it connects.
fn instructions(hub: &Hosted) -> String {
    rmcp::ServerHandler::get_info(&hub.hub)
        .instructions
        .unwrap_or_default()
}

/// The tenant's chain, oldest first, as `(event, detail)`.
async fn chain(hub: &Hosted, tenant: &str) -> Vec<(String, String)> {
    let mut events = hub.accounts.events(tenant, 200).await.unwrap();
    events.reverse();
    events.into_iter().map(|e| (e.event, e.detail)).collect()
}

/// A secret file the hub reads (0600).
fn private(dir: &std::path::Path, name: &str, text: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    path.to_string_lossy().to_string()
}

/// Sign in with Shippin on the hub: the `nomen` provider (never reached here: the tests
/// stand in for its sign-in by linking what it would prove).
fn nomen_sign_in(dir: &std::path::Path) -> Value {
    json!({ "providers": [{
        "kind": "oidc", "name": "nomen", "label": "Shippin",
        "issuer": "https://id.example.org", "client_id": "hub-app",
        "client_secret_file": private(dir, "nomen-secret", "s3cret\n"),
    }] })
}

fn decode(text: &str) -> String {
    percent_encoding::percent_decode_str(text)
        .decode_utf8()
        .unwrap()
        .to_string()
}

/// The link's path on the hub.
fn path_of(link: &str, hub: &Hosted) -> String {
    let path = link.strip_prefix(&hub.url).expect("a link on this hub").to_string();
    assert!(path.starts_with("/offers/dgo_"), "{path}");
    path
}

#[tokio::test]
async fn on_by_default_offered_once_and_a_yes_is_kept_in_the_users_own_words() {
    for url in stores().await {
        let hub = hosted_with(url.as_deref(), |_, _| json!({})).await;
        let mut person = hub.first_connect("Claude").await;
        let (subject, tenant) = who(&hub, &mut person).await;
        // The tools are there, and the instructions say when to offer and how.
        let names = tool_names(&hub, &mut person).await;
        assert!(names.iter().any(|n| n == "offers") && names.iter().any(|n| n == "offers_accept"));
        let said = instructions(&hub);
        assert!(said.contains("The Shippin Network: after the user's first machine works"), "{said}");
        assert!(said.contains("Never ask twice and never push"), "{said}");
        // The offer: the site's question, as it is, and its offerings.
        let offer = structured(&hub, &mut person, "offers", json!({})).await;
        assert_eq!(offer["state"], "not_asked");
        assert_eq!(offer["network"], "Shippin Network");
        let site = crate::site::builtin();
        let offers = site.info.offers().unwrap();
        assert_eq!(offer["question"], offers.question.as_str());
        // Only what is offered: an offering the site left blank (no address) is not.
        let offered: Vec<&str> = offers.offered().map(|item| item.name.as_str()).collect();
        assert!(offered.len() < offers.items.len(), "the built-in site has blank offerings");
        let names: Vec<&str> = offer["offerings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, offered);
        let vaultix = offer["offerings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["name"] == "Vaultix")
            .unwrap();
        assert_eq!(vaultix["url"], "https://vaultix.cloud");
        // A yes is kept exactly as said, whatever it says, with the time.
        let words = "Yes! Sure — link it 🙂\nthanks";
        let before = crate::devices::now_secs();
        let accepted = structured(&hub, &mut person, "offers_accept", json!({ "consent": words })).await;
        // No provisioning and no Shippin sign-in on this hub: kept, and said plainly.
        assert_eq!(accepted["state"], "unavailable", "{accepted}");
        assert!(accepted["message"].as_str().unwrap().contains("yes is kept"), "{accepted}");
        let row = hub.accounts.offer(&tenant).await.unwrap().unwrap();
        assert_eq!(row.consent, words);
        assert_eq!(row.account, subject);
        assert!(row.accepted_at >= before);
        assert_eq!(row.offerings, offered);
        assert_eq!(row.state, "pending");
        let events = hub.accounts.events(&tenant, 50).await.unwrap();
        let yes = events.iter().find(|e| e.event == "offers.accepted").unwrap();
        assert_eq!((yes.detail.as_str(), yes.actor.as_str()), (words, subject.as_str()));
        assert!(yes.at >= before);
        assert!(hub.accounts.chain_intact(&tenant).await.unwrap());
        let offer = structured(&hub, &mut person, "offers", json!({})).await;
        assert_eq!(offer["state"], "pending");
        // Words that are not words are refused, and nothing is kept for them.
        for bad in ["", "   ", "yes\u{0}", &"y".repeat(1001)] {
            let answer = hub.call(&mut person, "offers_accept", json!({ "consent": bad })).await;
            assert!(refused(&answer), "{bad:?}: {answer}");
        }
        assert_eq!(hub.accounts.offer(&tenant).await.unwrap().unwrap().consent, words);
        // One yes on the chain: the refused words left nothing.
        let accepted = chain(&hub, &tenant).await;
        assert_eq!(accepted.iter().filter(|(e, _)| e == "offers.accepted").count(), 1);
    }
}

/// A stand-in for Nomen's user API: the users it has, and every request it got.
#[derive(Default)]
struct FakeNomen {
    /// `(id, email, verified)`.
    users: Mutex<Vec<(String, String, bool)>>,
    requests: Mutex<Vec<(String, String, Value)>>,
    down: std::sync::atomic::AtomicBool,
}

async fn list_users(
    State(fake): State<Arc<FakeNomen>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let auth = headers[header::AUTHORIZATION].to_str().unwrap().to_string();
    fake.requests.lock().unwrap().push(("/v2/users".into(), auth, body.clone()));
    if fake.down.load(std::sync::atomic::Ordering::SeqCst) {
        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let email = body["queries"][0]["emailQuery"]["emailAddress"].as_str().unwrap().to_lowercase();
    let result: Vec<Value> = fake
        .users
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, address, _)| *address == email)
        .map(|(id, address, verified)| json!({
            "userId": id, "username": address, "state": "USER_STATE_ACTIVE",
            "human": { "profile": { "givenName": "A", "familyName": "B" },
                       "email": { "email": address, "isVerified": verified } } }))
        .collect();
    Json(json!({ "details": { "totalResult": result.len().to_string() }, "result": result }))
        .into_response()
}

async fn create_user(
    State(fake): State<Arc<FakeNomen>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    let auth = headers[header::AUTHORIZATION].to_str().unwrap().to_string();
    fake.requests.lock().unwrap().push(("/v2/users/new".into(), auth, body.clone()));
    let mut users = fake.users.lock().unwrap();
    let id = format!("3500000000000000{:02}", users.len() + 1);
    let email = body["human"]["email"]["email"].as_str().unwrap().to_string();
    users.push((id.clone(), email, true));
    Json(json!({ "id": id, "creationDate": "2026-11-03T00:00:00Z" }))
}

async fn fake_nomen() -> (String, Arc<FakeNomen>) {
    let fake = Arc::new(FakeNomen::default());
    let app = axum::Router::new()
        .route("/v2/users", post(list_users))
        .route("/v2/users/new", post(create_user))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .unwrap()
    });
    (url, fake)
}

/// Someone new whose email the hub verified (they secured the account with GitHub).
async fn verified_person(hub: &Hosted, n: u32, email: &str) -> (Person, String, String) {
    let mut person = hub.first_connect("Claude").await;
    let (subject, tenant) = who(hub, &mut person).await;
    hub.accounts
        .link_identity(
            &subject,
            &Proven {
                provider: "github".into(),
                subject: format!("{n}"),
                verified_email: Some(email.into()),
                name: Some("Ada Lovelace".into()),
            },
        )
        .await
        .unwrap();
    (person, subject, tenant)
}

#[tokio::test]
async fn with_provisioning_a_yes_links_the_account_on_nomen_with_no_click() {
    for url in stores().await {
        let (nomen, fake) = fake_nomen().await;
        let hub = hosted_with(url.as_deref(), |dir, _| {
            json!({
                "auth": nomen_sign_in(dir),
                "shippin": { "provisioning": {
                    "api": nomen, "organization_id": "org-1",
                    "token_file": private(dir, "nomen-token", "pat-123\n"),
                } },
            })
        })
        .await;
        // Nobody on Nomen has the address: the hub makes the user, verified, in the org.
        let (mut ada, ada_id, ada_tenant) = verified_person(&hub, 1, "ada@example.org").await;
        let accepted = structured(&hub, &mut ada, "offers_accept", json!({ "consent": "yes" })).await;
        assert_eq!(accepted["state"], "linked", "{accepted}");
        assert!(accepted["message"].as_str().unwrap().contains("made an account for ada@example.org"));
        {
            let requests = fake.requests.lock().unwrap();
            assert_eq!(requests.len(), 2, "{requests:?}");
            let (path, auth, body) = &requests[0];
            assert_eq!((path.as_str(), auth.as_str()), ("/v2/users", "Bearer pat-123"));
            assert_eq!(body["queries"][0]["emailQuery"]["method"], "TEXT_QUERY_METHOD_EQUALS_IGNORE_CASE");
            assert_eq!(body["queries"][1]["organizationIdQuery"]["organizationId"], "org-1");
            let (path, auth, body) = &requests[1];
            assert_eq!((path.as_str(), auth.as_str()), ("/v2/users/new", "Bearer pat-123"));
            assert_eq!(body["organizationId"], "org-1");
            assert_eq!(body["username"], "ada@example.org");
            assert_eq!(body["human"]["email"], json!({ "email": "ada@example.org", "isVerified": true }));
            for name in ["givenName", "familyName"] {
                assert!(!body["human"]["profile"][name].as_str().unwrap().is_empty(), "{body}");
            }
        }
        // Linked: Sign in with Shippin as that Nomen user reaches this account from now on.
        let made = fake.users.lock().unwrap()[0].0.clone();
        let reached = hub
            .accounts
            .sign_in_identity(&Proven {
                provider: "nomen".into(),
                subject: made,
                verified_email: None,
                name: None,
            })
            .await
            .unwrap();
        assert_eq!(reached.id, ada_id);
        let row = hub.accounts.offer(&ada_tenant).await.unwrap().unwrap();
        assert_eq!((row.state.as_str(), row.linked_via.as_deref()), ("linked", Some("provisioned")));
        let events: Vec<String> = chain(&hub, &ada_tenant).await.into_iter().map(|(e, _)| e).collect();
        let yes = events.iter().position(|e| e == "offers.accepted").unwrap();
        let linked = events.iter().position(|e| e == "offers.linked").unwrap();
        assert!(yes < linked, "{events:?}");
        assert_eq!(structured(&hub, &mut ada, "offers", json!({})).await["state"], "linked");
        // Asked again: already linked, nothing more kept and nothing asked of Nomen.
        let again = structured(&hub, &mut ada, "offers_accept", json!({ "consent": "yes again" })).await;
        assert_eq!(again["state"], "linked");
        assert_eq!(fake.requests.lock().unwrap().len(), 2);
        assert_eq!(hub.accounts.offer(&ada_tenant).await.unwrap().unwrap().consent, "yes");

        // Someone Nomen has, verified: found and linked, nothing made.
        fake.users.lock().unwrap().push(("350000000000000099".into(), "bo@example.org".into(), true));
        let (mut bo, bo_id, _) = verified_person(&hub, 2, "bo@example.org").await;
        let accepted = structured(&hub, &mut bo, "offers_accept", json!({ "consent": "ok" })).await;
        assert_eq!(accepted["state"], "linked", "{accepted}");
        assert_eq!(fake.requests.lock().unwrap().len(), 3, "found, not made");
        let reached = hub
            .accounts
            .sign_in_identity(&Proven {
                provider: "nomen".into(),
                subject: "350000000000000099".into(),
                verified_email: None,
                name: None,
            })
            .await
            .unwrap();
        assert_eq!(reached.id, bo_id);

        // An address Nomen has but never verified is not linked: the one-tap link instead.
        fake.users.lock().unwrap().push(("350000000000000098".into(), "cy@example.org".into(), false));
        let (mut cy, _, cy_tenant) = verified_person(&hub, 3, "cy@example.org").await;
        let accepted = structured(&hub, &mut cy, "offers_accept", json!({ "consent": "yes" })).await;
        assert_eq!(accepted["state"], "link", "{accepted}");
        path_of(accepted["link"].as_str().unwrap(), &hub);
        assert_eq!(hub.accounts.offer(&cy_tenant).await.unwrap().unwrap().state, "pending");

        // Nomen down: the yes is kept, and the one-tap link is the way.
        fake.down.store(true, std::sync::atomic::Ordering::SeqCst);
        let (mut di, _, di_tenant) = verified_person(&hub, 4, "di@example.org").await;
        let accepted = structured(&hub, &mut di, "offers_accept", json!({ "consent": "go" })).await;
        assert_eq!(accepted["state"], "link", "{accepted}");
        assert_eq!(hub.accounts.offer(&di_tenant).await.unwrap().unwrap().consent, "go");

        // No verified email (an account made on first connect): the link, Nomen not asked.
        fake.down.store(false, std::sync::atomic::Ordering::SeqCst);
        let asked = fake.requests.lock().unwrap().len();
        let mut new = hub.first_connect("ChatGPT").await;
        let accepted = structured(&hub, &mut new, "offers_accept", json!({ "consent": "yes" })).await;
        assert_eq!(accepted["state"], "link", "{accepted}");
        assert_eq!(fake.requests.lock().unwrap().len(), asked);
    }
}

#[tokio::test]
async fn the_one_tap_link_signs_in_with_the_network_and_offers_then_says_linked() {
    for url in stores().await {
        let hub = hosted_with(url.as_deref(), |dir, _| json!({ "auth": nomen_sign_in(dir) })).await;
        let mut person = hub.first_connect("Claude").await;
        let (subject, tenant) = who(&hub, &mut person).await;
        let accepted = structured(&hub, &mut person, "offers_accept", json!({ "consent": "Yes please" })).await;
        assert_eq!(accepted["state"], "link", "{accepted}");
        assert!(accepted["expires_at"].as_u64().unwrap() > crate::devices::now_secs());
        let path = path_of(accepted["link"].as_str().unwrap(), &hub);
        // Opening it spends nothing: what the button does, and the button.
        let opened = hub.get(&path, None).await;
        assert_eq!(opened.headers()[header::REFERRER_POLICY], "same-origin");
        let (status, page) = Hosted::text(opened).await;
        assert_eq!(status, 200, "{page}");
        assert!(page.contains("<h1>Link your account to the Shippin Network</h1>"), "{page}");
        assert!(page.contains(&format!("<form method=\"post\" action=\"{path}\">")), "{page}");
        assert!(page.contains(">Sign in with Shippin</button>"), "{page}");
        // A post from another site does nothing.
        let crossed = hub
            .http
            .post(format!("{}{path}", hub.url))
            .header(header::ORIGIN, "https://evil.example")
            .send()
            .await
            .unwrap();
        assert_eq!(crossed.status(), 303);
        assert!(set_cookie(&crossed, "dg_session").is_none());
        // One tap, in another browser: it joins the account nobody secured yet, then goes
        // on to Sign in with Shippin, which links it and comes back here.
        let tapped = hub.form(&path, None, &[]).await;
        assert_eq!(tapped.status(), 303);
        let location = tapped.headers()[header::LOCATION].to_str().unwrap().to_string();
        assert_eq!(
            location
                .strip_prefix("/auth/provider/nomen?return_to=")
                .and_then(|rest| rest.strip_suffix("&link=1"))
                .map(decode),
            Some(path.clone()),
            "{location}"
        );
        let cookie = set_cookie(&tapped, "dg_session").expect("a session of the account");
        let me: Value = hub.get("/api/v1/me", Some(&cookie)).await.json().await.unwrap();
        assert_eq!(me["subject"], subject.as_str());
        // (Sign in with Shippin proves this user, and the hub links it, as the account page does.)
        hub.accounts
            .link_identity(
                &subject,
                &Proven {
                    provider: "nomen".into(),
                    subject: "n-1".into(),
                    verified_email: None,
                    name: None,
                },
            )
            .await
            .unwrap();
        let (status, page) = Hosted::text(hub.get(&path, Some(&cookie)).await).await;
        assert_eq!(status, 200);
        assert!(page.contains("Your account is linked to the Shippin Network."), "{page}");
        assert!(!page.contains("action=\"/offers/"), "{page}");
        // The AI sees it.
        assert_eq!(structured(&hub, &mut person, "offers", json!({})).await["state"], "linked");
        let row = hub.accounts.offer(&tenant).await.unwrap().unwrap();
        assert_eq!((row.state.as_str(), row.linked_via.as_deref()), ("linked", Some("sign_in")));
        assert_eq!(row.consent, "Yes please");
        let events = chain(&hub, &tenant).await;
        assert!(events.contains(&("offers.linked".to_string(), "sign_in".to_string())));
        assert!(hub.accounts.chain_intact(&tenant).await.unwrap());

        // A secured account signs in first and comes back; signed in, straight on.
        let mut other = hub.first_connect("Grok").await;
        let (other_id, other_tenant) = who(&hub, &mut other).await;
        hub.accounts
            .link_identity(
                &other_id,
                &Proven {
                    provider: "github".into(),
                    subject: "77".into(),
                    verified_email: None,
                    name: None,
                },
            )
            .await
            .unwrap();
        let first = structured(&hub, &mut other, "offers_accept", json!({ "consent": "yes" })).await;
        let first = path_of(first["link"].as_str().unwrap(), &hub);
        let tapped = hub.form(&first, None, &[]).await;
        assert_eq!(tapped.status(), 303);
        let location = tapped.headers()[header::LOCATION].to_str().unwrap();
        assert_eq!(
            location.strip_prefix("/auth/login?return_to=").map(decode),
            Some(first.clone()),
            "{location}"
        );
        assert!(set_cookie(&tapped, "dg_session").is_none());
        let tapped = hub.form(&first, Some(&other.cookie), &[]).await;
        assert!(
            tapped.headers()[header::LOCATION]
                .to_str()
                .unwrap()
                .starts_with("/auth/provider/nomen?return_to=")
        );
        // A new yes replaces the link; one past its time works no more.
        let second = structured(&hub, &mut other, "offers_accept", json!({ "consent": "yes!" })).await;
        let second = path_of(second["link"].as_str().unwrap(), &hub);
        for gone in [&first, "/offers/dgo_nothing-here"] {
            let (status, page) = Hosted::text(hub.get(gone, None).await).await;
            assert_eq!(status, 404, "{gone}");
            assert!(page.contains("This link has expired or was replaced."), "{page}");
        }
        sqlx::query("UPDATE tenancy_offers SET link_expires_at = 1 WHERE tenant_id = $1")
            .bind(&other_tenant)
            .execute(hub.accounts.store().admin())
            .await
            .unwrap();
        assert_eq!(hub.get(&second, None).await.status(), 404);
        let tapped = hub.form(&second, Some(&other.cookie), &[]).await;
        assert_eq!(tapped.status(), 404);
    }
}

/// What a site's offers leave on everything a person or an AI reads: the tools, the
/// instructions, the landing page and its rail, the public pages, `/llms.txt`.
async fn surfaces(hub: &Hosted, person: &mut Person) -> (Vec<String>, String, String) {
    let names = tool_names(hub, person).await;
    let said = instructions(hub);
    let mut pages = String::new();
    for path in ["/", "/docs", "/privacy", "/terms", "/support", "/llms.txt", "/llms-full.txt", "/assets/hub.css"] {
        pages.push_str(&Hosted::text(hub.get(path, None).await).await.1);
    }
    (names, said, pages)
}

fn assert_nothing_of_the_offers(what: &str, (names, said, pages): &(Vec<String>, String, String)) {
    let site = crate::site::builtin();
    let offers = site.info.offers.as_ref().unwrap();
    for tool in crate::offers::TOOLS {
        assert!(!names.iter().any(|n| n == tool), "{what}: {tool} is listed");
    }
    let mut words = vec![offers.brand.clone(), "id=\"network\"".into(), "--p-network".into(), "offers_accept".into()];
    words.extend(offers.items.iter().filter(|i| !i.href.is_empty()).map(|i| i.href.clone()));
    words.extend(offers.items.iter().filter(|i| !i.text.is_empty()).map(|i| i.text.clone()));
    for word in &words {
        assert!(!said.contains(word.as_str()), "{what}: the instructions say {word}");
        assert!(!pages.contains(word.as_str()), "{what}: a page says {word}");
    }
}

/// A copy of the built-in site changed by `edit`.
pub(crate) fn builtin_with(dir: &std::path::Path, edit: impl FnOnce(&mut Value)) -> std::path::PathBuf {
    let site = dir.join("site");
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("site/default");
    for sub in ["", "content", "assets"] {
        std::fs::create_dir_all(site.join(sub)).unwrap();
        for entry in std::fs::read_dir(source.join(sub)).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_file() {
                std::fs::copy(entry.path(), site.join(sub).join(entry.file_name())).unwrap();
            }
        }
    }
    let file = site.join("site.json");
    let mut value: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    edit(&mut value);
    std::fs::write(&file, value.to_string()).unwrap();
    site
}

#[tokio::test]
async fn off_or_empty_or_on_a_neutral_site_nothing_of_the_offers_shows() {
    // On, by default: the section, its rail, the tools and the sentence are there.
    let on = hosted_with(None, |_, _| json!({})).await;
    let mut person = on.first_connect("Claude").await;
    let shown = surfaces(&on, &mut person).await;
    assert!(shown.0.iter().any(|n| n == "offers_accept"));
    assert!(shown.1.contains("The Shippin Network"));
    for needle in [
        "<section class=\"band network\" id=\"network\"",
        "<a href=\"#network\"><span>Shippin Network</span></a>",
        "<a href=\"https://vaultix.cloud\">Vaultix</a>",
        "#network { view-timeline: --p-network block, --page block; }",
        "<code>offers</code> <code>offers_accept</code>",
    ] {
        assert!(shown.2.contains(needle), "{needle}");
    }
    // Off, or with no offerings, and on the neutral site: gone, everywhere.
    let neutral = crate::site::tests::neutral_dir();
    let cases: Vec<(&str, Box<dyn FnOnce(&std::path::Path) -> std::path::PathBuf + Send>)> = vec![
        ("enabled false", Box::new(|dir| builtin_with(dir, |v| v["offers"]["enabled"] = false.into()))),
        ("no offerings", Box::new(|dir| builtin_with(dir, |v| v["offers"]["items"] = json!([])))),
        ("no offers block", Box::new(|dir| builtin_with(dir, |v| { v.as_object_mut().unwrap().remove("offers"); }))),
        ("the neutral site", Box::new(move |_| neutral.clone())),
    ];
    for (what, site) in cases {
        let hub = hosted_with(None, |dir, _| json!({ "site": { "dir": site(dir).to_string_lossy() } })).await;
        let mut person = hub.first_connect("Claude").await;
        let seen = surfaces(&hub, &mut person).await;
        assert_nothing_of_the_offers(what, &seen);
        // Asked for anyway: no such tool, and no such page.
        let answer = hub.call(&mut person, "offers", json!({})).await;
        assert!(refused(&answer), "{what}: {answer}");
        let answer = hub.call(&mut person, "offers_accept", json!({ "consent": "yes" })).await;
        assert!(refused(&answer), "{what}: {answer}");
        let (_, tenant) = who(&hub, &mut person).await;
        assert!(hub.accounts.offer(&tenant).await.unwrap().is_none(), "{what}: nothing kept");
        let (status, _) = Hosted::text(hub.get("/offers/dgo_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", None).await).await;
        assert_eq!(status, 404, "{what}");
    }
}
