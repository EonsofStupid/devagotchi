//! A software authenticator stands in for a phone: it holds a real key, builds the
//! authenticator data and client data a browser would, and signs them. Every check the
//! verifier makes is then broken one at a time.

use aws_lc_rs::{
    digest,
    encoding::{AsDer, Pkcs8V1Der},
    rand::SystemRandom,
    rsa::KeySize,
    signature::{self, EcdsaKeyPair, Ed25519KeyPair, KeyPair, RsaKeyPair},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

use super::*;

const RP: Relying<'static> = Relying {
    rp_id: "devagotchi.com",
    origin: "https://app.devagotchi.com",
};

enum Key {
    Ec(EcdsaKeyPair),
    Ed(Ed25519KeyPair),
    Rsa(RsaKeyPair),
}

struct Authenticator {
    alg: Alg,
    key: Key,
    id: Vec<u8>,
    count: u32,
}

/// What one ceremony hands the hub, owned so a test can corrupt any part of it.
#[derive(Clone)]
struct Sent {
    credential_id: Vec<u8>,
    client_data_json: Vec<u8>,
    authenticator_data: Vec<u8>,
    public_key: Vec<u8>,
    alg: i64,
    signature: Vec<u8>,
}

impl Sent {
    fn registration(&self) -> Registration<'_> {
        Registration {
            credential_id: &self.credential_id,
            client_data_json: &self.client_data_json,
            authenticator_data: &self.authenticator_data,
            public_key: &self.public_key,
            alg: self.alg,
        }
    }

    fn assertion(&self) -> Assertion<'_> {
        Assertion {
            credential_id: &self.credential_id,
            client_data_json: &self.client_data_json,
            authenticator_data: &self.authenticator_data,
            signature: &self.signature,
        }
    }
}

fn client_data(kind: &str, challenge: &[u8], origin: &str, cross: Option<bool>) -> Vec<u8> {
    let mut value = serde_json::json!({
        "type": kind,
        "challenge": URL_SAFE_NO_PAD.encode(challenge),
        "origin": origin,
    });
    if let Some(cross) = cross {
        value["crossOrigin"] = cross.into();
    }
    serde_json::to_vec(&value).unwrap()
}

impl Authenticator {
    fn new(alg: Alg) -> Self {
        let rng = SystemRandom::new();
        let key = match alg {
            Alg::Es256 => {
                let pkcs8 =
                    EcdsaKeyPair::generate_pkcs8(&signature::ECDSA_P256_SHA256_ASN1_SIGNING, &rng)
                        .unwrap();
                Key::Ec(
                    EcdsaKeyPair::from_pkcs8(
                        &signature::ECDSA_P256_SHA256_ASN1_SIGNING,
                        pkcs8.as_ref(),
                    )
                    .unwrap(),
                )
            }
            Alg::EdDsa => {
                let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
                Key::Ed(Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap())
            }
            Alg::Rs256 => {
                let pair = aws_lc_rs::rsa::KeyPair::generate(KeySize::Rsa2048).unwrap();
                let der = AsDer::<Pkcs8V1Der>::as_der(&pair).unwrap();
                Key::Rsa(RsaKeyPair::from_pkcs8(der.as_ref()).unwrap())
            }
        };
        Authenticator {
            alg,
            key,
            id: crate::link::random::<16>().to_vec(),
            count: 0,
        }
    }

    fn spki(&self) -> Vec<u8> {
        match &self.key {
            Key::Ec(pair) => pair.public_key().as_der().unwrap().as_ref().to_vec(),
            Key::Ed(pair) => pair.public_key().as_der().unwrap().as_ref().to_vec(),
            Key::Rsa(pair) => pair.public_key().as_der().unwrap().as_ref().to_vec(),
        }
    }

    fn sign(&self, message: &[u8]) -> Vec<u8> {
        let rng = SystemRandom::new();
        match &self.key {
            Key::Ec(pair) => pair.sign(&rng, message).unwrap().as_ref().to_vec(),
            Key::Ed(pair) => pair.sign(message).as_ref().to_vec(),
            Key::Rsa(pair) => {
                let mut out = vec![0; pair.public_modulus_len()];
                pair.sign(&signature::RSA_PKCS1_SHA256, &rng, message, &mut out)
                    .unwrap();
                out
            }
        }
    }

    fn auth_data(&self, rp_id: &str, flags: u8, attested: bool) -> Vec<u8> {
        let mut data = digest::digest(&digest::SHA256, rp_id.as_bytes())
            .as_ref()
            .to_vec();
        data.push(flags);
        data.extend_from_slice(&self.count.to_be_bytes());
        if attested {
            data.extend_from_slice(&[0u8; 16]); // AAGUID
            data.extend_from_slice(&(self.id.len() as u16).to_be_bytes());
            data.extend_from_slice(&self.id);
            data.extend_from_slice(&[0xa5, 0x01, 0x02]); // the start of a COSE key, unread
        }
        data
    }

    fn register(&self, challenge: &[u8]) -> Sent {
        Sent {
            credential_id: self.id.clone(),
            client_data_json: client_data("webauthn.create", challenge, RP.origin, None),
            authenticator_data: self.auth_data(RP.rp_id, FLAG_UP | FLAG_UV | FLAG_AT, true),
            public_key: self.spki(),
            alg: self.alg.cose(),
            signature: Vec::new(),
        }
    }

    fn assert(&mut self, challenge: &[u8]) -> Sent {
        self.count += 1;
        let client = client_data("webauthn.get", challenge, RP.origin, Some(false));
        let data = self.auth_data(RP.rp_id, FLAG_UP | FLAG_UV, false);
        self.signed(client, data)
    }

    /// Sign whatever authenticator and client data a test built.
    fn signed(&self, client: Vec<u8>, data: Vec<u8>) -> Sent {
        let mut message = data.clone();
        message.extend_from_slice(digest::digest(&digest::SHA256, &client).as_ref());
        Sent {
            credential_id: self.id.clone(),
            signature: self.sign(&message),
            client_data_json: client,
            authenticator_data: data,
            public_key: self.spki(),
            alg: self.alg.cose(),
        }
    }
}

fn stored<'a>(credential: &'a NewCredential, count: u32) -> Stored<'a> {
    Stored {
        id: &credential.id,
        public_key: &credential.public_key,
        alg: credential.alg,
        sign_count: count,
        backup_eligible: credential.backup_eligible,
    }
}

#[test]
fn every_algorithm_registers_and_signs_in() {
    for alg in [Alg::Es256, Alg::EdDsa, Alg::Rs256] {
        let mut phone = Authenticator::new(alg);
        let challenge = crate::link::random::<32>();
        let credential =
            verify_registration(RP, &challenge, phone.register(&challenge).registration())
                .unwrap_or_else(|refusal| panic!("{alg:?} registration: {refusal}"));
        assert_eq!(credential.id, phone.id);
        assert_eq!(credential.alg, alg);
        assert_eq!(credential.public_key, phone.spki());
        let challenge = crate::link::random::<32>();
        let sent = phone.assert(&challenge);
        let count = verify_assertion(RP, &challenge, stored(&credential, 0), sent.assertion())
            .unwrap_or_else(|refusal| panic!("{alg:?} assertion: {refusal}"));
        assert_eq!(count, 1);
    }
}

#[test]
fn registration_refusals() {
    let phone = Authenticator::new(Alg::Es256);
    let challenge = [7u8; 32];
    let good = phone.register(&challenge);
    let refused = |sent: &Sent, challenge: &[u8]| {
        verify_registration(RP, challenge, sent.registration()).unwrap_err()
    };

    assert!(verify_registration(RP, &challenge, good.registration()).is_ok());
    assert_eq!(refused(&good, &[8u8; 32]).0, "challenge");

    let mut sent = good.clone();
    sent.client_data_json = client_data("webauthn.get", &challenge, RP.origin, None);
    assert_eq!(refused(&sent, &challenge).0, "client data type");

    let mut sent = good.clone();
    sent.client_data_json = client_data(
        "webauthn.create",
        &challenge,
        "https://devagotchi.com.evil.example",
        None,
    );
    assert_eq!(refused(&sent, &challenge).0, "origin");

    let mut sent = good.clone();
    sent.client_data_json = client_data("webauthn.create", &challenge, RP.origin, Some(true));
    assert_eq!(refused(&sent, &challenge).0, "cross-origin");

    let mut sent = good.clone();
    sent.authenticator_data = phone.auth_data("evil.example", FLAG_UP | FLAG_UV | FLAG_AT, true);
    assert_eq!(refused(&sent, &challenge).0, "rp id");

    let mut sent = good.clone();
    sent.authenticator_data = phone.auth_data(RP.rp_id, FLAG_UV | FLAG_AT, true);
    assert_eq!(refused(&sent, &challenge).0, "user not present");

    let mut sent = good.clone();
    sent.authenticator_data = phone.auth_data(RP.rp_id, FLAG_UP | FLAG_AT, true);
    assert_eq!(refused(&sent, &challenge).0, "user not verified");

    let mut sent = good.clone();
    sent.authenticator_data = phone.auth_data(RP.rp_id, FLAG_UP | FLAG_UV, false);
    assert_eq!(refused(&sent, &challenge).0, "no attested credential");

    let mut sent = good.clone();
    sent.credential_id = vec![1, 2, 3];
    assert_eq!(refused(&sent, &challenge).0, "credential id");

    let mut sent = good.clone();
    sent.alg = -35; // ES384: not accepted
    assert_eq!(refused(&sent, &challenge).0, "algorithm");

    let mut sent = good.clone();
    sent.public_key = vec![0x30, 0x03, 0x02, 0x01, 0x00];
    assert_eq!(refused(&sent, &challenge).0, "public key");

    let mut sent = good.clone();
    sent.public_key = Authenticator::new(Alg::EdDsa).spki(); // an Ed25519 key declared ES256
    assert_eq!(refused(&sent, &challenge).0, "public key");

    let mut sent = good.clone();
    sent.authenticator_data.truncate(AUTH_DATA_HEADER + 10);
    assert_eq!(refused(&sent, &challenge).0, "authenticator data");

    let mut sent = good.clone();
    sent.client_data_json = b"{not json".to_vec();
    assert_eq!(refused(&sent, &challenge).0, "client data");
}

#[test]
fn assertion_refusals() {
    let mut phone = Authenticator::new(Alg::Es256);
    let challenge = [9u8; 32];
    let credential =
        verify_registration(RP, &challenge, phone.register(&challenge).registration()).unwrap();
    let good = phone.assert(&challenge);
    let refused = |sent: &Sent, challenge: &[u8], count: u32| {
        verify_assertion(RP, challenge, stored(&credential, count), sent.assertion()).unwrap_err()
    };

    assert_eq!(refused(&good, &[1u8; 32], 0).0, "challenge");

    let mut sent = good.clone();
    sent.signature[10] ^= 0x01;
    assert_eq!(refused(&sent, &challenge, 0).0, "signature");

    // Valid signatures over the wrong things.
    let wrong_type = phone.signed(
        client_data("webauthn.create", &challenge, RP.origin, None),
        phone.auth_data(RP.rp_id, FLAG_UP | FLAG_UV, false),
    );
    assert_eq!(refused(&wrong_type, &challenge, 0).0, "client data type");
    let wrong_origin = phone.signed(
        client_data("webauthn.get", &challenge, "https://evil.example", None),
        phone.auth_data(RP.rp_id, FLAG_UP | FLAG_UV, false),
    );
    assert_eq!(refused(&wrong_origin, &challenge, 0).0, "origin");
    let wrong_rp = phone.signed(
        client_data("webauthn.get", &challenge, RP.origin, None),
        phone.auth_data("evil.example", FLAG_UP | FLAG_UV, false),
    );
    assert_eq!(refused(&wrong_rp, &challenge, 0).0, "rp id");
    let unverified = phone.signed(
        client_data("webauthn.get", &challenge, RP.origin, None),
        phone.auth_data(RP.rp_id, FLAG_UP, false),
    );
    assert_eq!(refused(&unverified, &challenge, 0).0, "user not verified");

    // Another credential's assertion is not this one's.
    let mut sent = good.clone();
    sent.credential_id = vec![4, 5, 6];
    assert_eq!(refused(&sent, &challenge, 0).0, "credential id");

    // A counter that does not move forward is a cloned authenticator.
    assert_eq!(refused(&good, &challenge, 1).0, "signature counter");
    assert_eq!(refused(&good, &challenge, 5).0, "signature counter");
    assert_eq!(
        verify_assertion(RP, &challenge, stored(&credential, 0), good.assertion()),
        Ok(1)
    );
}

#[test]
fn authenticators_that_keep_no_counter_still_sign_in() {
    // Synced passkeys report 0 every time; 0 after 0 is not a clone.
    let mut phone = Authenticator::new(Alg::Es256);
    let challenge = [3u8; 32];
    let mut registered = phone.register(&challenge);
    registered.authenticator_data =
        phone.auth_data(RP.rp_id, FLAG_UP | FLAG_UV | FLAG_AT | FLAG_BE, true);
    let credential = verify_registration(RP, &challenge, registered.registration()).unwrap();
    phone.count = 0;
    let client = client_data("webauthn.get", &challenge, RP.origin, None);
    let data = phone.auth_data(RP.rp_id, FLAG_UP | FLAG_UV | FLAG_BE, false);
    let sent = phone.signed(client, data);
    assert_eq!(
        verify_assertion(RP, &challenge, stored(&credential, 0), sent.assertion()),
        Ok(0)
    );
    assert_eq!(
        verify_assertion(RP, &challenge, stored(&credential, 3), sent.assertion()),
        Err(Refusal("signature counter"))
    );
}

#[test]
fn backup_eligibility_is_recorded() {
    let phone = Authenticator::new(Alg::Es256);
    let challenge = [5u8; 32];
    let mut sent = phone.register(&challenge);
    sent.authenticator_data =
        phone.auth_data(RP.rp_id, FLAG_UP | FLAG_UV | FLAG_AT | FLAG_BE, true);
    assert!(
        verify_registration(RP, &challenge, sent.registration())
            .unwrap()
            .backup_eligible
    );
    let sent = phone.register(&challenge);
    assert!(
        !verify_registration(RP, &challenge, sent.registration())
            .unwrap()
            .backup_eligible
    );
}

/// A 1024-bit RSA public key (SubjectPublicKeyInfo), made once with OpenSSL: too small to
/// accept, and refused when it is registered rather than at every sign-in after.
const RSA_1024_SPKI: &str = "30819f300d06092a864886f70d010101050003818d0030818902818100bcd52027e8f0772a3aaeed709aa9c3f53dc8d3\
fb632fc6ad40ecb3b47f375ba4bd2a3719c9e4800d0cfbf5c0f44a701f5341ab4d6ea1eabdd40c3d8f59a0b2dcdede5c\
bf958c8148c6fb5ee20ba188b959b09653e2c5237dcae4c3899e8d1cb8b188e8c50bd8965c0807e466ff8cea34de074e\
cc4d1221c95d4c4ed1b811b7d10203010001";

#[test]
fn rsa_keys_must_be_2048_to_8192_bits_when_registered() {
    let phone = Authenticator::new(Alg::Es256);
    let challenge = [6u8; 32];
    let mut sent = phone.register(&challenge);
    sent.alg = Alg::Rs256.cose();
    sent.public_key = hex::decode(RSA_1024_SPKI).unwrap();
    assert_eq!(
        verify_registration(RP, &challenge, sent.registration()),
        Err(Refusal("rsa key size"))
    );
    // A 2048-bit key is the smallest accepted.
    let rsa = Authenticator::new(Alg::Rs256);
    assert!(verify_registration(RP, &challenge, rsa.register(&challenge).registration()).is_ok());
}

#[test]
fn malformed_authenticator_data_is_refused_without_reading_past_it() {
    let phone = Authenticator::new(Alg::Es256);
    let challenge = [2u8; 32];
    let good = phone.register(&challenge);
    let refused = |data: Vec<u8>| {
        let mut sent = good.clone();
        sent.authenticator_data = data;
        verify_registration(RP, &challenge, sent.registration()).unwrap_err()
    };
    let header = |length: u16| {
        let mut data = phone.auth_data(RP.rp_id, FLAG_UP | FLAG_UV | FLAG_AT, false);
        data.extend_from_slice(&[0u8; 16]);
        data.extend_from_slice(&length.to_be_bytes());
        data
    };
    assert_eq!(
        refused(good.authenticator_data[..36].to_vec()).0,
        "authenticator data"
    );
    assert_eq!(refused(Vec::new()).0, "authenticator data");
    // A credential id of length 0, over 1023, or running one byte past the data.
    assert_eq!(refused(header(0)).0, "authenticator data");
    let mut long = header(1024);
    long.extend_from_slice(&[1u8; 1024]);
    assert_eq!(refused(long).0, "authenticator data");
    let mut short = header(phone.id.len() as u16 + 1);
    short.extend_from_slice(&phone.id);
    assert_eq!(refused(short).0, "authenticator data");
}

#[test]
fn backup_state_needs_eligibility_and_eligibility_never_changes() {
    let mut phone = Authenticator::new(Alg::Es256);
    let challenge = [4u8; 32];
    // Backed up (BS) but not eligible (BE) is a contradiction the spec says to refuse.
    let mut sent = phone.register(&challenge);
    sent.authenticator_data =
        phone.auth_data(RP.rp_id, FLAG_UP | FLAG_UV | FLAG_AT | FLAG_BS, true);
    assert_eq!(
        verify_registration(RP, &challenge, sent.registration()),
        Err(Refusal("backup state"))
    );

    let single =
        verify_registration(RP, &challenge, phone.register(&challenge).registration()).unwrap();
    assert!(!single.backup_eligible);
    let bs_only = phone.signed(
        client_data("webauthn.get", &challenge, RP.origin, None),
        phone.auth_data(RP.rp_id, FLAG_UP | FLAG_UV | FLAG_BS, false),
    );
    assert_eq!(
        verify_assertion(RP, &challenge, stored(&single, 0), bs_only.assertion()),
        Err(Refusal("backup state"))
    );
    // Eligibility is fixed when a credential is made: a change means another credential.
    phone.count = 1;
    let now_eligible = phone.signed(
        client_data("webauthn.get", &challenge, RP.origin, None),
        phone.auth_data(RP.rp_id, FLAG_UP | FLAG_UV | FLAG_BE | FLAG_BS, false),
    );
    assert_eq!(
        verify_assertion(RP, &challenge, stored(&single, 0), now_eligible.assertion()),
        Err(Refusal("backup eligibility"))
    );
    let mut synced = single.clone();
    synced.backup_eligible = true;
    let not_eligible = phone.signed(
        client_data("webauthn.get", &challenge, RP.origin, None),
        phone.auth_data(RP.rp_id, FLAG_UP | FLAG_UV, false),
    );
    assert_eq!(
        verify_assertion(RP, &challenge, stored(&synced, 0), not_eligible.assertion()),
        Err(Refusal("backup eligibility"))
    );
    assert_eq!(
        verify_assertion(RP, &challenge, stored(&synced, 0), now_eligible.assertion()),
        Ok(1)
    );
}

#[test]
fn embedded_and_padded_client_data_is_refused() {
    let mut phone = Authenticator::new(Alg::Es256);
    let challenge = [1u8; 32];
    let credential =
        verify_registration(RP, &challenge, phone.register(&challenge).registration()).unwrap();
    // A page framed in another site carries topOrigin; the hub's pages are never framed.
    let mut framed: serde_json::Value =
        serde_json::from_slice(&client_data("webauthn.create", &challenge, RP.origin, None))
            .unwrap();
    framed["topOrigin"] = "https://evil.example".into();
    let mut sent = phone.register(&challenge);
    sent.client_data_json = serde_json::to_vec(&framed).unwrap();
    assert_eq!(
        verify_registration(RP, &challenge, sent.registration()),
        Err(Refusal("cross-origin"))
    );
    // The challenge is compared exactly: base64url without padding, as browsers send it.
    let mut padded: serde_json::Value =
        serde_json::from_slice(&client_data("webauthn.create", &challenge, RP.origin, None))
            .unwrap();
    padded["challenge"] = format!("{}=", URL_SAFE_NO_PAD.encode(challenge)).into();
    let mut sent = phone.register(&challenge);
    sent.client_data_json = serde_json::to_vec(&padded).unwrap();
    assert_eq!(
        verify_registration(RP, &challenge, sent.registration()),
        Err(Refusal("challenge"))
    );

    // The same checks on the sign-in side, each under a valid signature.
    let refused = |sent: &Sent| {
        verify_assertion(RP, &challenge, stored(&credential, 0), sent.assertion()).unwrap_err()
    };
    phone.count = 1;
    framed["type"] = "webauthn.get".into();
    let framed = phone.signed(
        serde_json::to_vec(&framed).unwrap(),
        phone.auth_data(RP.rp_id, FLAG_UP | FLAG_UV, false),
    );
    assert_eq!(refused(&framed).0, "cross-origin");
    let cross = phone.signed(
        client_data("webauthn.get", &challenge, RP.origin, Some(true)),
        phone.auth_data(RP.rp_id, FLAG_UP | FLAG_UV, false),
    );
    assert_eq!(refused(&cross).0, "cross-origin");
    let absent = phone.signed(
        client_data("webauthn.get", &challenge, RP.origin, None),
        phone.auth_data(RP.rp_id, FLAG_UV, false),
    );
    assert_eq!(refused(&absent).0, "user not present");
    let short = phone.signed(
        client_data("webauthn.get", &challenge, RP.origin, None),
        phone.auth_data(RP.rp_id, FLAG_UP | FLAG_UV, false)[..36].to_vec(),
    );
    assert_eq!(refused(&short).0, "authenticator data");
    let garbled = phone.signed(
        b"{\"type\":".to_vec(),
        phone.auth_data(RP.rp_id, FLAG_UP | FLAG_UV, false),
    );
    assert_eq!(refused(&garbled).0, "client data");
}
