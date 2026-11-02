//! The hub's end of the machine link (plan N20). A broker on another machine is reached
//! through one WebSocket its `devagotchi link` dials out to `/link`. The device proves
//! its enrolled Ed25519 key by signing a fresh challenge; after that the hub sends one
//! `call` frame per routed request and waits for the matching `result`.
//!
//! `/link` is reachable without a bearer token, so everything before a valid hello is
//! bounded: concurrent handshakes, the hello's size, and how often refusals are audited.
//! A peer learns only that it was refused; the reason goes to the operator's audit.

use std::{
    collections::HashMap,
    net::SocketAddr,
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
use ed25519_dalek::{Signature, VerifyingKey};
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};

use crate::{
    audit::{Audit, AuditEntry},
    config::HubConfig,
    devices::{Devices, now_secs},
};

const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// A hello is a few hundred bytes; anything larger is refused before parsing.
const MAX_HELLO_BYTES: usize = 4096;
/// Handshakes in flight at once; more are refused with 503 before the upgrade.
const MAX_HANDSHAKES: usize = 32;
/// Calls queued for one device beyond those in flight; more answer `link_busy`.
const CALL_QUEUE: usize = 64;
/// A link that sends nothing for this long is considered gone; devices beat every 20 s.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// No single write to a device may hold the session loop longer than this.
const SEND_TIMEOUT: Duration = Duration::from_secs(10);
const REVOCATION_CHECK: Duration = Duration::from_secs(2);

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<WireResponse>>>>;

#[derive(Clone)]
struct Session {
    id: u64,
    calls: mpsc::Sender<(u64, WireRequest)>,
    pending: Pending,
}

/// Removes a call's waiter if the caller stops waiting for any reason (cancelled, timed
/// out, link gone), so abandoned calls never accumulate.
struct Waiter {
    pending: Pending,
    id: u64,
}

impl Drop for Waiter {
    fn drop(&mut self) {
        self.pending.lock().expect("pending").remove(&self.id);
    }
}

/// Refusals are audited at most once a second; the rest are counted into the next row.
#[derive(Default)]
struct RefusalLog {
    last_second: u64,
    suppressed: u64,
}

pub struct Links {
    config: Arc<HubConfig>,
    devices: Arc<Devices>,
    audit: Arc<Audit>,
    sessions: Mutex<HashMap<String, Session>>,
    handshakes: Arc<Semaphore>,
    refusals: Mutex<RefusalLog>,
    next: AtomicU64,
    /// Tells an account's phones when a machine it added joins (increment 29a).
    push: Option<Arc<crate::push::Push>>,
}

/// A device the hello proved, and the account it just joined for, if it enrolled now
/// with an account's code.
#[derive(Debug)]
struct Admitted {
    device: String,
    key: String,
    joined_for: Option<String>,
}

fn offline(device: &str) -> BridgeError {
    BridgeError::new(
        "broker_offline",
        format!(
            "{device} is enrolled but its link is not connected; the machine is off, asleep or its `devagotchi link` is not running"
        ),
    )
}

/// A short, stable fingerprint of a device key for the audit log.
fn fingerprint(public_key: &str) -> String {
    hex::encode(&Sha256::digest(public_key.as_bytes())[..8])
}

/// What a refused peer is told. Details stay in the audit so the wire does not reveal
/// which device names exist or what state they are in.
fn wire_refusal(error: &BridgeError) -> HubFrame {
    let (code, message) = match error.code.as_str() {
        "protocol_version" => ("protocol_version", error.message.as_str()),
        "device_store" | "audit_write" => (
            "unavailable",
            "the hub could not check this device right now; retry",
        ),
        _ => (
            "refused",
            "this hub refused the link; its operator can see why in the hub's audit log",
        ),
    };
    HubFrame::Refused {
        code: code.into(),
        message: message.into(),
    }
}

impl Links {
    pub fn new(config: Arc<HubConfig>, devices: Arc<Devices>, audit: Arc<Audit>) -> Self {
        Self {
            config,
            devices,
            audit,
            sessions: Mutex::new(HashMap::new()),
            handshakes: Arc::new(Semaphore::new(MAX_HANDSHAKES)),
            refusals: Mutex::new(RefusalLog::default()),
            next: AtomicU64::new(1),
            push: None,
        }
    }

    /// Push "your machine joined" to the owner's phones when a machine an account added
    /// enrolls.
    pub fn notifying(mut self, push: Option<Arc<crate::push::Push>>) -> Self {
        self.push = push;
        self
    }

    /// The enrolled record for `device`, if any (revoked ones included).
    pub fn device(&self, device: &str) -> BridgeResult<Option<crate::devices::Device>> {
        self.devices.get(device)
    }

    /// The machines `owner` added, revoked ones included.
    pub fn owned_by(&self, owner: &str) -> BridgeResult<Vec<crate::devices::Device>> {
        self.devices.owned_by(owner)
    }

    /// Move machines, codes, and push subscriptions from one account id to another
    /// after a provisional account is merged.
    pub fn reassign_owner(&self, from: &str, to: &str) -> BridgeResult<usize> {
        self.devices.reassign_owner(from, to)
    }

    /// Spend an account's one-time code and bind `public_key` to `name`. The code is
    /// the secret; this is the same check the link handshake uses.
    pub fn enroll_owned(
        &self,
        name: &str,
        code: &str,
        public_key: &str,
    ) -> BridgeResult<crate::devices::Device> {
        let device = self.devices.enroll(name, code, public_key, false)?;
        self.event(
            device.owner.as_deref().unwrap_or(""),
            "join",
            "link.enroll",
            name,
            "",
            "ok",
        );
        Ok(device)
    }

    /// A one-time code for `owner` to add a machine named `device` ("Add a machine"). The
    /// names in the hub config are the operator's. Audited without the code.
    pub fn issue_owned_code(&self, owner: &str, device: &str) -> BridgeResult<String> {
        if !crate::config::valid_name(device) {
            return Err(BridgeError::new(
                "invalid_name",
                "a machine name is 1 to 63 lowercase letters, digits and dashes, starting with a letter or digit",
            ));
        }
        if self.config.broker(device).is_some() {
            return Err(BridgeError::new(
                "name_taken",
                format!("{device:?} is already in use on this hub; choose another name"),
            ));
        }
        let limit = if self.config.is_owner(owner) {
            usize::MAX
        } else {
            crate::devices::OWNED_LIMIT
        };
        let issued = self
            .devices
            .issue_owned_code(owner, device, random::<16>(), limit);
        let outcome = match &issued {
            Ok(_) => "ok".to_string(),
            Err(error) => error.code.clone(),
        };
        self.event(owner, "web", "link.code_issued", device, "", &outcome);
        issued
    }

    /// The machine a live code is for, so `/join` can write its script; audited with
    /// the machine's name, never the code. Looking does not spend the code.
    pub fn join_script(
        &self,
        code: &str,
        shell: crate::join::Shell,
    ) -> BridgeResult<Option<String>> {
        let device = self.devices.live_code(code)?;
        if let Some(device) = &device {
            let shell = match shell {
                crate::join::Shell::Sh => "sh",
                crate::join::Shell::PowerShell => "powershell",
            };
            self.event(
                &format!("device:{device}"),
                "join",
                "link.join_script",
                device,
                "",
                shell,
            );
        }
        Ok(device)
    }

    /// Revoke one of `owner`'s machines; its open link closes within seconds.
    pub fn revoke_owned(&self, owner: &str, device: &str) -> BridgeResult<(bool, usize)> {
        let result = self.devices.revoke_owned(owner, device);
        let outcome = match &result {
            Ok((revoked, cancelled)) => format!("revoked={revoked} codes_cancelled={cancelled}"),
            Err(error) => error.code.clone(),
        };
        self.event(owner, "web", "link.revoke", device, "", &outcome);
        result
    }

    pub fn connected(&self, device: &str) -> bool {
        self.sessions.lock().expect("links").contains_key(device)
    }

    /// A slot for one unauthenticated handshake, or `None` when all are taken.
    pub fn handshake_permit(&self) -> Option<OwnedSemaphorePermit> {
        self.handshakes.clone().try_acquire_owned().ok()
    }

    /// Send one broker request over the device's link and wait for its answer.
    pub async fn call(&self, device: &str, body: RequestBody) -> BridgeResult<ResponseBody> {
        match self.devices.get(device)? {
            None => {
                return Err(BridgeError::new(
                    "not_enrolled",
                    format!(
                        "{device} has never enrolled; the hub operator issues a code with `devagotchi hub enroll`"
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
        let _waiter = Waiter {
            pending: session.pending.clone(),
            id,
        };
        // A wait is answered when the operation changes or its own timeout passes.
        let within = CALL_TIMEOUT + body.held_for();
        let request = WireRequest {
            api_version: API_VERSION,
            request: body,
        };
        match session.calls.try_send((id, request)) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                return Err(BridgeError::new(
                    "link_busy",
                    format!("{device} already has {CALL_QUEUE} calls waiting; retry shortly"),
                ));
            }
            Err(mpsc::error::TrySendError::Closed(_)) => return Err(offline(device)),
        }
        let response = match tokio::time::timeout(within, rx).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => return Err(offline(device)),
            Err(_) => {
                return Err(BridgeError::new(
                    "link_timeout",
                    format!(
                        "{device} did not answer within {}s; an admitted operation keeps running there",
                        within.as_secs()
                    ),
                ));
            }
        };
        match response {
            WireResponse::Ok(body) => Ok(body),
            WireResponse::Error(error) => Err(error),
        }
    }

    fn event(&self, subject: &str, via: &str, tool: &str, broker: &str, key: &str, outcome: &str) {
        if let Err(error) = self.audit.record(&AuditEntry {
            subject,
            via,
            tool,
            broker,
            operation_id: key,
            outcome,
        }) {
            eprintln!("devagotchi hub: link audit failed: {error}");
        }
    }

    /// Audit a refused handshake, at most once a second. The claimed name is recorded as
    /// unverified; the peer address is the TCP peer (the edge, when behind Caddy).
    fn refused(&self, claimed: Option<&str>, peer: SocketAddr, error: &BridgeError) {
        let now = now_secs();
        let suppressed = {
            let mut log = self.refusals.lock().expect("refusals");
            if log.last_second == now {
                log.suppressed += 1;
                return;
            }
            log.last_second = now;
            std::mem::take(&mut log.suppressed)
        };
        let claimed: String = claimed
            .unwrap_or("-")
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .take(63)
            .collect();
        let subject = format!("unverified:{claimed}");
        let via = format!("link {peer}");
        let outcome = format!("{} ({} more refusals not itemized)", error.code, suppressed);
        eprintln!(
            "devagotchi hub: link refused from {peer} claiming {claimed:?}: {}",
            error
        );
        self.event(&subject, &via, "link.refused", &claimed, "", &outcome);
    }

    /// Serve one device connection until it closes, idles out or is revoked.
    pub async fn serve(
        self: Arc<Self>,
        socket: WebSocket,
        peer: SocketAddr,
        permit: OwnedSemaphorePermit,
    ) {
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
            Ok(Some(Ok(Message::Text(text)))) if text.len() <= MAX_HELLO_BYTES => {
                serde_json::from_str::<LinkFrame>(&text).ok()
            }
            _ => None,
        };
        let claimed = match &hello {
            Some(LinkFrame::Hello { device, .. }) => Some(device.clone()),
            _ => None,
        };
        let Admitted {
            device,
            key,
            joined_for,
        } = match self.admit(hello, &nonce) {
            Ok(admitted) => admitted,
            Err(error) => {
                self.refused(claimed.as_deref(), peer, &error);
                let _ = send(&mut sink, &wire_refusal(&error)).await;
                let _ = tokio::time::timeout(SEND_TIMEOUT, sink.close()).await;
                return;
            }
        };
        drop(permit);

        let (calls_tx, mut calls_rx) = mpsc::channel::<(u64, WireRequest)>(CALL_QUEUE);
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
        let subject = format!("device:{device}");
        let via = format!("link {peer}");
        self.event(
            &subject,
            &via,
            "link.connect",
            &device,
            &fingerprint(&key),
            "ok",
        );
        if let (Some(owner), Some(push)) = (joined_for, self.push.clone()) {
            let print = fingerprint(&key);
            let name = device.clone();
            tokio::spawn(async move {
                push.notify(
                    &[owner],
                    &serde_json::json!({
                        "title": format!("{name} joined Devagotchi"),
                        "body": format!("Key {print}. Not you? Open Machines and revoke it."),
                        "url": "/account",
                        "tag": format!("devagotchi-machine-{name}"),
                    }),
                )
                .await;
            });
        }
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
                        // Re-check at the moment of forwarding: a call queued just before a
                        // revoke must not reach the device. Fail closed if the store errs.
                        match self.devices.is_revoked(&device) {
                            Ok(false) => {
                                if send(&mut sink, &HubFrame::Call { id, request }).await.is_err() {
                                    break "send_failed";
                                }
                            }
                            Ok(true) => {
                                let _ = send(&mut sink, &revoked_frame(&device)).await;
                                break "revoked";
                            }
                            Err(error) => {
                                if let Some(waiter) = pending.lock().expect("pending").remove(&id) {
                                    let _ = waiter.send(WireResponse::Error(error));
                                }
                            }
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
                    // Only a definite revocation ends the link; a store hiccup does not.
                    if let Ok(true) = self.devices.is_revoked(&device) {
                        let _ = send(&mut sink, &revoked_frame(&device)).await;
                        break "revoked";
                    }
                }
            }
        };
        self.forget(&device, session_id, &pending);
        let _ = tokio::time::timeout(SEND_TIMEOUT, sink.close()).await;
        self.event(
            &subject,
            &via,
            "link.disconnect",
            &device,
            &fingerprint(&key),
            reason,
        );
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

    /// Check the device's hello: a configured link broker or a machine an account added,
    /// a strong key, a strict signature over this connection's challenge, and an enrolled,
    /// unrevoked key (or a valid one-time code that binds it now). Returns the device name
    /// and its key.
    fn admit(&self, hello: Option<LinkFrame>, nonce: &str) -> BridgeResult<Admitted> {
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
        // A configured broker on this host is never a link; a name the config does not
        // know is a machine an account added (checked against its code or record below).
        let configured = match self.config.broker(&device) {
            Some(entry) if entry.link => true,
            Some(_) => {
                return refuse(
                    "unknown_broker",
                    format!("{device:?} is configured as a broker on this hub's host, not a link"),
                );
            }
            None if crate::config::valid_name(&device) => false,
            None => {
                return refuse(
                    "unknown_broker",
                    format!("{device:?} is not a valid machine name"),
                );
            }
        };
        let key = decode::<32>(&public_key)
            .and_then(|bytes| VerifyingKey::from_bytes(&bytes).ok())
            .filter(|key| !key.is_weak())
            .ok_or_else(|| BridgeError::new("bad_key", "public_key is not a strong Ed25519 key"))?;
        let signature = decode::<64>(&signature)
            .map(|bytes| Signature::from_bytes(&bytes))
            .ok_or_else(|| BridgeError::new("bad_signature", "signature is not 64 hex bytes"))?;
        let message = link_signing_message(&self.config.public_url, &device, nonce);
        if key.verify_strict(&message, &signature).is_err() {
            return refuse(
                "bad_signature",
                "signature does not match this challenge".into(),
            );
        }
        if let Some(code) = enroll_code {
            let bound = self
                .devices
                .enroll(&device, &code, &public_key, configured)?;
            self.event(
                &format!("device:{device}"),
                "link",
                "link.enroll",
                &device,
                &fingerprint(&public_key),
                &match &bound.owner {
                    Some(owner) => format!("ok owner={owner}"),
                    None => "ok".into(),
                },
            );
            return Ok(Admitted {
                device,
                key: public_key,
                joined_for: bound.owner,
            });
        }
        match self.devices.get(&device)? {
            None => refuse("not_enrolled", format!("{device} has no enrolled key")),
            // A machine the config stopped naming, that no account added, is no longer served.
            Some(record) if !configured && record.owner.is_none() => refuse(
                "unknown_broker",
                format!("{device:?} is no longer configured on this hub"),
            ),
            Some(record) if record.revoked_at.is_some() => refuse(
                "device_revoked",
                format!("{device} was revoked on this hub"),
            ),
            Some(record) if record.public_key != public_key => refuse(
                "key_mismatch",
                format!("{device} is enrolled with a different key"),
            ),
            Some(_) => Ok(Admitted {
                device,
                key: public_key,
                joined_for: None,
            }),
        }
    }
}

fn revoked_frame(device: &str) -> HubFrame {
    HubFrame::Refused {
        code: "device_revoked".into(),
        message: format!("{device} was revoked on this hub"),
    }
}

/// One frame to the device, bounded in time so a peer that stops reading cannot hold
/// the session loop (and with it the revocation and idle checks).
async fn send<S>(sink: &mut S, frame: &HubFrame) -> Result<(), ()>
where
    S: futures_util::Sink<Message> + Unpin,
{
    let text = serde_json::to_string(frame).map_err(|_| ())?;
    match tokio::time::timeout(
        SEND_TIMEOUT,
        sink.send(Message::Text(Utf8Bytes::from(text))),
    )
    .await
    {
        Ok(Ok(())) => Ok(()),
        _ => Err(()),
    }
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
    const HUB: &str = "http://127.0.0.1:8787";

    fn links(dir: &std::path::Path) -> Links {
        let config = HubConfig::parse(
            &serde_json::json!({
                "listen": "127.0.0.1:8787",
                "public_url": HUB,
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
        let admitted = links
            .admit(hello(&key, HUB, "gus", "n1", Some(&code)), "n1")
            .unwrap();
        assert_eq!(
            (admitted.device.as_str(), admitted.joined_for),
            ("gus", None)
        );
        assert!(
            links
                .admit(hello(&key, HUB, "gus", "n2", None), "n2")
                .is_ok()
        );
    }

    #[test]
    fn replays_other_hubs_other_keys_and_revoked_devices_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let links = links(dir.path());
        let key = SigningKey::from_bytes(&[9; 32]);
        let other = SigningKey::from_bytes(&[8; 32]);
        let code = links.devices.issue_code("gus", [5; 16]).unwrap();
        links
            .admit(hello(&key, HUB, "gus", "n1", Some(&code)), "n1")
            .unwrap();
        let refused = |frame, nonce: &str| links.admit(frame, nonce).unwrap_err().code;
        // A signature over an old challenge does not open a new connection.
        assert_eq!(
            refused(hello(&key, HUB, "gus", "n1", None), "n2"),
            "bad_signature"
        );
        // Signed for another hub.
        assert_eq!(
            refused(hello(&key, "https://evil.example", "gus", "n3", None), "n3"),
            "bad_signature"
        );
        // A different key claiming the enrolled name.
        assert_eq!(
            refused(hello(&other, HUB, "gus", "n4", None), "n4"),
            "key_mismatch"
        );
        // A name nobody configured or added.
        assert_eq!(
            refused(hello(&key, HUB, "dabo", "n5", None), "n5"),
            "not_enrolled"
        );
        links.devices.revoke("gus").unwrap();
        assert_eq!(
            refused(hello(&key, HUB, "gus", "n6", None), "n6"),
            "device_revoked"
        );
    }

    #[test]
    fn a_machine_an_account_added_joins_for_that_account_without_config() {
        let dir = tempfile::tempdir().unwrap();
        let links = links(dir.path());
        let key = SigningKey::from_bytes(&[11; 32]);
        // An operator's code cannot add a name the config does not have.
        let operator = links.devices.issue_code("unnamed", [1; 16]).unwrap();
        assert_eq!(
            links
                .admit(hello(&key, HUB, "unnamed", "n1", Some(&operator)), "n1")
                .unwrap_err()
                .code,
            "enroll_refused"
        );
        let code = links
            .devices
            .issue_owned_code("acct-a", "platinum", [2; 16], crate::devices::OWNED_LIMIT)
            .unwrap();
        let joined = links
            .admit(hello(&key, HUB, "platinum", "n2", Some(&code)), "n2")
            .unwrap();
        assert_eq!(joined.joined_for.as_deref(), Some("acct-a"));
        // Reconnecting later is an ordinary signed hello, and nobody is told it joined again.
        let again = links
            .admit(hello(&key, HUB, "platinum", "n3", None), "n3")
            .unwrap();
        assert_eq!(
            (again.device.as_str(), again.joined_for),
            ("platinum", None)
        );
        assert_eq!(links.owned_by("acct-a").unwrap().len(), 1);
        // A name that could never be a broker stops at the door.
        assert_eq!(
            links
                .admit(hello(&key, HUB, "Bad Name", "n4", None), "n4")
                .unwrap_err()
                .code,
            "unknown_broker"
        );
    }

    #[test]
    fn weak_keys_are_refused_even_with_a_valid_code() {
        let dir = tempfile::tempdir().unwrap();
        let links = links(dir.path());
        let code = links.devices.issue_code("gus", [7; 16]).unwrap();
        // The identity point: a small-order key whose "signatures" anyone can forge.
        let mut identity = [0u8; 32];
        identity[0] = 1;
        let frame = Some(LinkFrame::Hello {
            version: LINK_PROTOCOL_VERSION,
            device: "gus".into(),
            public_key: hex::encode(identity),
            signature: hex::encode([0u8; 64]),
            enroll_code: Some(code),
        });
        assert_eq!(links.admit(frame, "n").unwrap_err().code, "bad_key");
        assert!(links.devices.get("gus").unwrap().is_none());
    }

    #[test]
    fn the_wire_says_only_refused_unless_retrying_can_help() {
        for code in [
            "unknown_broker",
            "not_enrolled",
            "key_mismatch",
            "device_revoked",
            "enroll_refused",
            "bad_key",
        ] {
            let HubFrame::Refused { code: wire, .. } =
                wire_refusal(&BridgeError::new(code, "detail"))
            else {
                panic!("expected a refusal");
            };
            assert_eq!(wire, "refused", "{code} must not reach the peer");
        }
        let HubFrame::Refused { code, .. } =
            wire_refusal(&BridgeError::new("device_store", "busy"))
        else {
            panic!("expected a refusal");
        };
        assert_eq!(code, "unavailable");
    }

    #[test]
    fn handshakes_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let links = links(dir.path());
        let held: Vec<_> = (0..MAX_HANDSHAKES)
            .map(|_| links.handshake_permit().unwrap())
            .collect();
        assert!(links.handshake_permit().is_none());
        drop(held);
        assert!(links.handshake_permit().is_some());
    }

    #[tokio::test]
    async fn calls_to_a_device_without_a_link_say_why() {
        let dir = tempfile::tempdir().unwrap();
        let links = links(dir.path());
        let list = || {
            RequestBody::OperationList(devagotchi_contracts::ListInput {
                offset: 0,
                limit: 1,
            })
        };
        assert_eq!(
            links.call("gus", list()).await.unwrap_err().code,
            "not_enrolled"
        );
        let key = SigningKey::from_bytes(&[9; 32]);
        let code = links.devices.issue_code("gus", [6; 16]).unwrap();
        links
            .admit(hello(&key, HUB, "gus", "n", Some(&code)), "n")
            .unwrap();
        assert_eq!(
            links.call("gus", list()).await.unwrap_err().code,
            "broker_offline"
        );
        links.devices.revoke("gus").unwrap();
        assert_eq!(
            links.call("gus", list()).await.unwrap_err().code,
            "device_revoked"
        );
    }

    #[tokio::test]
    async fn a_full_queue_answers_busy_and_leaves_no_waiters() {
        let dir = tempfile::tempdir().unwrap();
        let links = links(dir.path());
        let key = SigningKey::from_bytes(&[9; 32]);
        let code = links.devices.issue_code("gus", [3; 16]).unwrap();
        links
            .admit(hello(&key, HUB, "gus", "n", Some(&code)), "n")
            .unwrap();
        // A session whose loop never drains: the device stopped reading.
        let (calls, _keep) = mpsc::channel(CALL_QUEUE);
        let pending: Pending = Arc::default();
        for i in 0..CALL_QUEUE as u64 {
            let request = WireRequest {
                api_version: API_VERSION,
                request: RequestBody::OperationList(devagotchi_contracts::ListInput {
                    offset: 0,
                    limit: 1,
                }),
            };
            calls.try_send((u64::MAX - i, request)).unwrap();
        }
        links.sessions.lock().unwrap().insert(
            "gus".into(),
            Session {
                id: 1,
                calls,
                pending: pending.clone(),
            },
        );
        let body = RequestBody::OperationList(devagotchi_contracts::ListInput {
            offset: 0,
            limit: 1,
        });
        assert_eq!(links.call("gus", body).await.unwrap_err().code, "link_busy");
        assert!(
            pending.lock().unwrap().is_empty(),
            "a refused call must not leave a waiter"
        );
    }
}
