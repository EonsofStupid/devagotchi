//! One connection to the hub: prove the device key over the hub's challenge, then serve
//! `call` frames by handing each request to the local broker and sending back its answer.

use std::{sync::Arc, time::Duration};

use devagotchi_contracts::{
    API_VERSION, BridgeError, HubFrame, LINK_PROTOCOL_VERSION, LinkFrame, MAX_LINK_MESSAGE_BYTES,
    WireRequest, WireResponse, link_signing_message,
};
use ed25519_dalek::{Signer, SigningKey};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::{
    Connector,
    tungstenite::{Message, protocol::WebSocketConfig},
};

use crate::state::{LinkConfig, endpoint};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// The hub drops a link after 60 s of silence.
const HEARTBEAT: Duration = Duration::from_secs(20);
/// Calls handled at once. Beyond this the link answers `link_busy` instead of queueing,
/// so a hub cannot make this machine open unbounded broker connections.
const MAX_IN_FLIGHT: usize = 16;

/// Why a connection ended.
#[derive(Debug)]
pub enum Ended {
    /// Network trouble or the hub went away: reconnect after a pause.
    Retry(String),
    /// The hub refused this device (revoked, not enrolled, wrong key): stop and say why.
    /// A hub that is only temporarily unable to check (`unavailable`) is a Retry instead.
    Refused(BridgeError),
    /// `enroll` finished: the key is bound and the connection closed on purpose.
    Enrolled,
}

pub enum Mode<'a> {
    Enroll { code: &'a str },
    Serve,
}

fn tls() -> Result<Connector, String> {
    use rustls_platform_verifier::ConfigVerifierExt;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let config = rustls::ClientConfig::with_platform_verifier().map_err(|e| e.to_string())?;
    Ok(Connector::Rustls(Arc::new(config)))
}

pub async fn connect(config: &LinkConfig, key: &SigningKey, mode: Mode<'_>) -> Ended {
    let url = match endpoint(&config.hub) {
        Ok(url) => url,
        Err(error) => return Ended::Refused(error),
    };
    let connector = match tls() {
        Ok(connector) => connector,
        Err(error) => return Ended::Refused(BridgeError::new("tls", error)),
    };
    let ws_config = WebSocketConfig::default()
        .max_message_size(Some(MAX_LINK_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_LINK_MESSAGE_BYTES));
    let connecting = tokio_tungstenite::connect_async_tls_with_config(
        url.as_str(),
        Some(ws_config),
        false,
        Some(connector),
    );
    let socket = match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
        Ok(Ok((socket, _))) => socket,
        Ok(Err(error)) => return Ended::Retry(format!("connect {url}: {error}")),
        Err(_) => return Ended::Retry(format!("connect {url}: timed out")),
    };
    let (mut sink, mut stream) = socket.split();

    // Handshake: challenge in, signed hello out, welcome or refusal back.
    let nonce = match next_frame(&mut stream).await {
        Ok(HubFrame::Challenge { nonce, hub }) if hub == config.hub => nonce,
        Ok(HubFrame::Challenge { hub, .. }) => {
            return Ended::Refused(BridgeError::new(
                "hub_mismatch",
                format!("enrolled for {} but this hub says it is {hub}", config.hub),
            ));
        }
        Ok(_) => return Ended::Retry("hub did not open with a challenge".into()),
        Err(why) => return Ended::Retry(why),
    };
    let signature = key.sign(&link_signing_message(&config.hub, &config.device, &nonce));
    let hello = LinkFrame::Hello {
        version: LINK_PROTOCOL_VERSION,
        device: config.device.clone(),
        public_key: hex::encode(key.verifying_key().to_bytes()),
        signature: hex::encode(signature.to_bytes()),
        enroll_code: match mode {
            Mode::Enroll { code } => Some(code.to_string()),
            Mode::Serve => None,
        },
    };
    if let Err(why) = send(&mut sink, &hello).await {
        return Ended::Retry(why);
    }
    match next_frame(&mut stream).await {
        Ok(HubFrame::Welcome { .. }) => {}
        Ok(HubFrame::Refused { code, message }) => return refusal(code, message),
        Ok(_) => return Ended::Retry("hub answered the hello with an unexpected frame".into()),
        Err(why) => return Ended::Retry(why),
    }
    if let Mode::Enroll { .. } = mode {
        let _ = sink.close().await;
        return Ended::Enrolled;
    }
    eprintln!(
        "devagotchi link ready: {} linked to {} (broker {})",
        config.device, config.hub, config.broker
    );

    // Serve: each call runs in its own task; answers funnel back through one writer.
    let (answers_tx, mut answers_rx) = mpsc::channel::<LinkFrame>(64);
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.tick().await;
    let broker = std::path::PathBuf::from(&config.broker);
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT));
    loop {
        tokio::select! {
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<HubFrame>(&text) {
                    Ok(HubFrame::Call { id, request }) => {
                        let answers = answers_tx.clone();
                        let Ok(slot) = slots.clone().try_acquire_owned() else {
                            let busy = WireResponse::Error(BridgeError::new(
                                "link_busy",
                                format!("this machine is already handling {MAX_IN_FLIGHT} calls"),
                            ));
                            let _ = answers.try_send(LinkFrame::Result { id, response: busy });
                            continue;
                        };
                        let broker = broker.clone();
                        tokio::spawn(async move {
                            let response = handle(&broker, request).await;
                            let _ = answers.send(LinkFrame::Result { id, response }).await;
                            drop(slot);
                        });
                    }
                    Ok(HubFrame::Refused { code, message }) => return refusal(code, message),
                    Ok(_) | Err(_) => return Ended::Retry("hub sent a frame this link does not understand".into()),
                },
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {}
                Some(Ok(Message::Binary(_))) => return Ended::Retry("hub sent a binary frame".into()),
                Some(Ok(Message::Close(_))) | None => return Ended::Retry("hub closed the link".into()),
                Some(Err(error)) => return Ended::Retry(format!("link read: {error}")),
            },
            Some(answer) = answers_rx.recv() => {
                if let Err(why) = send(&mut sink, &answer).await {
                    return Ended::Retry(why);
                }
            }
            _ = heartbeat.tick() => {
                if let Err(why) = send(&mut sink, &LinkFrame::Heartbeat).await {
                    return Ended::Retry(why);
                }
            }
        }
    }
}

/// A refusal from the hub: permanent unless the hub says it only could not check now.
fn refusal(code: String, message: String) -> Ended {
    if code == "unavailable" {
        Ended::Retry(format!("hub temporarily unavailable: {message}"))
    } else {
        Ended::Refused(BridgeError::new(&code, message))
    }
}

/// Hand one request to the local broker. The broker's own validation and executable
/// grants decide; the link adds nothing and removes nothing.
async fn handle(broker: &std::path::Path, request: WireRequest) -> WireResponse {
    if request.api_version != API_VERSION {
        return WireResponse::Error(BridgeError::new(
            "api_version",
            format!(
                "broker speaks api {API_VERSION}, hub sent {}",
                request.api_version
            ),
        ));
    }
    match devagotchi_ipc::request(broker, request.request).await {
        Ok(body) => WireResponse::Ok(body),
        Err(error) => WireResponse::Error(error),
    }
}

async fn next_frame<S>(stream: &mut S) -> Result<HubFrame, String>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                return serde_json::from_str(&text).map_err(|e| format!("bad frame from hub: {e}"));
            }
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)))) => continue,
            Ok(Some(Ok(_))) => return Err("unexpected message from hub".into()),
            Ok(Some(Err(error))) => return Err(format!("link read: {error}")),
            Ok(None) => return Err("hub closed the link during the handshake".into()),
            Err(_) => return Err("hub did not answer in time".into()),
        }
    }
}

async fn send<S>(sink: &mut S, frame: &LinkFrame) -> Result<(), String>
where
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    let text = serde_json::to_string(frame).map_err(|e| e.to_string())?;
    sink.send(Message::text(text))
        .await
        .map_err(|e| format!("link write: {e}"))
}
