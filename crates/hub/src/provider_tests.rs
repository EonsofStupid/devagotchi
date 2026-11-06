//! Sign in with Google, GitHub, Shippin (Nomen as a generic OpenID Connect provider) and an
//! email link, on a hosted hub, against stub providers and a stub mail server on loopback:
//! the whole browser round trip, as a browser makes it.

use std::sync::{Arc, Mutex};

use axum::{
    Form, Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};

use crate::tenant_tests::{Hosted, hosted_with, query_param, set_cookie};

/// Who the stub says is signing in, and what it should get wrong on purpose.
#[derive(Clone, Default)]
struct Person {
    sub: String,
    email: Option<String>,
    verified: bool,
    wrong_audience: bool,
}

#[derive(Default)]
struct Stub {
    url: String,
    person: Person,
    /// code → (nonce, PKCE challenge)
    codes: Vec<(String, String, String)>,
    /// What the token endpoint was sent, to check the client authenticated.
    authorizations: Vec<Option<String>>,
}

type Shared = Arc<Mutex<Stub>>;

#[derive(serde::Deserialize)]
struct Authorize {
    redirect_uri: String,
    state: String,
    #[serde(default)]
    nonce: Option<String>,
    code_challenge: String,
}

async fn authorize(State(stub): State<Shared>, Query(asked): Query<Authorize>) -> Response {
    let code = format!("code-{}", hex::encode(crate::link::random::<8>()));
    stub.lock().unwrap().codes.push((
        code.clone(),
        asked.nonce.unwrap_or_default(),
        asked.code_challenge,
    ));
    Redirect::to(&format!(
        "{}?code={code}&state={}",
        asked.redirect_uri, asked.state
    ))
    .into_response()
}

/// The code back for its nonce, when the PKCE verifier matches its challenge.
fn redeem(stub: &mut Stub, form: &std::collections::HashMap<String, String>) -> Option<String> {
    let code = form.get("code")?;
    let at = stub.codes.iter().position(|(c, ..)| c == code)?;
    let (_, nonce, challenge) = stub.codes.remove(at);
    let verifier = form.get("code_verifier")?;
    (crate::accounts::oauth::s256(verifier) == challenge).then_some(nonce)
}

async fn oidc_token(
    State(stub): State<Shared>,
    headers: HeaderMap,
    Form(form): Form<std::collections::HashMap<String, String>>,
) -> Response {
    let mut stub = stub.lock().unwrap();
    stub.authorizations.push(
        headers
            .get(header::AUTHORIZATION)
            .map(|v| v.to_str().unwrap().to_string()),
    );
    let Some(nonce) = redeem(&mut stub, &form) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid_grant" })),
        )
            .into_response();
    };
    let person = stub.person.clone();
    let claims = json!({
        "iss": stub.url,
        "aud": if person.wrong_audience { "someone-else" } else { "client-1" },
        "sub": person.sub,
        "email": person.email,
        "email_verified": person.verified,
        "name": "Stub Person",
        "nonce": nonce,
        "exp": crate::devices::now_secs() + 300,
        "iat": crate::devices::now_secs(),
    });
    let id_token = format!(
        "{}.{}.{}",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256"}"#),
        URL_SAFE_NO_PAD.encode(claims.to_string()),
        URL_SAFE_NO_PAD.encode(b"signature")
    );
    Json(json!({ "access_token": "at", "token_type": "Bearer", "id_token": id_token }))
        .into_response()
}

async fn discovery(State(stub): State<Shared>) -> Json<Value> {
    let url = stub.lock().unwrap().url.clone();
    Json(json!({
        "issuer": url,
        "authorization_endpoint": format!("{url}/authorize"),
        "token_endpoint": format!("{url}/token"),
        "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post"],
    }))
}

async fn github_token(
    State(stub): State<Shared>,
    Form(form): Form<std::collections::HashMap<String, String>>,
) -> Response {
    let mut stub = stub.lock().unwrap();
    stub.authorizations.push(form.get("client_secret").cloned());
    match redeem(&mut stub, &form) {
        Some(_) => {
            Json(json!({ "access_token": "gh-access", "token_type": "bearer" })).into_response()
        }
        None => Json(json!({ "error": "bad_verification_code" })).into_response(),
    }
}

async fn github_user(State(stub): State<Shared>, headers: HeaderMap) -> Response {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        != Some("Bearer gh-access")
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let person = stub.lock().unwrap().person.clone();
    Json(json!({ "id": person.sub.parse::<u64>().unwrap(), "login": "octo", "name": null }))
        .into_response()
}

async fn github_emails(State(stub): State<Shared>) -> Json<Value> {
    let person = stub.lock().unwrap().person.clone();
    Json(json!([
        { "email": "old@example.com", "primary": false, "verified": true },
        { "email": person.email, "primary": true, "verified": person.verified },
    ]))
}

async fn stub_server(person: Person) -> Shared {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let stub = Arc::new(Mutex::new(Stub {
        url,
        person,
        ..Stub::default()
    }));
    let app = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/authorize", get(authorize))
        .route("/token", post(oidc_token))
        .route("/login/oauth/authorize", get(authorize))
        .route("/login/oauth/access_token", post(github_token))
        .route("/user", get(github_user))
        .route("/user/emails", get(github_emails))
        .with_state(stub.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    stub
}

struct World {
    hub: Hosted,
    stub: Shared,
    mail: crate::mail::stub::Stub,
}

async fn world(person: Person) -> World {
    let stub = stub_server(person).await;
    let mail = crate::mail::stub::start("hub").await;
    let stub_url = stub.lock().unwrap().url.clone();
    let mail_url = mail.url.clone();
    let hub = hosted_with(None, |dir, _| {
        let secret = dir.join("provider-secret");
        std::fs::write(&secret, "s3cret\n").unwrap();
        let password = dir.join("smtp-password");
        std::fs::write(&password, "mail-pass\n").unwrap();
        #[cfg(unix)]
        for file in [&secret, &password] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        json!({
            "operators": ["email:jesse@example.com"],
            "auth": {
                "providers": [
                    { "kind": "google", "issuer": stub_url, "client_id": "client-1",
                      "client_secret_file": secret.to_string_lossy() },
                    { "kind": "oidc", "name": "nomen", "label": "Shippin", "issuer": stub_url,
                      "client_id": "client-1" },
                    { "kind": "github", "client_id": "client-1",
                      "client_secret_file": secret.to_string_lossy(),
                      "authorize_url": format!("{stub_url}/login/oauth/authorize"),
                      "token_url": format!("{stub_url}/login/oauth/access_token"),
                      "api_url": stub_url }
                ],
                "email": { "smtp_url": mail_url, "password_file": password.to_string_lossy(),
                           "from": "Devagotchi <sign-in@devagotchi.test>" }
            }
        })
    })
    .await;
    World { hub, stub, mail }
}

impl World {
    fn person(&self, person: Person) {
        self.stub.lock().unwrap().person = person;
    }

    /// The browser round trip through provider `name`, from a browser holding `cookie`:
    /// the hub's redirect, the provider's, and the hub's callback answer.
    async fn sign_in(&self, name: &str, cookie: Option<&str>, link: bool) -> reqwest::Response {
        let path = format!(
            "/auth/provider/{name}?return_to=%2Faccount{}",
            if link { "&link=1" } else { "" }
        );
        let started = self.hub.get(&path, cookie).await;
        assert_eq!(started.status(), 303, "{name}");
        let signin = set_cookie(&started, "dg_signin").expect("the sign-in cookie");
        let to_provider = started.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .to_string();
        assert_eq!(
            query_param(&to_provider, "code_challenge_method").as_deref(),
            Some("S256")
        );
        let back = self.hub.http.get(&to_provider).send().await.unwrap();
        let callback = back.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .to_string();
        assert!(callback.starts_with(&format!("{}/auth/provider/{name}/callback", self.hub.url)));
        let jar = match cookie {
            Some(cookie) => format!("{cookie}; {signin}"),
            None => signin,
        };
        self.hub
            .http
            .get(&callback)
            .header(header::COOKIE, jar)
            .send()
            .await
            .unwrap()
    }

    async fn account_page(&self, cookie: &str) -> String {
        Hosted::text(self.hub.get("/account", Some(cookie)).await)
            .await
            .1
    }
}

fn google(sub: &str, email: Option<&str>) -> Person {
    Person {
        sub: sub.into(),
        email: email.map(str::to_string),
        verified: true,
        ..Person::default()
    }
}

#[tokio::test]
async fn google_and_shippin_sign_a_new_person_in_to_a_tenant_of_their_own() {
    let world = world(google("g-100", Some("sam@example.com"))).await;
    let home = world.hub.accounts.home_tenant().await.unwrap();
    let done = world.sign_in("google", None, false).await;
    assert_eq!(done.status(), 303);
    assert_eq!(done.headers()[header::LOCATION], "/account");
    let session = set_cookie(&done, "dg_session").expect("signed in");
    assert!(
        set_cookie(&done, "dg_signin").is_some_and(|c| c == "dg_signin="),
        "the sign-in cookie is spent"
    );
    let page = world.account_page(&session).await;
    assert!(
        page.contains("How you sign in") && page.contains("google (sam@example.com)"),
        "{page}"
    );
    assert!(
        !page.contains("id=\"secure\""),
        "secured from the start: {page}"
    );
    // The client authenticated to the token endpoint with its secret, by HTTP Basic.
    let sent = world.stub.lock().unwrap().authorizations.clone();
    assert!(
        sent[0].as_deref().is_some_and(|a| a.starts_with("Basic ")),
        "{sent:?}"
    );
    // Shippin (Nomen as a public OpenID Connect client) with the same verified email reaches
    // the same account; a stranger gets a tenant of their own, not the operator's.
    world.person(google("nomen-7", Some("sam@example.com")));
    let again = world.sign_in("nomen", None, false).await;
    let second = set_cookie(&again, "dg_session").unwrap();
    let page = world.account_page(&second).await;
    assert!(
        page.contains("nomen (sam@example.com)") && page.contains("google (sam@example.com)"),
        "{page}"
    );
    let made = world
        .hub
        .accounts
        .sign_in_identity(&crate::accounts::identities::Proven {
            provider: "google".into(),
            subject: "g-100".into(),
            verified_email: None,
            name: None,
        })
        .await
        .unwrap();
    assert_ne!(made.tenant, home);
    assert!(!made.owner);
}

#[tokio::test]
async fn the_operators_google_sign_in_is_the_owner_with_no_claim() {
    let world = world(google("g-1", Some("jesse@example.com"))).await;
    let done = world.sign_in("google", None, false).await;
    let session = set_cookie(&done, "dg_session").unwrap();
    let page = world.account_page(&session).await;
    assert!(page.contains("You own this hub"), "{page}");
    let owner = world.hub.accounts.owner().await.unwrap().unwrap();
    assert_eq!(
        owner.tenant,
        world.hub.accounts.home_tenant().await.unwrap()
    );
    // An unverified email is nobody's: not the operator's.
    world.person(Person {
        verified: false,
        ..google("g-2", Some("jesse@example.com"))
    });
    let other = world.sign_in("google", None, false).await;
    let page = world
        .account_page(&set_cookie(&other, "dg_session").unwrap())
        .await;
    assert!(!page.contains("You own this hub"), "{page}");
}

#[tokio::test]
async fn github_secures_the_account_made_on_first_connect() {
    let world = world(Person {
        sub: "4242".into(),
        email: Some("octo@example.com".into()),
        verified: true,
        ..Person::default()
    })
    .await;
    let person = world.hub.first_connect("Claude").await;
    let before = world.account_page(&person.cookie).await;
    assert!(before.contains("id=\"secure\""), "{before}");
    assert!(
        before.contains("/auth/provider/github?return_to=%2Faccount&amp;link=1"),
        "{before}"
    );
    assert!(
        before.contains("Continue with Shippin") && before.contains("Email me a sign-in link"),
        "{before}"
    );
    let done = world.sign_in("github", Some(&person.cookie), true).await;
    assert_eq!(done.status(), 303);
    assert!(
        set_cookie(&done, "dg_session").is_none(),
        "the same session goes on"
    );
    let after = world.account_page(&person.cookie).await;
    assert!(
        !after.contains("id=\"secure\"") && !after.contains("class=\"nudge\""),
        "{after}"
    );
    assert!(
        after.contains("github (octo@example.com)") && after.contains("account.secured"),
        "{after}"
    );
    // The client secret went in the form, as GitHub takes it.
    let sent = world.stub.lock().unwrap().authorizations.clone();
    assert_eq!(sent.last().unwrap().as_deref(), Some("s3cret"));
    // The same GitHub account later, from a browser with no session, is this account.
    let back = world.sign_in("github", None, false).await;
    let page = world
        .account_page(&set_cookie(&back, "dg_session").unwrap())
        .await;
    assert!(page.contains("github (octo@example.com)"), "{page}");
}

#[tokio::test]
async fn a_sign_in_that_does_not_check_out_is_refused() {
    let world = world(google("g-3", Some("x@example.com"))).await;
    // The provider's answer for another client.
    world.person(Person {
        wrong_audience: true,
        ..google("g-3", Some("x@example.com"))
    });
    assert_eq!(world.sign_in("google", None, false).await.status(), 400);
    world.person(google("g-3", Some("x@example.com")));
    // A callback without the browser's sign-in cookie, or with another state.
    let started = world.hub.get("/auth/provider/google", None).await;
    let signin = set_cookie(&started, "dg_signin").unwrap();
    let to_provider = started.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_string();
    let back = world.hub.http.get(&to_provider).send().await.unwrap();
    let callback = back.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_string();
    let bare = world.hub.http.get(&callback).send().await.unwrap();
    assert_eq!(bare.status(), 400);
    let swapped = callback.replace(&query_param(&callback, "state").unwrap(), "another-state");
    let wrong = world
        .hub
        .http
        .get(&swapped)
        .header(header::COOKIE, &signin)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 400);
    // A forged cookie (its MAC broken) is no sign-in.
    let forged = format!("{}x", signin);
    let refused = world
        .hub
        .http
        .get(&callback)
        .header(header::COOKIE, forged)
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 400);
    // An unknown provider.
    assert_eq!(
        world.hub.get("/auth/provider/twitter", None).await.status(),
        404
    );
}

/// The link in the last message the stub mail server got.
fn mailed_link(world: &World) -> String {
    let received = world.mail.received.lock().unwrap().clone();
    let message = received.last().expect("a message");
    message
        .data
        .lines()
        .find(|line| line.contains("/auth/email"))
        .unwrap()
        .trim()
        .to_string()
}

async fn wait_for_mail(world: &World, count: usize) {
    for _ in 0..200 {
        if world.mail.received.lock().unwrap().len() >= count {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("no mail arrived");
}

#[tokio::test]
async fn an_email_link_signs_in_once_and_secures_an_account() {
    let world = world(google("g-9", None)).await;
    // Asking says the same thing whatever the address; the link goes only by mail.
    let (status, page) = Hosted::text(
        world
            .hub
            .form(
                "/auth/email",
                None,
                &[("email", "Pat@Example.com"), ("return_to", "/account")],
            )
            .await,
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        page.contains("Check your email") && !page.contains("dge_"),
        "{page}"
    );
    wait_for_mail(&world, 1).await;
    let received = world.mail.received.lock().unwrap().clone();
    assert_eq!(received[0].to, "pat@example.com");
    assert!(
        received[0].auth.is_some(),
        "the mail server's user signed in"
    );
    let link = mailed_link(&world);
    let (path, token) = link
        .strip_prefix(&world.hub.url)
        .unwrap()
        .split_once('#')
        .unwrap();
    let (_, opened) = Hosted::text(world.hub.get(path, None).await).await;
    assert!(
        opened.contains("id=\"email-token\"") && !opened.contains(token),
        "{opened}"
    );
    let done = world
        .hub
        .form(
            "/auth/email/verify",
            None,
            &[("token", token), ("return_to", "/account")],
        )
        .await;
    assert_eq!(done.status(), 303);
    let session = set_cookie(&done, "dg_session").expect("signed in");
    let page = world.account_page(&session).await;
    assert!(page.contains("email (pat@example.com)"), "{page}");
    // Once only.
    let again = world
        .hub
        .form("/auth/email/verify", None, &[("token", token)])
        .await;
    assert_eq!(again.status(), 400);

    // From the account page of an account made on first connect: the link adds the address,
    // in the browser that asked, and secures the account.
    let person = world.hub.first_connect("Claude").await;
    world
        .hub
        .form(
            "/auth/email",
            Some(&person.cookie),
            &[("email", "new@example.com"), ("link", "1")],
        )
        .await;
    wait_for_mail(&world, 2).await;
    let link = mailed_link(&world);
    let token = link.split_once('#').unwrap().1.to_string();
    // Not in another browser.
    let elsewhere = world
        .hub
        .form("/auth/email/verify", Some(&session), &[("token", &token)])
        .await;
    assert_eq!(elsewhere.status(), 403);
    world
        .hub
        .form(
            "/auth/email",
            Some(&person.cookie),
            &[("email", "new@example.com"), ("link", "1")],
        )
        .await;
    wait_for_mail(&world, 3).await;
    let token = mailed_link(&world).split_once('#').unwrap().1.to_string();
    let linked = world
        .hub
        .form(
            "/auth/email/verify",
            Some(&person.cookie),
            &[("token", &token)],
        )
        .await;
    assert_eq!(linked.status(), 303);
    let page = world.account_page(&person.cookie).await;
    assert!(
        page.contains("email (new@example.com)") && !page.contains("id=\"secure\""),
        "{page}"
    );
    let _ = &world.hub.dir;
}

#[tokio::test]
async fn email_links_are_rate_limited_per_address() {
    let world = world(google("g-9", None)).await;
    for _ in 0..5 {
        let (status, _) = Hosted::text(
            world
                .hub
                .form("/auth/email", None, &[("email", "spam@example.com")])
                .await,
        )
        .await;
        assert_eq!(status, 200);
    }
    let (status, _) = Hosted::text(
        world
            .hub
            .form("/auth/email", None, &[("email", "spam@example.com")])
            .await,
    )
    .await;
    assert_eq!(status, 429);
}

#[tokio::test]
async fn google_for_an_address_with_a_password_account_asks_to_link_it() {
    // Spec first-connect, A2: the same address signed up with a password, then Google.
    let world = world(google("g-300", Some("pat@example.com"))).await;
    let password = world
        .hub
        .accounts
        .sign_up_password("pat@example.com", "pats password")
        .await
        .unwrap();
    let asked = world.sign_in("google", None, false).await;
    assert!(
        set_cookie(&asked, "dg_session").is_none(),
        "nobody signed in yet"
    );
    let waiting = set_cookie(&asked, "dg_link").expect("the proven sign-in waits");
    let (status, page) = Hosted::text(asked).await;
    assert_eq!(status, 200);
    assert!(
        page.contains("pat@example.com already has an account. Link Google to it?"),
        "{page}"
    );
    assert!(
        page.contains(">Link<") && page.contains(">Keep separate<"),
        "{page}"
    );
    assert!(
        page.contains("Enter this account&#39;s password to link Google to it.")
            && page.contains("name=\"password\""),
        "{page}"
    );
    assert!(
        page.contains("Password for pat@example.com") && page.contains("Forgot password?"),
        "{page}"
    );
    // Link with the account's password: Google now signs in to the password account.
    let linked = world
        .hub
        .form(
            "/auth/provider/link",
            Some(&waiting),
            &[("decision", "link"), ("password", "pats password")],
        )
        .await;
    assert_eq!(linked.status(), 303);
    assert_eq!(linked.headers()[header::LOCATION], "/account");
    let session = set_cookie(&linked, "dg_session").expect("signed in");
    assert!(world.account_page(&session).await.contains(&password.id));
    let again = world.sign_in("google", None, false).await;
    assert!(
        set_cookie(&again, "dg_link").is_none(),
        "linked: no question twice"
    );
    let back = set_cookie(&again, "dg_session").unwrap();
    assert!(world.account_page(&back).await.contains(&password.id));
    // A forged or missing waiting cookie links nothing.
    let none = world
        .hub
        .form(
            "/auth/provider/link",
            None,
            &[("decision", "link"), ("password", "pats password")],
        )
        .await;
    assert_eq!(none.status(), 400);
}

#[tokio::test]
async fn link_with_a_wrong_password_links_nothing_and_signs_nobody_in() {
    // Someone signed up first with this address and their own password: the person Google
    // proved the address for cannot be joined to that account without its password.
    let world = world(google("g-302", Some("ash@example.com"))).await;
    let squatter = world
        .hub
        .accounts
        .sign_up_password("ash@example.com", "not ash's password")
        .await
        .unwrap();
    let asked = world.sign_in("google", None, false).await;
    let waiting = set_cookie(&asked, "dg_link").expect("asked");
    for wrong in ["", "a guess at it"] {
        let refused = world
            .hub
            .form(
                "/auth/provider/link",
                Some(&waiting),
                &[("decision", "link"), ("password", wrong)],
            )
            .await;
        assert!(set_cookie(&refused, "dg_session").is_none(), "no session");
        let (status, page) = Hosted::text(refused).await;
        assert_eq!(status, 400, "{page}");
        assert!(page.contains("Email or password is wrong."), "{page}");
        assert!(
            page.contains(">Keep separate<") && page.contains("Forgot password?"),
            "{page}"
        );
    }
    // Nothing was linked: Google still reaches no account and asks again.
    let again = world.sign_in("google", None, false).await;
    assert!(set_cookie(&again, "dg_link").is_some(), "still not linked");
    assert!(set_cookie(&again, "dg_session").is_none());
    // Keep separate still works, as its own account.
    let kept = world
        .hub
        .form(
            "/auth/provider/link",
            Some(&waiting),
            &[("decision", "separate")],
        )
        .await;
    assert_eq!(kept.status(), 303);
    let session = set_cookie(&kept, "dg_session").expect("signed in");
    assert!(!world.account_page(&session).await.contains(&squatter.id));
}

#[tokio::test]
async fn keep_separate_makes_a_new_account_for_the_google_sign_in() {
    let world = world(google("g-301", Some("lee@example.com"))).await;
    let password = world
        .hub
        .accounts
        .sign_up_password("lee@example.com", "lees password")
        .await
        .unwrap();
    let asked = world.sign_in("google", None, false).await;
    let waiting = set_cookie(&asked, "dg_link").expect("asked");
    let kept = world
        .hub
        .form(
            "/auth/provider/link",
            Some(&waiting),
            &[("decision", "separate")],
        )
        .await;
    assert_eq!(kept.status(), 303);
    let session = set_cookie(&kept, "dg_session").expect("signed in");
    let page = world.account_page(&session).await;
    assert!(!page.contains(&password.id), "{page}");
    // The password still reaches its own account.
    let same = world
        .hub
        .accounts
        .sign_in_password("lee@example.com", "lees password")
        .await
        .unwrap();
    assert_eq!(same.id, password.id);
}
