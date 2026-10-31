//! Against a fake Nomen that checks the hub's private-key JWT the way Nomen does.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering::SeqCst},
    },
};

use axum::{
    Form, Router,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use jsonwebtoken::{DecodingKey, Validation, decode, decode_header};
use serde_json::{Value, json};

use super::*;
use crate::test_keys::{self, TestKey};

const OWNER: &str = "300000000000000001";
/// The org Nomen reports the accounts belong to (`urn:zitadel:iam:user:resourceowner:id`).
const OWNER_ORG: &str = "300000000000000900";
const HUB_PROJECT: &str = "hub-project";
const CLIENT_ID: &str = "devagotchi-hub@shippin";
const RELAY: &str = "shippin-mcp";

struct Fake {
    issuer: String,
    n: String,
    e: String,
    /// What Nomen says about each token; a test can sign a token out by removing it.
    answers: Mutex<HashMap<String, Value>>,
    calls: AtomicUsize,
    status: AtomicU16,
    /// Accept the request and never answer, as a wedged Nomen does.
    hang: AtomicBool,
    redirect_to: Mutex<Option<String>>,
    assertions: Mutex<HashSet<String>>,
}

async fn introspect(
    State(fake): State<Arc<Fake>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    fake.calls.fetch_add(1, SeqCst);
    if fake.hang.load(SeqCst) {
        tokio::time::sleep(Duration::from_secs(120)).await;
    }
    let status = fake.status.load(SeqCst);
    if status != 200 {
        let mut response = StatusCode::from_u16(status).unwrap().into_response();
        if let Some(to) = fake.redirect_to.lock().unwrap().clone() {
            response
                .headers_mut()
                .insert(header::LOCATION, to.parse().unwrap());
        }
        return response;
    }
    let refused = (
        StatusCode::UNAUTHORIZED,
        axum::Json(json!({"error": "invalid_client"})),
    );
    if form.get("client_assertion_type").map(String::as_str) != Some(JWT_BEARER) {
        return refused.into_response();
    }
    let assertion = form.get("client_assertion").cloned().unwrap_or_default();
    let kid = decode_header(&assertion).ok().and_then(|h| h.kid);
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_audience(&[fake.issuer.as_str()]);
    validation.set_issuer(&[CLIENT_ID]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    let key = DecodingKey::from_rsa_components(&fake.n, &fake.e).unwrap();
    match decode::<Value>(&assertion, &key, &validation) {
        Ok(data)
            if data.claims["sub"] == CLIENT_ID
                && kid.as_deref() == Some("test-key")
                && data.claims["iat"].as_u64() < Some(unix_now()) => {}
        _ => return refused.into_response(),
    }
    fake.assertions.lock().unwrap().insert(assertion);
    let token = form.get("token").cloned().unwrap_or_default();
    axum::Json(
        fake.answers
            .lock()
            .unwrap()
            .get(&token)
            .cloned()
            .unwrap_or(json!({"active": false})),
    )
    .into_response()
}

/// Start a fake Nomen that trusts `trusted`; `answers` gets the issuer URL.
async fn fake_nomen(
    trusted: &TestKey,
    answers: impl FnOnce(&str) -> HashMap<String, Value>,
) -> Arc<Fake> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let fake = Arc::new(Fake {
        answers: Mutex::new(answers(&issuer)),
        issuer,
        n: trusted.n.clone(),
        e: trusted.e.clone(),
        calls: AtomicUsize::new(0),
        status: AtomicU16::new(200),
        hang: AtomicBool::new(false),
        redirect_to: Mutex::new(None),
        assertions: Mutex::new(HashSet::new()),
    });
    let app = Router::new()
        .route("/oauth/v2/introspect", post(introspect))
        .with_state(fake.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    fake
}

fn key_json(key: &TestKey) -> String {
    json!({"type": "application", "keyId": "test-key", "key": key.pem, "appId": "1", "clientId": CLIENT_ID})
        .to_string()
}

fn verifier(issuer: &str, key: &TestKey) -> NomenVerifier {
    NomenVerifier::from_key(
        issuer,
        &[HUB_PROJECT.to_string()],
        &[RELAY.to_string()],
        &key_json(key),
    )
    .unwrap()
}

fn active(issuer: &str, sub: &str, aud: Value) -> Value {
    json!({"active": true, "iss": issuer, "aud": aud, "sub": sub, "exp": unix_now() + 3600,
           "username": "jessay@example.test", "client_id": RELAY, "token_type": "Bearer",
           "urn:zitadel:iam:user:resourceowner:id": OWNER_ORG})
}

fn with(mut answer: Value, field: &str, value: Value) -> Value {
    answer[field] = value;
    answer
}

#[tokio::test]
async fn a_live_token_for_this_hub_signs_in_and_is_remembered() {
    let key = test_keys::rsa();
    let fake = fake_nomen(&key, |iss| {
        HashMap::from([
            (
                "tok-owner".into(),
                active(iss, OWNER, json!([HUB_PROJECT, "zitadel"])),
            ),
            (
                "tok-other".into(),
                active(iss, "300000000000000002", json!(HUB_PROJECT)),
            ),
        ])
    })
    .await;
    let nomen = verifier(&fake.issuer, &key);
    for _ in 0..3 {
        let subject = nomen.verify("tok-owner").await.unwrap();
        assert_eq!(
            subject,
            Subject {
                id: OWNER.into(),
                via: "nomen",
                name: Some("jessay@example.test".into()),
                org: Some(OWNER_ORG.into()),
                provisional_until: None,
                agent: None,
                claim_code: None,
                provisional_warning: false,
            }
        );
    }
    assert_eq!(
        fake.calls.load(SeqCst),
        1,
        "Nomen is asked once per token per minute"
    );
    nomen.verify("tok-other").await.unwrap();
    assert_eq!(
        fake.assertions.lock().unwrap().len(),
        1,
        "one signed assertion serves introspections within a minute"
    );
}

#[tokio::test]
async fn refusals_are_remembered_and_outages_are_not() {
    let key = test_keys::rsa();
    let fake = fake_nomen(&key, |iss| {
        HashMap::from([
            ("tok-owner".into(), active(iss, OWNER, json!(HUB_PROJECT))),
            (
                "tok-elsewhere".into(),
                active(iss, OWNER, json!(["another-project"])),
            ),
        ])
    })
    .await;
    let nomen = verifier(&fake.issuer, &key);

    for _ in 0..2 {
        assert!(matches!(
            nomen.verify("tok-elsewhere").await,
            Err(AuthError::Invalid(_))
        ));
        assert!(matches!(
            nomen.verify("tok-unknown").await,
            Err(AuthError::Invalid(_))
        ));
    }
    assert_eq!(
        fake.calls.load(SeqCst),
        2,
        "a refusal is Nomen's answer and is reused"
    );

    fake.status.store(503, SeqCst);
    for _ in 0..2 {
        let outage = nomen.verify("tok-owner").await;
        assert!(
            matches!(outage, Err(AuthError::Unavailable(_))),
            "{outage:?}"
        );
    }
    assert_eq!(
        fake.calls.load(SeqCst),
        3,
        "once Nomen has failed, the next caller is refused at once instead of waiting on it"
    );
    fake.status.store(200, SeqCst);
    tokio::time::sleep(RETRY_FIRST + Duration::from_millis(100)).await;
    assert_eq!(
        nomen.verify("tok-owner").await.unwrap().id,
        OWNER,
        "an outage is never remembered"
    );
    assert_eq!(fake.calls.load(SeqCst), 4);
    assert!(nomen.health().reachable);
}

#[tokio::test]
async fn a_confirmed_sign_in_outlives_an_outage_and_an_unknown_one_waits() {
    let key = test_keys::rsa();
    let fake = fake_nomen(&key, |iss| {
        let owner = active(iss, OWNER, json!(HUB_PROJECT));
        HashMap::from([
            ("tok-owner".into(), owner.clone()),
            (
                "tok-brief".into(),
                with(owner, "exp", json!(unix_now() + 2)),
            ),
        ])
    })
    .await;
    let mut nomen = verifier(&fake.issuer, &key);
    nomen.fresh_for = Duration::from_millis(100);
    nomen.retry = (Duration::from_millis(200), Duration::from_millis(400));
    nomen.verify("tok-owner").await.unwrap();
    nomen.verify("tok-brief").await.unwrap();
    assert!(nomen.health().reachable);

    fake.status.store(503, SeqCst);
    tokio::time::sleep(Duration::from_millis(150)).await;
    // Past its minute, Nomen is asked again, fails, and the earlier confirmation stands.
    assert_eq!(nomen.verify("tok-owner").await.unwrap().id, OWNER);
    let health = nomen.health();
    assert!(
        !health.reachable && health.down_since.is_some(),
        "{health:?}"
    );
    assert!(
        health
            .last_error
            .as_deref()
            .is_some_and(|why| why.contains("503")),
        "{health:?}"
    );
    // Further requests are answered from it without waiting on Nomen.
    let calls = fake.calls.load(SeqCst);
    for _ in 0..5 {
        assert_eq!(nomen.verify("tok-owner").await.unwrap().id, OWNER);
    }
    assert_eq!(
        fake.calls.load(SeqCst),
        calls,
        "no request waits on a Nomen known to be down"
    );
    assert!(nomen.health().honoured_during_outage >= 6);
    // A token Nomen never confirmed is refused as an outage, never as a sign-out.
    assert!(matches!(
        nomen.verify("tok-never-seen").await,
        Err(AuthError::Unavailable(_))
    ));
    // A confirmation is not honoured past its own token's expiry.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(matches!(
        nomen.verify("tok-brief").await,
        Err(AuthError::Unavailable(_))
    ));

    fake.status.store(200, SeqCst);
    tokio::time::sleep(nomen.retry.1).await;
    assert_eq!(
        nomen.verify("tok-never-seen").await.ok(),
        None,
        "Nomen answers: inactive"
    );
    assert!(nomen.health().reachable, "the first answer ends the outage");
}

#[tokio::test]
async fn confirmed_sign_ins_survive_a_restart_during_an_outage_and_a_sign_out_ends_them() {
    let key = test_keys::rsa();
    let fake = fake_nomen(&key, |iss| {
        let owner = active(iss, OWNER, json!(HUB_PROJECT));
        HashMap::from([
            ("tok-owner".into(), owner.clone()),
            ("tok-signed-out".into(), owner.clone()),
            (
                "tok-brief".into(),
                with(owner, "exp", json!(unix_now() + 2)),
            ),
        ])
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("signins.sqlite3");
    let first = verifier(&fake.issuer, &key).remembering(&store);
    for token in ["tok-owner", "tok-signed-out", "tok-brief"] {
        first.verify(token).await.unwrap();
    }
    drop(first);
    // The store holds hashes of tokens, never a token.
    for file in std::fs::read_dir(dir.path()).unwrap() {
        let bytes = std::fs::read(file.unwrap().path()).unwrap();
        assert!(
            !bytes.windows(9).any(|w| w == b"tok-owner"),
            "a token was written to disk"
        );
    }

    // Nomen goes down and the hub restarts in the middle of it.
    fake.status.store(503, SeqCst);
    tokio::time::sleep(Duration::from_secs(3)).await;
    let mut nomen = verifier(&fake.issuer, &key).remembering(&store);
    nomen.retry = (Duration::from_millis(200), Duration::from_millis(400));
    let calls = fake.calls.load(SeqCst);
    let owner = nomen.verify("tok-owner").await.unwrap();
    assert_eq!(
        (owner.id.as_str(), owner.org.as_deref()),
        (OWNER, Some(OWNER_ORG))
    );
    assert_eq!(owner.name.as_deref(), Some("jessay@example.test"));
    assert_eq!(
        fake.calls.load(SeqCst),
        calls + 1,
        "Nomen is asked first, then the kept sign-in honoured"
    );
    assert!(nomen.health().honoured_during_outage >= 1);
    // Nothing is honoured past its token's own expiry, and nothing never confirmed is.
    assert!(matches!(
        nomen.verify("tok-brief").await,
        Err(AuthError::Unavailable(_))
    ));
    assert!(matches!(
        nomen.verify("tok-never-seen").await,
        Err(AuthError::Unavailable(_))
    ));

    // Nomen returns, having signed one of them out: that one ends, on disk too.
    fake.answers.lock().unwrap().remove("tok-signed-out");
    fake.status.store(200, SeqCst);
    tokio::time::sleep(nomen.retry.1).await;
    assert!(matches!(
        nomen.verify("tok-signed-out").await,
        Err(AuthError::Invalid(_))
    ));
    drop(nomen);
    fake.status.store(503, SeqCst);
    let after = verifier(&fake.issuer, &key).remembering(&store);
    assert!(matches!(
        after.verify("tok-signed-out").await,
        Err(AuthError::Unavailable(_))
    ));
    assert_eq!(after.verify("tok-owner").await.unwrap().id, OWNER);
}

#[tokio::test]
async fn a_nomen_that_accepts_and_never_answers_costs_seconds_once() {
    let key = test_keys::rsa();
    let fake = fake_nomen(&key, |iss| {
        HashMap::from([("tok-owner".into(), active(iss, OWNER, json!(HUB_PROJECT)))])
    })
    .await;
    let nomen = verifier(&fake.issuer, &key);
    fake.hang.store(true, SeqCst);
    let began = std::time::Instant::now();
    assert!(matches!(
        nomen.verify("tok-owner").await,
        Err(AuthError::Unavailable(_))
    ));
    let first = began.elapsed();
    assert!(
        first >= ANSWER_TIMEOUT && first < ANSWER_TIMEOUT + Duration::from_secs(2),
        "{first:?}"
    );
    let began = std::time::Instant::now();
    for _ in 0..10 {
        assert!(matches!(
            nomen.verify("tok-owner").await,
            Err(AuthError::Unavailable(_))
        ));
    }
    assert!(
        began.elapsed() < Duration::from_millis(500),
        "{:?}",
        began.elapsed()
    );
    assert_eq!(fake.calls.load(SeqCst), 1);
}

#[tokio::test]
async fn an_account_id_is_taken_exactly_and_only_from_accepted_clients() {
    let key = test_keys::rsa();
    let fake = fake_nomen(&key, |iss| {
        let owner = active(iss, OWNER, json!(HUB_PROJECT));
        HashMap::from([
            (
                "tok-lead-space".into(),
                with(owner.clone(), "sub", json!(format!(" {OWNER}"))),
            ),
            (
                "tok-tab".into(),
                with(owner.clone(), "sub", json!(format!("{OWNER}\t"))),
            ),
            (
                "tok-newline".into(),
                with(owner.clone(), "sub", json!(format!("{OWNER}\n"))),
            ),
            (
                "tok-tenant-app".into(),
                with(owner.clone(), "client_id", json!("tenant-app")),
            ),
            (
                "tok-no-client".into(),
                with(owner.clone(), "client_id", Value::Null),
            ),
            ("tok-owner".into(), owner),
        ])
    })
    .await;
    let nomen = verifier(&fake.issuer, &key);
    for token in [
        "tok-lead-space",
        "tok-tab",
        "tok-newline",
        "tok-tenant-app",
        "tok-no-client",
    ] {
        assert!(
            matches!(nomen.verify(token).await, Err(AuthError::Invalid(_))),
            "{token}"
        );
    }
    assert_eq!(nomen.verify("tok-owner").await.unwrap().id, OWNER);
}

#[tokio::test]
async fn remembering_follows_the_tokens_own_expiry() {
    let key = test_keys::rsa();
    let fake = fake_nomen(&key, |iss| {
        let owner = active(iss, OWNER, json!(HUB_PROJECT));
        HashMap::from([
            (
                "tok-short".into(),
                with(owner.clone(), "exp", json!(unix_now() + 1)),
            ),
            (
                "tok-leeway".into(),
                with(owner, "exp", json!(unix_now() - 10)),
            ),
        ])
    })
    .await;
    let nomen = verifier(&fake.issuer, &key);
    // Inside the leeway: accepted, but never remembered past its own expiry.
    nomen.verify("tok-leeway").await.unwrap();
    nomen.verify("tok-leeway").await.unwrap();
    assert_eq!(fake.calls.load(SeqCst), 2);
    // Expiring in a second: remembered no longer than that.
    nomen.verify("tok-short").await.unwrap();
    tokio::time::sleep(Duration::from_millis(2100)).await;
    let _ = nomen.verify("tok-short").await;
    assert_eq!(fake.calls.load(SeqCst), 4);
}

#[tokio::test]
async fn nomen_refusing_the_hubs_own_key_is_an_outage_not_a_sign_out() {
    let trusted = test_keys::rsa();
    let fake = fake_nomen(&trusted, |iss| {
        HashMap::from([("tok-owner".into(), active(iss, OWNER, json!(HUB_PROJECT)))])
    })
    .await;
    let wrong_key = verifier(&fake.issuer, &test_keys::rsa());
    match wrong_key.verify("tok-owner").await {
        Err(AuthError::Unavailable(why)) => assert!(why.contains("credentials"), "{why}"),
        other => panic!("expected issuer_unavailable, got {other:?}"),
    }
}

#[tokio::test]
async fn redirects_oversized_answers_and_odd_tokens_reach_no_one() {
    let key = test_keys::rsa();
    let elsewhere = fake_nomen(&key, |_| HashMap::new()).await;
    let fake = fake_nomen(&key, |iss| {
        HashMap::from([(
            "tok-huge".into(),
            with(
                active(iss, OWNER, json!(HUB_PROJECT)),
                "padding",
                json!("x".repeat(MAX_ANSWER_BYTES)),
            ),
        )])
    })
    .await;
    let nomen = verifier(&fake.issuer, &key);
    assert!(matches!(
        nomen.verify("has space").await,
        Err(AuthError::Invalid(_))
    ));
    assert!(matches!(
        nomen.verify("tab\there").await,
        Err(AuthError::Invalid(_))
    ));
    assert_eq!(fake.calls.load(SeqCst), 0);

    assert!(matches!(
        nomen.verify("tok-huge").await,
        Err(AuthError::Unavailable(_))
    ));

    *fake.redirect_to.lock().unwrap() = Some(format!("{}/oauth/v2/introspect", elsewhere.issuer));
    fake.status.store(307, SeqCst);
    assert!(matches!(
        nomen.verify("tok-x").await,
        Err(AuthError::Unavailable(_))
    ));
    assert_eq!(
        elsewhere.calls.load(SeqCst),
        0,
        "the redirect was not followed"
    );
}

#[test]
fn a_full_cache_never_evicts_a_confirmed_sign_in_and_late_answers_never_win() {
    let nomen = NomenVerifier::from_key(
        "https://id.example",
        &[HUB_PROJECT.to_string()],
        &[RELAY.to_string()],
        &key_json(&test_keys::rsa()),
    )
    .unwrap();
    let owner = || {
        Ok(Subject {
            id: OWNER.into(),
            via: "nomen",
            name: None,
            org: None,
            provisional_until: None,
            agent: None,
            claim_code: None,
            provisional_warning: false,
        })
    };
    let digest = |i: usize| -> [u8; 32] { Sha256::digest(i.to_le_bytes()).into() };
    let asked = Instant::now();
    nomen.remember(digest(0), owner(), asked, CACHE_TTL, None);
    for i in 1..CACHE_MAX * 2 {
        nomen.remember(digest(i), Err("inactive"), asked, CACHE_TTL, None);
    }
    assert!(
        nomen.recall(&digest(0)).unwrap().is_ok(),
        "confirmed sign-in survives a flood"
    );
    assert!(nomen.cache.lock().unwrap().len() <= CACHE_MAX);

    // An answer to an earlier question never replaces a newer one.
    let token = digest(usize::MAX);
    let later = Instant::now();
    nomen.remember(token, Err("signed out"), later, CACHE_TTL, None);
    nomen.remember(
        token,
        owner(),
        later - Duration::from_secs(5),
        CACHE_TTL,
        None,
    );
    assert_eq!(nomen.recall(&token), Some(Err("signed out")));
}

#[test]
fn every_check_on_the_answer_is_the_hubs_own() {
    let accepts = Accepts {
        issuer: "https://id.shippin.cloud".into(),
        audiences: vec![HUB_PROJECT.into()],
        clients: vec![RELAY.into()],
    };
    let now = unix_now();
    let base = || Introspection {
        active: true,
        iss: Some("https://id.shippin.cloud/".into()),
        aud: Some(Audience::One(HUB_PROJECT.into())),
        client_id: Some(RELAY.into()),
        exp: Some(now + 60),
        sub: Some(OWNER.into()),
        username: Some("jessay@gmail.com".into()),
        org: Some(OWNER_ORG.into()),
    };
    assert_eq!(assess(&base(), &accepts, now).unwrap().id, OWNER);
    assert_eq!(
        assess(&base(), &accepts, now).unwrap().org.as_deref(),
        Some(OWNER_ORG),
        "the org Nomen proved comes through"
    );
    for (org, kept) in [
        (None, None),
        (Some("../etc"), None),
        (Some(""), None),
        (Some("org with spaces"), None),
        (Some(" 395263680805142530 "), Some("395263680805142530")),
    ] {
        let answer = Introspection {
            org: org.map(str::to_string),
            ..base()
        };
        assert_eq!(
            assess(&answer, &accepts, now).unwrap().org.as_deref(),
            kept,
            "{org:?} is not a usable org"
        );
    }

    let refused = [
        Introspection {
            active: false,
            ..base()
        },
        Introspection {
            iss: Some("https://evil.example".into()),
            ..base()
        },
        Introspection {
            iss: None,
            ..base()
        },
        Introspection {
            aud: Some(Audience::Many(vec!["other".into()])),
            ..base()
        },
        Introspection {
            aud: None,
            ..base()
        },
        Introspection {
            client_id: Some("tenant-app".into()),
            ..base()
        },
        Introspection {
            client_id: None,
            ..base()
        },
        Introspection {
            exp: Some(now - LEEWAY_SECS - 1),
            ..base()
        },
        Introspection {
            exp: None,
            ..base()
        },
        Introspection {
            sub: None,
            ..base()
        },
        Introspection {
            sub: Some("  ".into()),
            ..base()
        },
        Introspection {
            sub: Some(format!(" {OWNER}")),
            ..base()
        },
        Introspection {
            sub: Some(format!("{OWNER}\u{0}")),
            ..base()
        },
        Introspection {
            sub: Some("jessay@gmail.com".into()),
            ..base()
        },
    ];
    for answer in &refused {
        assert!(assess(answer, &accepts, now).is_err(), "{answer:?}");
    }

    let odd_name = Introspection {
        username: Some("jesse\u{7}".into()),
        ..base()
    };
    assert_eq!(assess(&odd_name, &accepts, now).unwrap().name, None);
}

#[test]
fn key_files_load_in_either_pem_form_and_errors_never_echo_the_key() {
    let issuer = "https://id.shippin.cloud";
    let audiences = [HUB_PROJECT.to_string()];
    let clients = [RELAY.to_string()];
    let secret = r#"{"type":"application","keyId":"k","key":"SECRET-MATERIAL","clientId":"c"}"#;
    for (text, expect) in [
        ("SECRET-MATERIAL not json", "not a Nomen key file"),
        (secret, "not an RSA private key"),
        (
            r#"{"type":"serviceaccount","keyId":"k","key":"SECRET-MATERIAL","clientId":"c"}"#,
            "API application",
        ),
        (
            r#"{"type":"application","keyId":"","key":"SECRET-MATERIAL","clientId":"c"}"#,
            "keyId",
        ),
    ] {
        let error = NomenVerifier::from_key(issuer, &audiences, &clients, text)
            .err()
            .unwrap();
        assert_eq!(error.code, "invalid_config");
        assert!(error.message.contains(expect), "{}", error.message);
        assert!(!error.message.contains("SECRET"), "{}", error.message);
    }
    let key = test_keys::rsa();
    assert!(key.pem.starts_with("-----BEGIN RSA PRIVATE KEY-----"));
    assert!(NomenVerifier::from_key(issuer, &audiences, &clients, &key_json(&key)).is_ok());
    let pkcs8 = TestKey {
        pem: key.pkcs8_pem.clone(),
        pkcs8_pem: String::new(),
        n: String::new(),
        e: String::new(),
    };
    assert!(NomenVerifier::from_key(issuer, &audiences, &clients, &key_json(&pkcs8)).is_ok());
}

#[cfg(unix)]
#[test]
fn the_key_file_must_be_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub-key.json");
    std::fs::write(&path, key_json(&test_keys::rsa())).unwrap();
    let audiences = [HUB_PROJECT.to_string()];
    let clients = [RELAY.to_string()];
    let load = || {
        NomenVerifier::new(
            "https://id.example",
            &audiences,
            &clients,
            path.to_str().unwrap(),
        )
    };
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    let error = load().err().unwrap();
    assert!(error.message.contains("0600"), "{}", error.message);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(load().is_ok());
}
