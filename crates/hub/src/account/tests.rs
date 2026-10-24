//! Against a fake Nomen that plays the device-authorization grant the way Zitadel does.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering::SeqCst},
    },
};

use axum::{
    Form, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};

use super::*;

const SUBJECT: &str = "395290028818825218";

struct Fake {
    issuer: Mutex<String>,
    /// Token-endpoint answers for the device grant, in order; the last repeats.
    device_answers: Vec<(StatusCode, Value)>,
    polls: AtomicUsize,
    revoked: Mutex<Vec<String>>,
    refreshes: AtomicUsize,
}

fn fake(device_answers: Vec<(StatusCode, Value)>) -> Arc<Fake> {
    Arc::new(Fake {
        issuer: Mutex::new(String::new()),
        device_answers,
        polls: AtomicUsize::new(0),
        revoked: Mutex::new(Vec::new()),
        refreshes: AtomicUsize::new(0),
    })
}

async fn device_authorization(
    State(fake): State<Arc<Fake>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    assert_eq!(
        form.get("client_id").map(String::as_str),
        Some(DEFAULT_CLIENT_ID)
    );
    assert_eq!(form.get("scope").map(String::as_str), Some(SCOPE));
    let issuer = fake.issuer.lock().unwrap().clone();
    axum::Json(json!({
        "device_code": "dc-1",
        "user_code": "ABCD-EFGH",
        "verification_uri": format!("{issuer}/device"),
        "verification_uri_complete": format!("{issuer}/device?user_code=ABCD-EFGH"),
        "expires_in": 600,
        "interval": 0,
    }))
    .into_response()
}

async fn token(
    State(fake): State<Arc<Fake>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let field = |key: &str| form.get(key).map(String::as_str).unwrap_or("");
    assert_eq!(field("client_id"), DEFAULT_CLIENT_ID);
    match field("grant_type") {
        DEVICE_GRANT => {
            assert_eq!(field("device_code"), "dc-1");
            let n = fake.polls.fetch_add(1, SeqCst);
            let (status, body) = fake.device_answers[n.min(fake.device_answers.len() - 1)].clone();
            (status, axum::Json(body)).into_response()
        }
        "refresh_token" => {
            fake.refreshes.fetch_add(1, SeqCst);
            if field("refresh_token") != "rt-1" {
                return (
                    StatusCode::BAD_REQUEST,
                    axum::Json(json!({"error": "invalid_grant"})),
                )
                    .into_response();
            }
            axum::Json(json!({
                "access_token": "at-2", "refresh_token": "rt-2", "token_type": "Bearer", "expires_in": 7200
            }))
            .into_response()
        }
        other => panic!("unexpected grant {other}"),
    }
}

async fn userinfo(headers: HeaderMap) -> Response {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if bearer != "Bearer at-1" {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    axum::Json(
        json!({ "sub": SUBJECT, "preferred_username": "hades", "email": "jessay@example.test" }),
    )
    .into_response()
}

async fn revoke(
    State(fake): State<Arc<Fake>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    fake.revoked
        .lock()
        .unwrap()
        .push(form.get("token").cloned().unwrap_or_default());
    StatusCode::OK.into_response()
}

async fn serve(fake: Arc<Fake>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    *fake.issuer.lock().unwrap() = issuer.clone();
    let app = Router::new()
        .route("/oauth/v2/device_authorization", post(device_authorization))
        .route("/oauth/v2/token", post(token))
        .route("/oauth/v2/revoke", post(revoke))
        .route("/oidc/v1/userinfo", get(userinfo))
        .with_state(fake);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    issuer
}

fn pending() -> (StatusCode, Value) {
    (
        StatusCode::BAD_REQUEST,
        json!({"error": "authorization_pending"}),
    )
}

fn granted() -> (StatusCode, Value) {
    (
        StatusCode::OK,
        json!({"access_token": "at-1", "refresh_token": "rt-1", "token_type": "Bearer", "expires_in": 3600}),
    )
}

#[tokio::test]
async fn login_shows_the_code_waits_and_lands_the_account_owner_only() {
    let fake = fake(vec![pending(), granted()]);
    let issuer = serve(fake.clone()).await;
    let nomen = Issuer::new(&issuer, DEFAULT_CLIENT_ID).unwrap();
    let mut told = Vec::new();
    let account = nomen.login(&mut told).await.unwrap();
    let told = String::from_utf8(told).unwrap();
    assert!(told.contains("ABCD-EFGH"), "{told}");
    assert!(told.contains(&format!("{issuer}/device")), "{told}");
    assert_eq!(fake.polls.load(SeqCst), 2, "pending once, then granted");
    assert_eq!(account.subject, SUBJECT);
    assert_eq!(account.username.as_deref(), Some("hades"));
    assert_eq!(account.access_token(), "at-1");
    assert_eq!(account.standing(unix_now()), Standing::Fresh);
    assert!(
        !format!("{account:?}").contains("at-1"),
        "Debug never shows a token"
    );

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join(FILE_NAME);
    write(&path, &Stored::Account(account.clone())).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{mode:o}");
    }
    assert_eq!(read(&path).unwrap(), Some(Stored::Account(account.clone())));
    assert!(!path.with_extension("json.tmp").exists());

    // Refresh keeps the identity and takes the new tokens; revoke names both tokens.
    let refreshed = nomen.refresh(&account).await.unwrap();
    assert_eq!(refreshed.subject, SUBJECT);
    assert_eq!(refreshed.access_token(), "at-2");
    assert_eq!(refreshed.refresh_token.as_deref(), Some("rt-2"));
    assert!(refreshed.expires_at > account.expires_at);
    nomen.revoke(&account).await.unwrap();
    assert_eq!(*fake.revoked.lock().unwrap(), vec!["at-1", "rt-1"]);
    assert!(remove(&path).unwrap());
    assert!(!remove(&path).unwrap());
    assert_eq!(read(&path).unwrap(), None);
}

#[tokio::test]
async fn a_declined_or_expired_code_is_a_refusal_in_plain_words() {
    for (answer, expect) in [
        (json!({"error": "access_denied"}), "declined"),
        (json!({"error": "expired_token"}), "expired"),
        (
            json!({"error": "invalid_client", "error_description": "no such app"}),
            "invalid_client (no such app)",
        ),
    ] {
        let fake = fake(vec![(StatusCode::BAD_REQUEST, answer)]);
        let issuer = serve(fake).await;
        let nomen = Issuer::new(&issuer, DEFAULT_CLIENT_ID).unwrap();
        let why = nomen.login(&mut Vec::new()).await.err().unwrap();
        match why {
            Refusal::Refused(text) => assert!(text.contains(expect), "{text}"),
            other => panic!("{other:?}"),
        }
    }
}

#[tokio::test]
async fn an_unreachable_issuer_is_unavailable_not_refused() {
    // Nothing listens here: bind a port, then drop it.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let nomen = Issuer::new(&issuer, DEFAULT_CLIENT_ID).unwrap();
    assert!(matches!(
        nomen.login(&mut Vec::new()).await,
        Err(Refusal::Unavailable(_))
    ));
    assert!(
        Issuer::new("http://nomen.example", DEFAULT_CLIENT_ID).is_err(),
        "https only"
    );
    assert!(Issuer::new(DEFAULT_ISSUER, " ").is_err());
}

#[tokio::test]
async fn current_refreshes_when_due_and_keeps_the_file_otherwise() {
    let fake = fake(vec![granted()]);
    let issuer = serve(fake.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE_NAME);
    assert_eq!(current(&path).await.unwrap(), None);

    let now = unix_now();
    let account = Account {
        issuer: issuer.clone(),
        client_id: DEFAULT_CLIENT_ID.into(),
        subject: SUBJECT.into(),
        username: Some("hades".into()),
        email: None,
        access_token: "at-1".into(),
        refresh_token: Some("rt-1".into()),
        expires_at: now + 60, // within REFRESH_AHEAD
        signed_in_at: now - 3600,
    };
    assert_eq!(account.standing(now), Standing::Refresh);
    write(&path, &Stored::Account(account.clone())).unwrap();
    let Some(Stored::Account(after)) = current(&path).await.unwrap() else {
        panic!("account expected");
    };
    assert_eq!(after.access_token(), "at-2");
    assert_eq!(fake.refreshes.load(SeqCst), 1);
    assert_eq!(
        read(&path).unwrap(),
        Some(Stored::Account(after.clone())),
        "written back"
    );
    // Fresh now: no second refresh.
    assert_eq!(after.standing(unix_now()), Standing::Fresh);
    current(&path).await.unwrap();
    assert_eq!(fake.refreshes.load(SeqCst), 1);

    // A refresh Nomen refuses leaves the file as it was, and the standing says so.
    let stale = Account {
        refresh_token: Some("rt-gone".into()),
        expires_at: now - 10,
        ..account.clone()
    };
    write(&path, &Stored::Account(stale.clone())).unwrap();
    assert_eq!(
        current(&path).await.unwrap(),
        Some(Stored::Account(stale.clone()))
    );
    assert!(matches!(stale.standing(now), Standing::Offline { .. }));
    assert_eq!(
        stale.standing(now + OFFLINE_GRACE.as_secs() + 11),
        Standing::Expired
    );

    // The local choice is kept as it is.
    write(&path, &Stored::Local { chosen_at: now }).unwrap();
    assert_eq!(
        current(&path).await.unwrap(),
        Some(Stored::Local { chosen_at: now })
    );
    std::fs::write(&path, "not json").unwrap();
    assert!(read(&path).unwrap_err().contains("logout"));
}
