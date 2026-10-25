//! Web Push to the phones and browsers people turned notifications on from (doc 39e):
//! the hub tells them an approval is waiting; the page is still the record. Payloads are
//! encrypted for each subscription (RFC 8291, `aes128gcm`) and signed for the push service
//! with this hub's VAPID key (RFC 8292). The key is generated once into the state directory
//! (0600) and only its public half is ever served. A payload names the approval and links
//! to it; it never carries the command text.

use std::{path::Path, sync::Arc, time::Duration};

use aws_lc_rs::{
    aead::{AES_128_GCM, Aad, LessSafeKey, Nonce, UnboundKey},
    agreement::{self, ECDH_P256, PrivateKey, UnparsedPublicKey},
    hkdf::{HKDF_SHA256, KeyType, Salt},
    rand::{SecureRandom, SystemRandom},
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use devagotchi_contracts::{BridgeError, BridgeResult};

use crate::devices::{Devices, PushSubscription, now_secs};

fn push_error(message: impl Into<String>) -> BridgeError {
    BridgeError::new("push", message)
}

/// Waits before trying a busy or unreachable push service again: a phone that misses a
/// push only finds the approval by opening the page.
#[cfg(not(test))]
const RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(10)];
#[cfg(test)]
const RETRY_DELAYS: [Duration; 2] = [Duration::from_millis(10), Duration::from_millis(10)];

/// What one try at a push came to.
#[derive(Debug, PartialEq)]
enum Sent {
    Delivered(u16),
    /// The push service no longer knows the subscription; it was dropped.
    Gone(u16),
    /// Busy (429, 5xx) or not reached; worth another try.
    Retry(String),
    Failed(String),
}

struct Len(usize);
impl KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

fn hkdf(salt: &[u8], ikm: &[u8], info: &[u8], len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    Salt::new(HKDF_SHA256, salt)
        .extract(ikm)
        .expand(&[info], Len(len))
        .and_then(|okm| okm.fill(&mut out))
        .expect("hkdf output of a valid length");
    out
}

/// RFC 8291: one `aes128gcm` record for the subscription `ua_public` / `auth`, from the
/// application server key `as_private` and a 16-byte `salt`.
pub fn encrypt_with(
    as_private: &PrivateKey,
    ua_public: &[u8],
    auth: &[u8],
    salt: &[u8; 16],
    plaintext: &[u8],
) -> BridgeResult<Vec<u8>> {
    let as_public = as_private
        .compute_public_key()
        .map_err(|_| push_error("server key"))?;
    let peer = UnparsedPublicKey::new(&ECDH_P256, ua_public);
    let shared = agreement::agree(as_private, &peer, push_error("bad subscription key"), |s| {
        Ok(s.to_vec())
    })?;
    let mut key_info = b"WebPush: info\0".to_vec();
    key_info.extend_from_slice(ua_public);
    key_info.extend_from_slice(as_public.as_ref());
    let ikm = hkdf(auth, &shared, &key_info, 32);
    let cek = hkdf(salt, &ikm, b"Content-Encoding: aes128gcm\0", 16);
    let nonce = hkdf(salt, &ikm, b"Content-Encoding: nonce\0", 12);
    let key = LessSafeKey::new(UnboundKey::new(&AES_128_GCM, &cek).expect("16-byte key"));
    let mut record = plaintext.to_vec();
    record.push(2); // last record, no padding
    key.seal_in_place_append_tag(
        Nonce::try_assume_unique_for_key(&nonce).expect("12-byte nonce"),
        Aad::empty(),
        &mut record,
    )
    .map_err(|_| push_error("seal"))?;
    let mut body = Vec::with_capacity(86 + record.len());
    body.extend_from_slice(salt);
    body.extend_from_slice(&4096u32.to_be_bytes());
    body.push(as_public.as_ref().len() as u8);
    body.extend_from_slice(as_public.as_ref());
    body.extend_from_slice(&record);
    Ok(body)
}

pub struct Push {
    vapid: EcdsaKeyPair,
    /// The VAPID public key, base64url: what browsers subscribe with.
    pub public_key: String,
    subject: String,
    devices: Arc<Devices>,
    http: reqwest::Client,
}

impl Push {
    /// The hub's VAPID key from `state_dir/vapid.pk8`, made on first use. `subject` is
    /// the contact push services see (the hub's public URL).
    pub fn open(state_dir: &Path, subject: &str, devices: Arc<Devices>) -> BridgeResult<Self> {
        let path = state_dir.join("vapid.pk8");
        let pkcs8 = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(_) => {
                let doc = EcdsaKeyPair::generate_pkcs8(
                    &ECDSA_P256_SHA256_FIXED_SIGNING,
                    &SystemRandom::new(),
                )
                .map_err(|_| push_error("could not make a VAPID key"))?;
                write_private(&path, doc.as_ref())?;
                doc.as_ref().to_vec()
            }
        };
        let vapid = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &pkcs8)
            .map_err(|_| push_error(format!("{} is not a P-256 key", path.display())))?;
        let public_key = URL_SAFE_NO_PAD.encode(vapid.public_key().as_ref());
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| push_error(e.to_string()))?;
        Ok(Self {
            vapid,
            public_key,
            subject: subject.to_string(),
            devices,
            http,
        })
    }

    /// Where subscriptions are kept (the hub's devices store).
    pub fn devices(&self) -> &Devices {
        &self.devices
    }

    /// `vapid t=<jwt>, k=<key>` for a push service at `endpoint` (RFC 8292).
    pub fn authorization(&self, endpoint: &str) -> BridgeResult<String> {
        let url = reqwest::Url::parse(endpoint).map_err(|_| push_error("bad endpoint"))?;
        let audience = url.origin().ascii_serialization();
        let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = serde_json::json!({ "aud": audience, "exp": now_secs() + 12 * 3600, "sub": self.subject });
        let claims = URL_SAFE_NO_PAD.encode(claims.to_string());
        let signing_input = format!("{header}.{claims}");
        let signature = self
            .vapid
            .sign(&SystemRandom::new(), signing_input.as_bytes())
            .map_err(|_| push_error("sign"))?;
        Ok(format!(
            "vapid t={signing_input}.{}, k={}",
            URL_SAFE_NO_PAD.encode(signature.as_ref()),
            self.public_key
        ))
    }

    /// How many phones a push to `subjects` would reach right now.
    pub fn phones_for(&self, subjects: &[String]) -> usize {
        subjects
            .iter()
            .map(|s| self.devices.push_for(s).map(|p| p.len()).unwrap_or(0))
            .sum()
    }

    /// Send `payload` to every subscription of `subjects`. A push service that is busy or
    /// unreachable is tried again ([`RETRY_DELAYS`]); one that says the subscription is gone
    /// (404, 410) has it dropped. Every outcome is logged with the push service's host only,
    /// never raised: push is a hint, the approvals page is the record.
    pub async fn notify(&self, subjects: &[String], payload: &serde_json::Value) {
        let body = payload.to_string();
        let tag = payload["tag"].as_str().unwrap_or("-").to_string();
        for subscription in subjects
            .iter()
            .flat_map(|s| self.devices.push_for(s).unwrap_or_default())
        {
            let host = reqwest::Url::parse(&subscription.endpoint)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .unwrap_or_else(|| "?".into());
            let started = std::time::Instant::now();
            let mut attempt = 0;
            let outcome = loop {
                let outcome = self.send(&subscription, body.as_bytes()).await;
                match (&outcome, RETRY_DELAYS.get(attempt)) {
                    (Sent::Retry(_), Some(delay)) => {
                        attempt += 1;
                        tokio::time::sleep(*delay).await;
                    }
                    _ => break outcome,
                }
            };
            let took = started.elapsed().as_millis();
            let tries = attempt + 1;
            match outcome {
                Sent::Delivered(code) => eprintln!(
                    "devagotchi hub: push {tag} to {host}: delivered ({code}) in {took} ms, {tries} tr{}",
                    if tries == 1 { "y" } else { "ies" }
                ),
                Sent::Gone(code) => eprintln!(
                    "devagotchi hub: push {tag} to {host}: subscription gone ({code}), dropped; that phone must tap Notify this phone again"
                ),
                Sent::Retry(why) | Sent::Failed(why) => eprintln!(
                    "devagotchi hub: push {tag} to {host} for {} failed after {tries} tries in {took} ms: {why}",
                    subscription.subject
                ),
            }
        }
    }

    async fn send(&self, to: &PushSubscription, body: &[u8]) -> Sent {
        match self.try_send(to, body).await {
            Ok(sent) => sent,
            Err(error) => Sent::Failed(error.message),
        }
    }

    async fn try_send(&self, to: &PushSubscription, body: &[u8]) -> BridgeResult<Sent> {
        let ua_public = URL_SAFE_NO_PAD
            .decode(&to.p256dh)
            .map_err(|_| push_error("bad p256dh"))?;
        let auth = URL_SAFE_NO_PAD
            .decode(&to.auth)
            .map_err(|_| push_error("bad auth"))?;
        let mut salt = [0u8; 16];
        SystemRandom::new()
            .fill(&mut salt)
            .map_err(|_| push_error("random"))?;
        let ephemeral = PrivateKey::generate(&ECDH_P256).map_err(|_| push_error("key"))?;
        let sealed = encrypt_with(&ephemeral, &ua_public, &auth, &salt, body)?;
        let response = match self
            .http
            .post(&to.endpoint)
            .header("TTL", "86400")
            .header("Urgency", "high")
            .header("Content-Encoding", "aes128gcm")
            .header("Content-Type", "application/octet-stream")
            .header("Authorization", self.authorization(&to.endpoint)?)
            .body(sealed)
            .send()
            .await
        {
            Ok(response) => response,
            // Not reached, timed out, cut off: worth another try.
            Err(error) => return Ok(Sent::Retry(error.to_string())),
        };
        let code = response.status().as_u16();
        Ok(match code {
            200..=299 => Sent::Delivered(code),
            404 | 410 => {
                let _ = self.devices.drop_push(&to.endpoint);
                Sent::Gone(code)
            }
            429 | 500..=599 => Sent::Retry(format!("push service answered {code}")),
            _ => Sent::Failed(format!("push service answered {code}")),
        })
    }
}

/// Write a new file only its owner can read (0600 on Unix); refuses to replace one.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> BridgeResult<()> {
    let failed =
        |e: std::io::Error| BridgeError::new("state_file", format!("{}: {e}", path.display()));
    #[cfg(unix)]
    {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(failed)?;
        file.write_all(bytes).map_err(failed)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes).map_err(failed)
    }
}

/// The browser's side of RFC 8291, for tests: open a body sealed by `encrypt_with`.
#[cfg(test)]
pub(crate) fn decrypt(ua_private: &PrivateKey, auth: &[u8], body: &[u8]) -> Vec<u8> {
    let (salt, rest) = body.split_at(16);
    let id_len = rest[4] as usize;
    let (as_public, sealed) = rest[5..].split_at(id_len);
    let ua_public = ua_private.compute_public_key().unwrap();
    let shared = agreement::agree(
        ua_private,
        &UnparsedPublicKey::new(&ECDH_P256, as_public),
        (),
        |s| Ok(s.to_vec()),
    )
    .unwrap();
    let mut key_info = b"WebPush: info\0".to_vec();
    key_info.extend_from_slice(ua_public.as_ref());
    key_info.extend_from_slice(as_public);
    let ikm = hkdf(auth, &shared, &key_info, 32);
    let cek = hkdf(salt, &ikm, b"Content-Encoding: aes128gcm\0", 16);
    let nonce = hkdf(salt, &ikm, b"Content-Encoding: nonce\0", 12);
    let key = LessSafeKey::new(UnboundKey::new(&AES_128_GCM, &cek).unwrap());
    let mut record = sealed.to_vec();
    let plain = key
        .open_in_place(
            Nonce::try_assume_unique_for_key(&nonce).unwrap(),
            Aad::empty(),
            &mut record,
        )
        .unwrap();
    assert_eq!(plain.last(), Some(&2), "one last record");
    plain[..plain.len() - 1].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 8291 Appendix A, as the `ece` crate's tests carry it in hex.
    #[test]
    fn encryption_matches_rfc_8291_appendix_a() {
        let h = |s: &str| hex::decode(s).unwrap();
        let as_private = PrivateKey::from_private_key(
            &ECDH_P256,
            &h("c9f58f89813e9f8e872e71f42aa64e1757c9254dcc62b72ddc010bb4043ea11c"),
        )
        .unwrap();
        let ua_public = h(
            "042571b2becdfde360551aaf1ed0f4cd366c11cebe555f89bcb7b186a53339173168ece2ebe018597bd30479b86e3c8f8eced577ca59187e9246990db682008b0e",
        );
        let auth = h("05305932a1c7eabe13b6cec9fda48882");
        let salt: [u8; 16] = h("0c6bfaadad67958803092d454676f397").try_into().unwrap();
        let body = encrypt_with(
            &as_private,
            &ua_public,
            &auth,
            &salt,
            b"When I grow up, I want to be a watermelon",
        )
        .unwrap();
        assert_eq!(
            hex::encode(body),
            "0c6bfaadad67958803092d454676f397000010004104fe33f4ab0dea71914db55823f73b54948f41306d920732dbb9a59a53286482200e597a7b7bc260ba1c227998580992e93973002f3012a28ae8f06bbb78e5ec0ff297de5b429bba7153d3a4ae0caa091fd425f3b4b5414add8ab37a19c1bbb05cf5cb5b2a2e0562d558635641ec52812c6c8ff42e95ccb86be7cd"
        );
    }

    #[test]
    fn the_vapid_key_is_made_once_and_its_token_verifies() {
        use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED, UnparsedPublicKey as Verify};
        let dir = tempfile::tempdir().unwrap();
        let devices = Arc::new(Devices::open(&dir.path().join("d.sqlite3")).unwrap());
        let push = Push::open(dir.path(), "https://hub.example", devices.clone()).unwrap();
        let again = Push::open(dir.path(), "https://hub.example", devices).unwrap();
        assert_eq!(
            push.public_key, again.public_key,
            "the key survives a restart"
        );
        let header = push
            .authorization("https://fcm.googleapis.com/fcm/send/abc")
            .unwrap();
        let token = header
            .strip_prefix("vapid t=")
            .unwrap()
            .split(", k=")
            .next()
            .unwrap();
        let (input, signature) = token.rsplit_once('.').unwrap();
        let claims: serde_json::Value = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(input.split('.').nth(1).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(claims["aud"], "https://fcm.googleapis.com");
        assert_eq!(claims["sub"], "https://hub.example");
        let key = URL_SAFE_NO_PAD.decode(&push.public_key).unwrap();
        Verify::new(&ECDSA_P256_SHA256_FIXED, &key)
            .verify(
                input.as_bytes(),
                &URL_SAFE_NO_PAD.decode(signature).unwrap(),
            )
            .expect("a push service can verify the token with the served key");
    }

    /// A busy push service is tried again; one that says the phone is gone has it dropped;
    /// one that refuses outright is not hammered. Each case keeps or drops exactly the
    /// subscription it should.
    #[tokio::test]
    async fn busy_push_services_are_retried_and_gone_phones_dropped() {
        use std::sync::{Arc as StdArc, Mutex};

        use axum::{extract::Path as UrlPath, http::StatusCode, routing::post};

        // phone-busy answers 503 once, then 201; phone-gone 410; phone-bad 400.
        type Hits = StdArc<Mutex<Vec<String>>>;
        let hits: Hits = StdArc::default();
        async fn receive(
            axum::extract::State(hits): axum::extract::State<Hits>,
            UrlPath(id): UrlPath<String>,
        ) -> StatusCode {
            let mut seen = hits.lock().unwrap();
            seen.push(id.clone());
            let tries = seen.iter().filter(|h| **h == id).count();
            match id.as_str() {
                "phone-busy" if tries == 1 => StatusCode::SERVICE_UNAVAILABLE,
                "phone-busy" => StatusCode::CREATED,
                "phone-gone" => StatusCode::GONE,
                _ => StatusCode::BAD_REQUEST,
            }
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = axum::Router::new()
            .route("/push/{id}", post(receive))
            .with_state(hits.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let dir = tempfile::tempdir().unwrap();
        let devices = Arc::new(Devices::open(&dir.path().join("devices.sqlite3")).unwrap());
        let push = Push::open(dir.path(), "https://hub.example", devices.clone()).unwrap();
        let phone = PrivateKey::generate(&ECDH_P256).unwrap();
        let p256dh = URL_SAFE_NO_PAD.encode(phone.compute_public_key().unwrap().as_ref());
        let auth = URL_SAFE_NO_PAD.encode([7u8; 16]);
        for id in ["phone-busy", "phone-gone", "phone-bad"] {
            devices
                .add_push("me", &format!("{base}/push/{id}"), &p256dh, &auth)
                .unwrap();
        }
        assert_eq!(
            push.phones_for(&["me".to_string(), "nobody".to_string()]),
            3
        );

        push.notify(
            &["me".to_string()],
            &serde_json::json!({"title": "Approval needed", "tag": "t-1"}),
        )
        .await;
        let mut seen = hits.lock().unwrap().clone();
        seen.sort();
        assert_eq!(
            seen,
            ["phone-bad", "phone-busy", "phone-busy", "phone-gone"],
            "503 is tried again, 400 and 410 are not"
        );
        let left: Vec<String> = devices
            .push_for("me")
            .unwrap()
            .into_iter()
            .map(|s| s.endpoint.rsplit('/').next().unwrap().to_string())
            .collect();
        assert!(
            left.contains(&"phone-busy".to_string()) && left.contains(&"phone-bad".to_string()),
            "{left:?}"
        );
        assert!(
            !left.contains(&"phone-gone".to_string()),
            "a gone phone is dropped"
        );
    }
}
