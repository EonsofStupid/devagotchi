//! Passkeys (WebAuthn), verified by the hub itself on `aws-lc-rs` (D25, §14).
//!
//! No CBOR and no attestation: the browser hands over the credential's public key as an
//! X.509 `SubjectPublicKeyInfo` (`AuthenticatorAttestationResponse.getPublicKey()`) and the
//! authenticator data as bytes (`getAuthenticatorData()`), so the hub parses only the
//! fixed binary layout of the authenticator data and the client data JSON. Attestation is
//! `none`: the hub trusts the key it was given at registration, as every passkey site
//! without an enterprise attestation policy does. User verification is required on every
//! ceremony, because a passkey here replaces a password.

use aws_lc_rs::{digest, signature};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;

/// The algorithms a hub accepts, by their COSE identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alg {
    /// ECDSA P-256 with SHA-256 (COSE -7), what nearly every passkey uses.
    Es256,
    /// Ed25519 (COSE -8).
    EdDsa,
    /// RSASSA-PKCS1-v1_5 with SHA-256 (COSE -257), Windows Hello's older keys.
    Rs256,
}

impl Alg {
    pub fn from_cose(alg: i64) -> Option<Self> {
        match alg {
            -7 => Some(Alg::Es256),
            -8 => Some(Alg::EdDsa),
            -257 => Some(Alg::Rs256),
            _ => None,
        }
    }

    pub fn cose(self) -> i64 {
        match self {
            Alg::Es256 => -7,
            Alg::EdDsa => -8,
            Alg::Rs256 => -257,
        }
    }

    fn verification(self) -> &'static dyn signature::VerificationAlgorithm {
        match self {
            Alg::Es256 => &signature::ECDSA_P256_SHA256_ASN1,
            Alg::EdDsa => &signature::ED25519,
            Alg::Rs256 => &signature::RSA_PKCS1_2048_8192_SHA256,
        }
    }
}

/// Who the ceremony is for: the relying party id the passkey is bound to (decision 1:
/// `devagotchi.com` on the hosted hub, the hub's own host when self-hosted) and the exact
/// origin of the page that ran it.
#[derive(Debug, Clone, Copy)]
pub struct Relying<'a> {
    pub rp_id: &'a str,
    pub origin: &'a str,
}

/// Why a ceremony was refused. For the operator's log; a person is only told it failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refusal(pub &'static str);

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// What the browser sends back from `navigator.credentials.create()`.
#[derive(Debug, Clone, Copy)]
pub struct Registration<'a> {
    /// `PublicKeyCredential.rawId`.
    pub credential_id: &'a [u8],
    pub client_data_json: &'a [u8],
    /// `getAuthenticatorData()`.
    pub authenticator_data: &'a [u8],
    /// `getPublicKey()`: DER `SubjectPublicKeyInfo`.
    pub public_key: &'a [u8],
    /// `getPublicKeyAlgorithm()`.
    pub alg: i64,
}

/// A credential that passed registration, ready to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCredential {
    pub id: Vec<u8>,
    pub public_key: Vec<u8>,
    pub alg: Alg,
    pub sign_count: u32,
    /// The authenticator may sync this passkey to the person's other devices.
    pub backup_eligible: bool,
}

/// What the browser sends back from `navigator.credentials.get()`.
#[derive(Debug, Clone, Copy)]
pub struct Assertion<'a> {
    pub credential_id: &'a [u8],
    pub client_data_json: &'a [u8],
    pub authenticator_data: &'a [u8],
    pub signature: &'a [u8],
}

/// A stored credential an assertion is checked against.
#[derive(Debug, Clone, Copy)]
pub struct Stored<'a> {
    pub id: &'a [u8],
    pub public_key: &'a [u8],
    pub alg: Alg,
    pub sign_count: u32,
    /// Whether the credential was backup eligible when it was registered.
    pub backup_eligible: bool,
}

/// The longest credential id WebAuthn allows.
const MAX_CREDENTIAL_ID: usize = 1023;
/// rpIdHash (32), flags (1), signCount (4).
const AUTH_DATA_HEADER: usize = 37;
const FLAG_UP: u8 = 0x01;
const FLAG_UV: u8 = 0x04;
const FLAG_BE: u8 = 0x08;
const FLAG_BS: u8 = 0x10;
const FLAG_AT: u8 = 0x40;

/// Check a registration made for `challenge` and return the credential to store.
pub fn verify_registration(
    relying: Relying<'_>,
    challenge: &[u8],
    registration: Registration<'_>,
) -> Result<NewCredential, Refusal> {
    check_client_data(
        registration.client_data_json,
        "webauthn.create",
        challenge,
        relying,
    )?;
    let data = registration.authenticator_data;
    let flags = check_authenticator_data(data, relying)?;
    if flags & FLAG_AT == 0 {
        return Err(Refusal("no attested credential"));
    }
    // AAGUID (16), credential id length (2), credential id, then the COSE key, which is
    // not read: the key comes as SubjectPublicKeyInfo beside it.
    let at = AUTH_DATA_HEADER + 16;
    let length = data
        .get(at..at + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
        .ok_or(Refusal("authenticator data"))?;
    let id = data
        .get(at + 2..at + 2 + length)
        .filter(|_| length > 0 && length <= MAX_CREDENTIAL_ID)
        .ok_or(Refusal("authenticator data"))?;
    if id != registration.credential_id {
        return Err(Refusal("credential id"));
    }
    let alg = Alg::from_cose(registration.alg).ok_or(Refusal("algorithm"))?;
    signature::ParsedPublicKey::new(alg.verification(), registration.public_key)
        .map_err(|_| Refusal("public key"))?;
    // RSA keys are judged when they are registered, not at every sign-in after: 2048 to
    // 8192 bits, as the verification algorithm will demand.
    if alg == Alg::Rs256 {
        aws_lc_rs::rsa::PublicEncryptingKey::from_der(registration.public_key)
            .map_err(|_| Refusal("rsa key size"))?;
    }
    Ok(NewCredential {
        id: id.to_vec(),
        public_key: registration.public_key.to_vec(),
        alg,
        sign_count: sign_count(data),
        backup_eligible: flags & FLAG_BE != 0,
    })
}

/// Check an assertion made for `challenge` against the stored credential; returns the
/// authenticator's new signature counter to store.
pub fn verify_assertion(
    relying: Relying<'_>,
    challenge: &[u8],
    stored: Stored<'_>,
    assertion: Assertion<'_>,
) -> Result<u32, Refusal> {
    if assertion.credential_id != stored.id {
        return Err(Refusal("credential id"));
    }
    check_client_data(
        assertion.client_data_json,
        "webauthn.get",
        challenge,
        relying,
    )?;
    let data = assertion.authenticator_data;
    let flags = check_authenticator_data(data, relying)?;
    // Backup eligibility is fixed when a credential is made; a change means another one.
    if (flags & FLAG_BE != 0) != stored.backup_eligible {
        return Err(Refusal("backup eligibility"));
    }
    let mut message = Vec::with_capacity(data.len() + 32);
    message.extend_from_slice(data);
    message.extend_from_slice(digest::digest(&digest::SHA256, assertion.client_data_json).as_ref());
    signature::UnparsedPublicKey::new(stored.alg.verification(), stored.public_key)
        .verify(&message, assertion.signature)
        .map_err(|_| Refusal("signature"))?;
    // A counter that does not move forward means the key was copied; authenticators that
    // keep no counter (most synced passkeys) report 0 every time.
    let count = sign_count(data);
    if (count != 0 || stored.sign_count != 0) && count <= stored.sign_count {
        return Err(Refusal("signature counter"));
    }
    Ok(count)
}

fn check_client_data(
    json: &[u8],
    kind: &str,
    challenge: &[u8],
    relying: Relying<'_>,
) -> Result<(), Refusal> {
    let data: ClientData = serde_json::from_slice(json).map_err(|_| Refusal("client data"))?;
    if data.kind != kind {
        return Err(Refusal("client data type"));
    }
    // Browsers send the challenge as base64url without padding; it is compared exactly.
    if data.challenge != URL_SAFE_NO_PAD.encode(challenge) {
        return Err(Refusal("challenge"));
    }
    if data.origin != relying.origin {
        return Err(Refusal("origin"));
    }
    // The hub's pages are never framed: a ceremony run inside another site's page carries
    // `crossOrigin` or `topOrigin`.
    if data.cross_origin == Some(true) || data.top_origin.is_some() {
        return Err(Refusal("cross-origin"));
    }
    Ok(())
}

/// The fixed header every ceremony checks: the passkey belongs to this relying party, and
/// the person was present and verified. Returns the flags.
fn check_authenticator_data(data: &[u8], relying: Relying<'_>) -> Result<u8, Refusal> {
    if data.len() < AUTH_DATA_HEADER {
        return Err(Refusal("authenticator data"));
    }
    if data[..32] != *digest::digest(&digest::SHA256, relying.rp_id.as_bytes()).as_ref() {
        return Err(Refusal("rp id"));
    }
    let flags = data[32];
    if flags & FLAG_UP == 0 {
        return Err(Refusal("user not present"));
    }
    if flags & FLAG_UV == 0 {
        return Err(Refusal("user not verified"));
    }
    // Backed up but not eligible for backup is a contradiction (WebAuthn §6.1.3).
    if flags & FLAG_BS != 0 && flags & FLAG_BE == 0 {
        return Err(Refusal("backup state"));
    }
    Ok(flags)
}

fn sign_count(data: &[u8]) -> u32 {
    u32::from_be_bytes([data[33], data[34], data[35], data[36]])
}

#[derive(Deserialize)]
struct ClientData {
    #[serde(rename = "type")]
    kind: String,
    challenge: String,
    origin: String,
    #[serde(default, rename = "crossOrigin")]
    cross_origin: Option<bool>,
    #[serde(default, rename = "topOrigin")]
    top_origin: Option<String>,
}

#[cfg(test)]
mod tests;
