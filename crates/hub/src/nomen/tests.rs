//! Against a fake Nomen that checks the hub's private-key JWT the way Nomen does.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU16, AtomicUsize, Ordering::SeqCst},
    },
};

use axum::{
    Form, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use jsonwebtoken::{DecodingKey, Validation, decode, decode_header};
use serde_json::{Value, json};

use super::*;
use crate::test_keys::{self, TestKey};

const OWNER: &str = "300000000000000001";
const HUB_PROJECT: &str = "hub-project";
const CLIENT_ID: &str = "devagotchi-hub@shippin";

struct Fake {
    issuer: String,
    n: String,
    e: String,
    answers: HashMap<String, Value>,
    calls: AtomicUsize,
    status: AtomicU16,
}

async fn introspect(
    State(fake): State<Arc<Fake>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    fake.calls.fetch_add(1, SeqCst);
    let status = fake.status.load(SeqCst);
    if status != 200 {
        return StatusCode::from_u16(status).unwrap().into_response();
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
        Ok(data) if data.claims["sub"] == CLIENT_ID && kid.as_deref() == Some("test-key") => {}
        _ => return refused.into_response(),
    }
    let token = form.get("token").cloned().unwrap_or_default();
    axum::Json(
        fake.answers
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
        answers: answers(&issuer),
        issuer,
        n: trusted.n.clone(),
        e: trusted.e.clone(),
        calls: AtomicUsize::new(0),
        status: AtomicU16::new(200),
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
    NomenVerifier::from_key(issuer, &[HUB_PROJECT.to_string()], &key_json(key)).unwrap()
}

fn active(issuer: &str, sub: &str, aud: Value) -> Value {
    json!({"active": true, "iss": issuer, "aud": aud, "sub": sub, "exp": unix_now() + 3600,
           "username": "jessay@example.test", "client_id": "shippin-mcp", "token_type": "Bearer"})
}

#[tokio::test]
async fn a_live_token_for_this_hub_signs_in_and_is_remembered() {
    let key = test_keys::rsa();
    let fake = fake_nomen(&key, |iss| {
        HashMap::from([(
            "tok-owner".into(),
            active(iss, OWNER, json!([HUB_PROJECT, "zitadel"])),
        )])
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
            }
        );
    }
    assert_eq!(
        fake.calls.load(SeqCst),
        1,
        "Nomen is asked once per token per minute"
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
    fake.status.store(200, SeqCst);
    assert_eq!(
        nomen.verify("tok-owner").await.unwrap().id,
        OWNER,
        "an outage is never remembered"
    );
    assert_eq!(fake.calls.load(SeqCst), 5);
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
async fn redirects_and_odd_tokens_never_reach_anyone() {
    let key = test_keys::rsa();
    let fake = fake_nomen(&key, |_| HashMap::new()).await;
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

    fake.status.store(307, SeqCst);
    assert!(matches!(
        nomen.verify("tok-x").await,
        Err(AuthError::Unavailable(_))
    ));
}

#[test]
fn every_check_on_the_answer_is_the_hubs_own() {
    let issuer = "https://id.shippin.cloud";
    let accepted = [HUB_PROJECT.to_string()];
    let now = unix_now();
    let base = || Introspection {
        active: true,
        iss: Some(format!("{issuer}/")),
        aud: Some(Audience::One(HUB_PROJECT.into())),
        exp: Some(now + 60),
        sub: Some(OWNER.into()),
        username: Some("jessay@gmail.com".into()),
    };
    assert_eq!(assess(&base(), issuer, &accepted, now).unwrap().id, OWNER);

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
            sub: Some("jessay@gmail.com".into()),
            ..base()
        },
    ];
    for answer in &refused {
        assert!(
            assess(answer, issuer, &accepted, now).is_err(),
            "{answer:?}"
        );
    }

    let odd_name = Introspection {
        username: Some("jesse\u{7}".into()),
        ..base()
    };
    assert_eq!(
        assess(&odd_name, issuer, &accepted, now).unwrap().name,
        None
    );
}

#[test]
fn key_file_errors_never_echo_the_key() {
    let issuer = "https://id.shippin.cloud";
    let audiences = [HUB_PROJECT.to_string()];
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
        let error = NomenVerifier::from_key(issuer, &audiences, text)
            .err()
            .unwrap();
        assert_eq!(error.code, "invalid_config");
        assert!(error.message.contains(expect), "{}", error.message);
        assert!(!error.message.contains("SECRET"), "{}", error.message);
    }
    let key = test_keys::rsa();
    assert!(NomenVerifier::from_key(issuer, &audiences, &key_json(&key)).is_ok());
}

#[cfg(unix)]
#[test]
fn the_key_file_must_be_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub-key.json");
    std::fs::write(&path, key_json(&test_keys::rsa())).unwrap();
    let audiences = [HUB_PROJECT.to_string()];
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    let error = NomenVerifier::new("https://id.example", &audiences, path.to_str().unwrap())
        .err()
        .unwrap();
    assert!(error.message.contains("0600"), "{}", error.message);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(NomenVerifier::new("https://id.example", &audiences, path.to_str().unwrap()).is_ok());
}
