//! A software authenticator for tests: it holds a real key, builds the authenticator data
//! and client data a browser would, and signs them, for one relying party.

use aws_lc_rs::{
    digest,
    encoding::{AsDer, Pkcs8V1Der},
    rand::SystemRandom,
    rsa::KeySize,
    signature::{self, EcdsaKeyPair, Ed25519KeyPair, KeyPair, RsaKeyPair},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

use super::*;

pub(crate) enum Key {
    Ec(EcdsaKeyPair),
    Ed(Ed25519KeyPair),
    Rsa(RsaKeyPair),
}

pub(crate) struct Authenticator {
    pub alg: Alg,
    key: Key,
    pub id: Vec<u8>,
    pub count: u32,
    pub rp_id: String,
    pub origin: String,
}

/// What one ceremony hands the hub, owned so a test can corrupt any part of it.
#[derive(Clone)]
pub(crate) struct Sent {
    pub credential_id: Vec<u8>,
    pub client_data_json: Vec<u8>,
    pub authenticator_data: Vec<u8>,
    pub public_key: Vec<u8>,
    pub alg: i64,
    pub signature: Vec<u8>,
}

impl Sent {
    pub fn registration(&self) -> Registration<'_> {
        Registration {
            credential_id: &self.credential_id,
            client_data_json: &self.client_data_json,
            authenticator_data: &self.authenticator_data,
            public_key: &self.public_key,
            alg: self.alg,
        }
    }

    pub fn assertion(&self) -> Assertion<'_> {
        Assertion {
            credential_id: &self.credential_id,
            client_data_json: &self.client_data_json,
            authenticator_data: &self.authenticator_data,
            signature: &self.signature,
        }
    }
}

pub(crate) fn client_data(
    kind: &str,
    challenge: &[u8],
    origin: &str,
    cross: Option<bool>,
) -> Vec<u8> {
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
    /// For the hosted hub: passkeys for `devagotchi.com`, made on `app.devagotchi.com`.
    pub fn new(alg: Alg) -> Self {
        Self::for_site(alg, "devagotchi.com", "https://app.devagotchi.com")
    }

    pub fn for_site(alg: Alg, rp_id: &str, origin: &str) -> Self {
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
            rp_id: rp_id.to_string(),
            origin: origin.to_string(),
        }
    }

    pub fn spki(&self) -> Vec<u8> {
        match &self.key {
            Key::Ec(pair) => pair.public_key().as_der().unwrap().as_ref().to_vec(),
            Key::Ed(pair) => pair.public_key().as_der().unwrap().as_ref().to_vec(),
            Key::Rsa(pair) => pair.public_key().as_der().unwrap().as_ref().to_vec(),
        }
    }

    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
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

    pub fn auth_data(&self, rp_id: &str, flags: u8, attested: bool) -> Vec<u8> {
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

    pub fn register(&self, challenge: &[u8]) -> Sent {
        Sent {
            credential_id: self.id.clone(),
            client_data_json: client_data("webauthn.create", challenge, &self.origin, None),
            authenticator_data: self.auth_data(&self.rp_id, FLAG_UP | FLAG_UV | FLAG_AT, true),
            public_key: self.spki(),
            alg: self.alg.cose(),
            signature: Vec::new(),
        }
    }

    pub fn assert(&mut self, challenge: &[u8]) -> Sent {
        self.count += 1;
        let client = client_data("webauthn.get", challenge, &self.origin, Some(false));
        let data = self.auth_data(&self.rp_id, FLAG_UP | FLAG_UV, false);
        self.signed(client, data)
    }

    /// Sign whatever authenticator and client data a test built.
    pub fn signed(&self, client: Vec<u8>, data: Vec<u8>) -> Sent {
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
