//! A new hub claimed from a browser end to end over HTTP (16c): the landing page offers the
//! claim, the code from the log makes the owner and their passkey on one page, and the hub
//! is then signed in to like any other. The browser is the software authenticator.

use axum::http::header;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};

use super::passkey_tests::{World, b64, cookie, world};

impl World {
    /// The code a starting hub would print now.
    async fn printed(&self) -> String {
        self.accounts
            .issue_claim_code()
            .await
            .unwrap()
            .expect("an unclaimed hub prints a code")
            .code
    }

    /// Claim with `code` as the claim page does, then make the passkey with the options the
    /// claim answered: returns the session cookie and the owner's account id.
    async fn claim(&self, code: &str, name: Option<&str>) -> (String, String) {
        let response = self
            .post("/claim", None, json!({ "code": code, "name": name }))
            .await;
        assert_eq!(response.status(), 200);
        let jar = cookie(&response, "dg_challenge").expect("the challenge cookie");
        let options: Value = response.json().await.unwrap();
        let link = options["link"]
            .as_str()
            .expect("the owner link")
            .to_string();
        let account = String::from_utf8(
            URL_SAFE_NO_PAD
                .decode(options["user"]["id"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        let challenge = URL_SAFE_NO_PAD
            .decode(options["challenge"].as_str().unwrap())
            .unwrap();
        let sent = self.phone().register(&challenge);
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
        (cookie(&response, "dg_session").expect("a session"), account)
    }

    async fn text(&self, path: &str, session: Option<&str>) -> String {
        let response = self.get(path, session).await;
        assert_eq!(response.status(), 200, "{path}");
        response.text().await.unwrap()
    }
}

#[tokio::test]
async fn a_new_hub_is_claimed_from_the_browser_with_the_code_from_its_log() {
    let world = world().await;
    let code = world.printed().await;

    // The landing page of a hub nobody owns offers the claim, and nothing else.
    let landing = world.text("/", None).await;
    assert!(
        landing.contains("Nobody owns this hub yet") && landing.contains("href=\"/claim\""),
        "{landing}"
    );
    assert!(!landing.contains("/auth/login"), "{landing}");
    let page = world.get("/claim", None).await;
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
    assert!(
        html.contains("/assets/passkey.js")
            && html.contains("id=\"claim-code\"")
            && html.contains("passkey-claim"),
        "{html}"
    );
    assert!(!html.contains(&code), "the page never carries a code");

    // A wrong code makes nothing.
    let wrong = world
        .post(
            "/claim",
            None,
            json!({ "code": "0000-0000-0000-0000-0000-0000" }),
        )
        .await;
    assert_eq!(wrong.status(), 403);
    assert_eq!(wrong.json::<Value>().await.unwrap()["error"], "wrong_code");
    assert!(world.accounts.owner().await.unwrap().is_none());

    // The right one, typed back in lower case: the owner, named, and their passkey; the
    // claim is on the audit chain.
    let before = world.audit.verify().unwrap().entries;
    let (session, account) = world.claim(&code.to_lowercase(), Some("Jesse")).await;
    let after = world.audit.verify().unwrap();
    assert!(after.ok && after.entries == before + 1, "{after:?}");
    assert!(
        world.config.is_owner(&account),
        "the hub learns its owner at once"
    );
    let owner = world.accounts.owner().await.unwrap().unwrap();
    assert_eq!(
        (owner.id.as_str(), owner.name.as_str()),
        (account.as_str(), "Jesse")
    );
    let html = world.text("/account", Some(&session)).await;
    assert!(
        html.contains("Signed in as Jesse") && html.contains("You own this hub"),
        "{html}"
    );

    // Claimed: the code is spent for good, the claim page points to sign-in, and the
    // landing page says where AIs connect.
    let again = world.post("/claim", None, json!({ "code": code })).await;
    assert_eq!(again.status(), 409);
    assert_eq!(again.json::<Value>().await.unwrap()["error"], "claimed");
    assert!(
        world
            .text("/claim", None)
            .await
            .contains("This hub has an owner")
    );
    let landing = world.text("/", None).await;
    assert!(
        landing.contains("/auth/login?return_to=/account")
            && landing.contains(&format!("<code>{}/mcp</code>", world.url))
            && !landing.contains("/claim"),
        "{landing}"
    );
    let home = world.get("/", Some(&session)).await;
    assert_eq!(home.status(), 303);
    assert_eq!(home.headers()[header::LOCATION], "/account");
}

#[tokio::test]
async fn a_cancelled_passkey_prompt_is_retried_with_the_same_code() {
    let world = world().await;
    let code = world.printed().await;
    // The first press claimed, and the person cancelled the browser's prompt.
    let first = world.post("/claim", None, json!({ "code": code })).await;
    assert_eq!(first.status(), 200);
    let first: Value = first.json().await.unwrap();
    assert_eq!(
        first["user"]["displayName"], "Owner",
        "an empty name is the default"
    );
    // The second press claims again, with a fresh link; the first one is gone.
    let (_, account) = world.claim(&code, Some("  ")).await;
    assert_eq!(world.accounts.owner().await.unwrap().unwrap().id, account);
    let stale = world
        .post(
            "/passkey/register/options",
            None,
            json!({ "link": first["link"] }),
        )
        .await;
    assert_eq!(stale.status(), 400);
}

#[tokio::test]
async fn the_claim_answers_only_the_hubs_own_page() {
    let world = world().await;
    let code = world.printed().await;
    for (origin, site) in [
        (Some("https://evil.example"), None),
        (None, None),
        (Some(world.url.as_str()), Some("cross-site")),
    ] {
        let mut request = world
            .http
            .post(format!("{}/claim", world.url))
            .json(&json!({ "code": code }));
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        if let Some(site) = site {
            request = request.header("sec-fetch-site", site);
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), 403, "{origin:?} {site:?}");
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"],
            "not_same_origin"
        );
    }
    // A form post is not JSON.
    let form = world
        .http
        .post(format!("{}/claim", world.url))
        .header(header::ORIGIN, &world.url)
        .form(&[("code", code.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(form.status(), 415);
    assert!(
        world.accounts.owner().await.unwrap().is_none(),
        "nothing was made"
    );
    // A name the hub refuses changes nothing either.
    let refused = world
        .post("/claim", None, json!({ "code": code, "name": "a\nb" }))
        .await;
    assert_eq!(refused.status(), 400);
    assert_eq!(
        refused.json::<Value>().await.unwrap()["error"],
        "invalid_name"
    );
    assert!(world.accounts.owner().await.unwrap().is_none());
}

#[tokio::test]
async fn the_docs_say_how_to_connect_to_this_hub() {
    let world = world().await;
    let docs = world.text("/docs", None).await;
    assert!(
        docs.contains(&format!("<code>{}/mcp</code>", world.url)),
        "{docs}"
    );
    assert!(docs.contains("leave the OAuth fields empty"), "{docs}");
    assert!(
        !docs.contains("app.devagotchi.com") && !docs.contains("client ID"),
        "{docs}"
    );
}
