//! The hub's end of the machine link (plan N20). A broker on another machine is reached
//! through one WebSocket its `devagotchi-link` dials out to `/link`. The device proves
//! its enrolled Ed25519 key by signing a fresh challenge; after that the hub sends one
//! `call` frame per routed request and waits for the matching `result`.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use axum::extract::ws::{Message, Utf8Bytes, WebSocket};
use devagotchi_contracts::{
    API_VERSION, BridgeError, BridgeResult, HubFrame, LINK_PROTOCOL_VERSION, LinkFrame,
    RequestBody, ResponseBody, WireRequest, WireResponse, link_signing_message,
};
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot};

use crate::{
    audit::{Audit, AuditEntry},
    config::HubConfig,
    devices::Devices,
};

const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// A link that sends nothing for this long is considered gone; devices beat every 20 s.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const REVOCATION_CHECK: Duration = Duration::from_secs(2);

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<WireResponse>>>>;

#[derive(Clone)]
struct Session {
    id: u64,
    calls: mpsc::Sender<(u64, WireRequest)>,
    pending: Pending,
}

pub struct Links {
    config: Arc<HubConfig>,
    devices: Arc<Devices>,
    audit: Arc<Audit>,
    sessions: Mutex<HashMap<String, Session>>,
    next: AtomicU64,
}

fn offline(device: &str) -> BridgeError {
    BridgeError::new(
        "broker_offline",
        format!(
            "{device} is enrolled but its link is not connected; the machine is off, asleep or its devagotchi-link is not running"
        ),
    )
}

impl Links {
    pub fn new(config: Arc<HubConfig>, devices: Arc<Devices>, audit: Arc<Audit>) -> Self {
        Self {
            config,
            devices,
            audit,
            sessions: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
        }
    }

    pub fn connected(&self, device: &str) -> bool {
        self.sessions.lock().expect("links").contains_key(device)
    }

    /// Send one broker request over the device's link and wait for its answer.
    pub async fn call(&self, device: &str, body: RequestBody) -> BridgeResult<ResponseBody> {
        match self.devices.get(device)? {
            None => {
                return Err(BridgeError::new(
                    "not_enrolled",
                    format!(
                        "{device} has never enrolled; the hub operator issues a code with devagotchi-hub enroll"
                    ),
                ));
            }
            Some(record) if record.revoked_at.is_some() => {
                return Err(BridgeError::new(
                    "device_revoked",
                    format!("{device} is revoked"),
                ));
            }
            Some(_) => {}
        }
        let session = self
            .sessions
            .lock()
            .expect("links")
            .get(device)
            .cloned()
            .ok_or_else(|| offline(device))?;
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        session.pending.lock().expect("pending").insert(id, tx);
        let request = WireRequest {
            api_version: API_VERSION,
            request: body,
        };
        if session.calls.send((id, request)).await.is_err() {
            session.pending.lock().expect("pending").remove(&id);
            return Err(offline(device));
        }
        let response = match tokio::time::timeout(CALL_TIMEOUT, rx).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => return Err(offline(device)),
            Err(_) => {
                session.pending.lock().expect("pending").remove(&id);
                return Err(BridgeError::new(
                    "link_timeout",
                    format!(
                        "{device} did not answer within {}s; an admitted operation keeps running there",
                        CALL_TIMEOUT.as_secs()
                    ),
                ));
            }
        };
        match response {
            WireResponse::Ok(body) => Ok(body),
            WireResponse::Error(error) => Err(error),
        }
    }

    fn event(&self, device: &str, tool: &str, outcome: &str) {
        let subject = format!("device:{device}");
        if let Err(error) = self.audit.record(&AuditEntry {
            subject: &subject,
            via: "link",
            tool,
            broker: device,
            operation_id: "",
            outcome,
        }) {
            eprintln!("devagotchi-hub: link audit failed: {error}");
        }
    }

    /// Serve one device connection until it closes, idles out or is revoked.
    pub async fn serve(self: Arc<Self>, socket: WebSocket) {
        let (mut sink, mut stream) = socket.split();
        let nonce = hex::encode(random::<32>());
        let challenge = HubFrame::Challenge {
            nonce: nonce.clone(),
            hub: self.config.public_url.clone(),
        };
        if send(&mut sink, &challenge).await.is_err() {
            return;
        }
        let hello = match tokio::time::timeout(HELLO_TIMEOUT, stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => serde_json::from_str::<LinkFrame>(&text).ok(),
            _ => None,
        };
        let device = match self.admit(hello, &nonce) {
            Ok(device) => device,
            Err(error) => {
                self.event("unknown", "link.refused", &error.code);
                let _ = send(
                    &mut sink,
                    &HubFrame::Refused {
                        code: error.code,
                        message: error.message,
                    },
                )
                .await;
                let _ = sink.close().await;
                return;
            }
        };

        let (calls_tx, mut calls_rx) = mpsc::channel::<(u64, WireRequest)>(64);
        let pending: Pending = Arc::default();
        let session_id = self.next.fetch_add(1, Ordering::Relaxed);
        let previous = self.sessions.lock().expect("links").insert(
            device.clone(),
            Session {
                id: session_id,
                calls: calls_tx,
                pending: pending.clone(),
            },
        );
        drop(previous); // A reconnect replaces the old session; its sender drops and it ends.
        self.event(&device, "link.connect", "ok");
        if send(
            &mut sink,
            &HubFrame::Welcome {
                device: device.clone(),
            },
        )
        .await
        .is_err()
        {
            self.forget(&device, session_id, &pending);
            return;
        }

        let mut revocation = tokio::time::interval(REVOCATION_CHECK);
        let mut idle = Box::pin(tokio::time::sleep(IDLE_TIMEOUT));
        let reason = loop {
            tokio::select! {
                outgoing = calls_rx.recv() => match outgoing {
                    Some((id, request)) => {
                        if send(&mut sink, &HubFrame::Call { id, request }).await.is_err() {
                            break "send_failed";
                        }
                    }
                    None => break "replaced",
                },
                incoming = stream.next() => match incoming {
                    Some(Ok(Message::Text(text))) => {
                        idle.as_mut().reset(tokio::time::Instant::now() + IDLE_TIMEOUT);
                        match serde_json::from_str::<LinkFrame>(&text) {
                            Ok(LinkFrame::Result { id, response }) => {
                                if let Some(waiter) = pending.lock().expect("pending").remove(&id) {
                                    let _ = waiter.send(response);
                                }
                            }
                            Ok(LinkFrame::Heartbeat) => {}
                            Ok(LinkFrame::Hello { .. }) | Err(_) => break "protocol_error",
                        }
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {
                        idle.as_mut().reset(tokio::time::Instant::now() + IDLE_TIMEOUT);
                    }
                    Some(Ok(Message::Binary(_))) => break "protocol_error",
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break "closed",
                },
                _ = &mut idle => break "idle_timeout",
                _ = revocation.tick() => {
                    if self.devices.is_revoked(&device).unwrap_or(true) {
                        let _ = send(&mut sink, &HubFrame::Refused {
                            code: "device_revoked".into(),
                            message: format!("{device} was revoked on this hub"),
                        }).await;
                        break "revoked";
                    }
                }
            }
        };
        let _ = sink.close().await;
        self.forget(&device, session_id, &pending);
        self.event(&device, "link.disconnect", reason);
    }

    fn forget(&self, device: &str, session_id: u64, pending: &Pending) {
        let mut sessions = self.sessions.lock().expect("links");
        if sessions.get(device).is_some_and(|s| s.id == session_id) {
            sessions.remove(device);
        }
        drop(sessions);
        // Dropping the waiters answers every in-flight call with broker_offline.
        pending.lock().expect("pending").clear();
    }

    /// Check the device's hello: a configured link broker, a well-formed key, a valid
    /// signature over this connection's challenge, and an enrolled, unrevoked key (or a
    /// valid one-time code that binds it now).
    fn admit(&self, hello: Option<LinkFrame>, nonce: &str) -> BridgeResult<String> {
        let refuse = |code: &str, message: String| Err(BridgeError::new(code, message));
        let Some(LinkFrame::Hello {
            version,
            device,
            public_key,
            signature,
            enroll_code,
        }) = hello
        else {
            return refuse("protocol_error", "expected a hello frame".into());
        };
        if version != LINK_PROTOCOL_VERSION {
            return refuse(
                "protocol_version",
                format!("hub speaks link protocol {LINK_PROTOCOL_VERSION}, device sent {version}"),
            );
        }
        match self.config.broker(&device) {
            Some(entry) if entry.link => {}
            _ => {
                return refuse(
                    "unknown_broker",
                    format!("{device:?} is not configured as a linked broker on this hub"),
                );
            }
        }
        let key = decode::<32>(&public_key)
            .and_then(|bytes| VerifyingKey::from_bytes(&bytes).ok())
            .ok_or_else(|| BridgeError::new("bad_key", "public_key is not an Ed25519 key"))?;
        let signature = decode::<64>(&signature)
            .map(|bytes| Signature::from_bytes(&bytes))
            .ok_or_else(|| BridgeError::new("bad_signature", "signature is not 64 hex bytes"))?;
        let message = link_signing_message(&self.config.public_url, &device, nonce);
        if key.verify(&message, &signature).is_err() {
            return refuse(
                "bad_signature",
                "signature does not match this challenge".into(),
            );
        }
        if let Some(code) = enroll_code {
            self.devices.enroll(&device, &code, &public_key)?;
            self.event(&device, "link.enroll", "ok");
            return Ok(device);
        }
        match self.devices.get(&device)? {
            None => refuse(
                "not_enrolled",
                format!(
                    "{device} has no enrolled key; run devagotchi-link enroll with a code from the hub operator"
                ),
            ),
            Some(record) if record.revoked_at.is_some() => refuse(
                "device_revoked",
                format!("{device} was revoked on this hub"),
            ),
            Some(record) if record.public_key != public_key => refuse(
                "key_mismatch",
                format!("{device} is enrolled with a different key"),
            ),
            Some(_) => Ok(device),
        }
    }
}

async fn send<S>(sink: &mut S, frame: &HubFrame) -> Result<(), ()>
where
    S: futures_util::Sink<Message> + Unpin,
{
    let text = serde_json::to_string(frame).map_err(|_| ())?;
    sink.send(Message::Text(Utf8Bytes::from(text)))
        .await
        .map_err(|_| ())
}

fn decode<const N: usize>(text: &str) -> Option<[u8; N]> {
    hex::decode(text).ok()?.try_into().ok()
}

pub fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("operating system random source");
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[cfg(unix)]
    const TOKENS: &str = "/tmp/tokens.json";
    #[cfg(windows)]
    const TOKENS: &str = r"C:\tmp\tokens.json";

    fn links(dir: &std::path::Path) -> Links {
        let config = HubConfig::parse(
            &serde_json::json!({
                "listen": "127.0.0.1:8787",
                "public_url": "https://hub.example",
                "auth": { "mode": "dev", "tokens_file": TOKENS },
                "brokers": [ { "name": "gus", "link": true } ],
                "grants": [ { "subject": "jesse", "brokers": ["gus"] } ]
            })
            .to_string(),
        )
        .unwrap();
        Links::new(
            Arc::new(config),
            Arc::new(Devices::open(&dir.join("devices.sqlite3")).unwrap()),
            Arc::new(Audit::open(&dir.join("audit.sqlite3")).unwrap()),
        )
    }

    fn hello(
        key: &SigningKey,
        hub: &str,
        device: &str,
        nonce: &str,
        code: Option<&str>,
    ) -> Option<LinkFrame> {
        Some(LinkFrame::Hello {
            version: LINK_PROTOCOL_VERSION,
            device: device.into(),
            public_key: hex::encode(key.verifying_key().to_bytes()),
            signature: hex::encode(
                key.sign(&link_signing_message(hub, device, nonce))
                    .to_bytes(),
            ),
            enroll_code: code.map(str::to_string),
        })
    }

    #[test]
    fn enrollment_then_signed_reconnects_are_admitted() {
        let dir = tempfile::tempdir().unwrap();
        let links = links(dir.path());
        let key = SigningKey::from_bytes(&[9; 32]);
        let code = links.devices.issue_code("gus", [4; 16]).unwrap();
        let hub = "https://hub.example";
        assert_eq!(
            links
                .admit(hello(&key, hub, "gus", "n1", Some(&code)), "n1")
                .unwrap(),
            "gus"
        );
        assert_eq!(
            links
                .admit(hello(&key, hub, "gus", "n2", None), "n2")
                .unwrap(),
            "gus"
        );
    }

    #[test]
    fn replays_other_hubs_other_keys_and_revoked_devices_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let links = links(dir.path());
        let key = SigningKey::from_bytes(&[9; 32]);
        let other = SigningKey::from_bytes(&[8; 32]);
        let hub = "https://hub.example";
        let code = links.devices.issue_code("gus", [5; 16]).unwrap();
        links
            .admit(hello(&key, hub, "gus", "n1", Some(&code)), "n1")
            .unwrap();
        let refused = |frame, nonce: &str| links.admit(frame, nonce).unwrap_err().code;
        // A signature over an old challenge does not open a new connection.
        assert_eq!(
            refused(hello(&key, hub, "gus", "n1", None), "n2"),
            "bad_signature"
        );
        // Signed for another hub.
        assert_eq!(
            refused(hello(&key, "https://evil.example", "gus", "n3", None), "n3"),
            "bad_signature"
        );
        // A different key claiming the enrolled name.
        assert_eq!(
            refused(hello(&other, hub, "gus", "n4", None), "n4"),
            "key_mismatch"
        );
        // A name the hub does not route to over a link.
        assert_eq!(
            refused(hello(&key, hub, "dabo", "n5", None), "n5"),
            "unknown_broker"
        );
        // Not enrolled yet.
        let fresh = tempfile::tempdir().unwrap();
        assert_eq!(links_refuse_unenrolled(fresh.path(), &key), "not_enrolled");
        links.devices.revoke("gus").unwrap();
        assert_eq!(
            refused(hello(&key, hub, "gus", "n6", None), "n6"),
            "device_revoked"
        );
    }

    fn links_refuse_unenrolled(dir: &std::path::Path, key: &SigningKey) -> String {
        let links = links(dir);
        links
            .admit(hello(key, "https://hub.example", "gus", "n", None), "n")
            .unwrap_err()
            .code
    }

    #[tokio::test]
    async fn calls_to_a_device_without_a_link_say_it_is_offline() {
        let dir = tempfile::tempdir().unwrap();
        let links = links(dir.path());
        let body = RequestBody::OperationList(devagotchi_contracts::ListInput {
            offset: 0,
            limit: 1,
        });
        assert_eq!(
            links.call("gus", body).await.unwrap_err().code,
            "not_enrolled"
        );
        let key = SigningKey::from_bytes(&[9; 32]);
        let code = links.devices.issue_code("gus", [6; 16]).unwrap();
        links
            .admit(
                hello(&key, "https://hub.example", "gus", "n", Some(&code)),
                "n",
            )
            .unwrap();
        let body = RequestBody::OperationList(devagotchi_contracts::ListInput {
            offset: 0,
            limit: 1,
        });
        assert_eq!(
            links.call("gus", body).await.unwrap_err().code,
            "broker_offline"
        );
        links.devices.revoke("gus").unwrap();
        let body = RequestBody::OperationList(devagotchi_contracts::ListInput {
            offset: 0,
            limit: 1,
        });
        assert_eq!(
            links.call("gus", body).await.unwrap_err().code,
            "device_revoked"
        );
    }
}
