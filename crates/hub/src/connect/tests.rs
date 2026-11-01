//! Against a fake Nomen that plays the device grant and the management API as Nomen does.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering::SeqCst},
    },
};

use axum::{
    Form, Json, Router,
    extract::{Path as UrlPath, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};

use super::*;

const CLI: &str = "300000000000000700";
const PROJECT: &str = "300000000000000500";
const OWNER: &str = "300000000000000001";

/// App name to (app id, client id, redirect URIs).
type Apps = HashMap<String, (String, String, Vec<String>)>;

struct Fake {
    issuer: Mutex<String>,
    subject: Mutex<String>,
    decline: bool,
    apps: Mutex<Apps>,
    created: AtomicUsize,
    keys: AtomicUsize,
}

fn fake(decline: bool) -> Arc<Fake> {
    Arc::new(Fake {
        issuer: Mutex::new(String::new()),
        subject: Mutex::new(OWNER.into()),
        decline,
        apps: Mutex::new(HashMap::new()),
        created: AtomicUsize::new(0),
        keys: AtomicUsize::new(0),
    })
}

fn owner_only(headers: &HeaderMap) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        == Some("Bearer owner-at")
}

async fn device_authorization(
    State(fake): State<Arc<Fake>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    assert_eq!(form.get("client_id").map(String::as_str), Some(CLI));
    // Nomen's own APIs, in Nomen's spelling.
    assert_eq!(
        form.get("scope").map(String::as_str),
        Some("openid profile email offline_access urn:nomen:iam:org:project:id:nomen:aud")
    );
    let issuer = fake.issuer.lock().unwrap().clone();
    Json(json!({
        "device_code": "dc-1", "user_code": "WXYZ-1234",
        "verification_uri": format!("{issuer}/device"), "expires_in": 600, "interval": 0,
    }))
    .into_response()
}

async fn token(
    State(fake): State<Arc<Fake>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    assert_eq!(form.get("client_id").map(String::as_str), Some(CLI));
    if fake.decline {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "access_denied"})),
        )
            .into_response();
    }
    Json(json!({"access_token": "owner-at", "refresh_token": "rt", "token_type": "Bearer", "expires_in": 3600}))
        .into_response()
}

async fn userinfo(State(fake): State<Arc<Fake>>, headers: HeaderMap) -> Response {
    if !owner_only(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({ "sub": fake.subject.lock().unwrap().clone(), "preferred_username": "owner" }))
        .into_response()
}

async fn search(
    State(fake): State<Arc<Fake>>,
    UrlPath(project): UrlPath<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    assert!(owner_only(&headers) && project == PROJECT);
    let name = body["queries"][0]["nameQuery"]["name"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let apps = fake.apps.lock().unwrap();
    let result: Vec<Value> = apps
        .get(&name)
        .map(|(id, client, redirects)| {
            let config = json!({ "clientId": client, "redirectUris": redirects });
            if name == API_APP {
                json!({ "id": id, "name": name, "apiConfig": config })
            } else {
                json!({ "id": id, "name": name, "oidcConfig": config })
            }
        })
        .into_iter()
        .collect();
    Json(json!({ "result": result })).into_response()
}

fn create(fake: &Fake, name: &str, redirects: Vec<String>) -> Response {
    let n = fake.created.fetch_add(1, SeqCst);
    let (id, client) = (
        format!("40000000000000000{n}"),
        format!("50000000000000000{n}"),
    );
    fake.apps
        .lock()
        .unwrap()
        .insert(name.into(), (id.clone(), client.clone(), redirects));
    Json(json!({ "appId": id, "clientId": client })).into_response()
}

async fn create_api(
    State(fake): State<Arc<Fake>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    assert!(owner_only(&headers));
    assert_eq!(
        body["authMethodType"],
        "API_AUTH_METHOD_TYPE_PRIVATE_KEY_JWT"
    );
    create(&fake, body["name"].as_str().unwrap(), Vec::new())
}

async fn create_oidc(
    State(fake): State<Arc<Fake>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    assert!(owner_only(&headers));
    let expected = if body["name"] == AI_APP {
        "OIDC_APP_TYPE_NATIVE"
    } else {
        "OIDC_APP_TYPE_USER_AGENT"
    };
    assert_eq!(body["appType"], expected);
    assert_eq!(
        body["authMethodType"], "OIDC_AUTH_METHOD_TYPE_NONE",
        "public apps only"
    );
    let redirects = body["redirectUris"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    create(&fake, body["name"].as_str().unwrap(), redirects)
}

async fn new_key(
    State(fake): State<Arc<Fake>>,
    UrlPath((_project, app)): UrlPath<(String, String)>,
    headers: HeaderMap,
) -> Response {
    assert!(owner_only(&headers));
    let n = fake.keys.fetch_add(1, SeqCst);
    let client = fake
        .apps
        .lock()
        .unwrap()
        .values()
        .find(|(id, ..)| *id == app)
        .unwrap()
        .1
        .clone();
    let details = json!({ "type": "application", "keyId": format!("k{n}"), "key": "-----BEGIN RSA PRIVATE KEY-----\ntest\n-----END RSA PRIVATE KEY-----\n", "appId": app, "clientId": client });
    let encoded = base64::engine::general_purpose::STANDARD.encode(details.to_string());
    Json(json!({ "id": format!("k{n}"), "keyDetails": encoded })).into_response()
}

async fn serve(fake: Arc<Fake>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    *fake.issuer.lock().unwrap() = issuer.clone();
    let app = Router::new()
        .route("/oauth/v2/device_authorization", post(device_authorization))
        .route("/oauth/v2/token", post(token))
        .route("/oidc/v1/userinfo", get(userinfo))
        .route(
            "/management/v1/projects/{project}/apps/_search",
            post(search),
        )
        .route(
            "/management/v1/projects/{project}/apps/api",
            post(create_api),
        )
        .route(
            "/management/v1/projects/{project}/apps/oidc",
            post(create_oidc),
        )
        .route(
            "/management/v1/projects/{project}/apps/{app}/keys",
            post(new_key),
        )
        .with_state(fake);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    issuer
}

/// A hub that was connected to an earlier Nomen (another project, owner and clients).
fn old_config(dir: &Path) -> PathBuf {
    let path = dir.join("hub.json");
    let config = json!({
        "listen": "127.0.0.1:8787",
        "public_url": "https://hub.example",
        "auth": { "mode": "nomen", "issuer": "https://id.shippin.cloud", "audiences": ["396558202386251780"],
                  "clients": ["396558202386251999"], "key_file": dir.join("old-key.json") },
        "owner": "396558202386250001",
        "brokers": [{ "name": "tx-infra-prod", "link": true }],
        "grants": [{ "subject": "396558202386250002", "brokers": ["tx-infra-prod"] }],
    });
    std::fs::write(&path, serde_json::to_string_pretty(&config).unwrap()).unwrap();
    path
}

fn connect(dir: &Path, config: PathBuf, issuer: &str) -> Connect {
    Connect {
        state: dir.join("state"),
        config,
        issuer: issuer.into(),
        client_id: CLI.into(),
        project: PROJECT.into(),
    }
}

fn read(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[tokio::test]
async fn connects_a_hub_with_the_owners_approval_and_no_copied_key() {
    let dir = tempfile::tempdir().unwrap();
    let config = old_config(dir.path());
    let before = std::fs::read_to_string(&config).unwrap();
    let fake = fake(false);
    let issuer = serve(fake.clone()).await;
    let mut told = Vec::new();
    run(&connect(dir.path(), config.clone(), &issuer), &mut told)
        .await
        .unwrap();

    let doc = read(&config);
    let (api_client, web_client, ai_client) = {
        let apps = fake.apps.lock().unwrap();
        (
            apps[API_APP].1.clone(),
            apps[WEB_APP].1.clone(),
            apps[AI_APP].1.clone(),
        )
    };
    assert_eq!(doc["auth"]["mode"], "nomen");
    assert_eq!(doc["auth"]["issuer"], issuer.as_str());
    assert_eq!(doc["auth"]["audiences"], json!([PROJECT]));
    // The earlier Nomen's clients mean nothing to this one.
    assert_eq!(doc["auth"]["clients"], json!([CLI, web_client, ai_client]));
    assert_eq!(
        fake.apps.lock().unwrap()[AI_APP].2,
        AI_REDIRECTS.map(String::from).to_vec()
    );
    assert_eq!(doc["owner"], OWNER);
    assert_eq!(doc["web"]["client_id"], web_client.as_str());
    assert_eq!(doc["brokers"][0]["name"], "tx-infra-prod");
    assert_eq!(
        fake.apps.lock().unwrap()[WEB_APP].2,
        vec!["https://hub.example/auth/callback".to_string()]
    );
    HubConfig::parse(&std::fs::read_to_string(&config).unwrap()).unwrap();

    let key_path = dir.path().join("state").join(KEY_FILE);
    assert_eq!(doc["auth"]["key_file"], key_path.to_str().unwrap());
    assert_eq!(read(&key_path)["clientId"], api_client.as_str());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&key_path, &config] {
            let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{}", path.display());
        }
    }
    let backups: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .contains("before-connect-nomen")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(std::fs::read_to_string(backups[0].path()).unwrap(), before);
    let told = String::from_utf8(told).unwrap();
    assert!(
        told.contains("WXYZ-1234") && told.contains("systemctl --user restart devagotchi-hub"),
        "{told}"
    );
    assert!(
        !told.contains("BEGIN RSA PRIVATE KEY"),
        "the key is never printed"
    );
    assert!(
        told.contains(&format!("OAuth client id {ai_client}")),
        "{told}"
    );
}

#[tokio::test]
async fn a_second_run_reuses_the_apps_adds_a_key_and_keeps_listed_clients() {
    let dir = tempfile::tempdir().unwrap();
    let config = old_config(dir.path());
    let fake = fake(false);
    let issuer = serve(fake.clone()).await;
    let c = connect(dir.path(), config.clone(), &issuer);
    run(&c, &mut Vec::new()).await.unwrap();
    let mut doc = read(&config);
    doc["auth"]["clients"]
        .as_array_mut()
        .unwrap()
        .push(json!("300000000000000900"));
    std::fs::write(&config, doc.to_string()).unwrap();

    run(&c, &mut Vec::new()).await.unwrap();
    assert_eq!(fake.created.load(SeqCst), 3, "the apps are made once");
    assert_eq!(fake.keys.load(SeqCst), 2, "each run gets a fresh key");
    let clients = read(&config)["auth"]["clients"].clone();
    assert!(
        clients
            .as_array()
            .unwrap()
            .contains(&json!("300000000000000900")),
        "{clients}"
    );
    assert_eq!(
        read(&dir.path().join("state").join(KEY_FILE))["keyId"],
        "k1"
    );
}

#[tokio::test]
async fn another_person_cannot_take_a_connected_hub() {
    let dir = tempfile::tempdir().unwrap();
    let config = old_config(dir.path());
    let fake = fake(false);
    let issuer = serve(fake.clone()).await;
    let c = connect(dir.path(), config.clone(), &issuer);
    run(&c, &mut Vec::new()).await.unwrap();
    let connected = std::fs::read_to_string(&config).unwrap();

    *fake.subject.lock().unwrap() = "300000000000000002".into();
    let error = run(&c, &mut Vec::new()).await.unwrap_err();
    assert_eq!(error.code, "not_owner");
    assert_eq!(std::fs::read_to_string(&config).unwrap(), connected);
    assert_eq!(
        fake.keys.load(SeqCst),
        1,
        "no key for someone who is not the owner"
    );
}

#[tokio::test]
async fn a_declined_approval_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let config = old_config(dir.path());
    let before = std::fs::read_to_string(&config).unwrap();
    let issuer = serve(fake(true)).await;
    let error = run(
        &connect(dir.path(), config.clone(), &issuer),
        &mut Vec::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "nomen");
    assert!(error.message.contains("declined"), "{}", error.message);
    assert_eq!(std::fs::read_to_string(&config).unwrap(), before);
    assert!(!dir.path().join("state").join(KEY_FILE).exists());
}

#[test]
fn ids_are_nomens_numeric_ids() {
    assert!(valid_id(PROJECT));
    for bad in [
        "",
        "abc",
        "1 2",
        "../1",
        "123456789012345678901234567890123",
    ] {
        assert!(!valid_id(bad), "{bad:?}");
    }
}
