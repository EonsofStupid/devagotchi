//! A fresh RSA key per test run, so no private key is ever committed.

use aws_lc_rs::{
    encoding::AsDer,
    rsa::{KeyPair, KeySize},
    signature::KeyPair as _,
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};

pub struct TestKey {
    /// PKCS#8 PEM, as `EncodingKey::from_rsa_pem` and Nomen key files take it.
    pub pem: String,
    /// Public modulus and exponent, base64url, as a JWK carries them.
    pub n: String,
    pub e: String,
}

pub fn rsa() -> TestKey {
    let pair = KeyPair::generate(KeySize::Rsa2048).expect("generate RSA key");
    let der = AsDer::<aws_lc_rs::encoding::Pkcs8V1Der>::as_der(&pair).expect("PKCS#8");
    let body = STANDARD.encode(der.as_ref());
    let lines: Vec<&str> = body
        .as_bytes()
        .chunks(64)
        .map(|chunk| std::str::from_utf8(chunk).expect("base64 is ASCII"))
        .collect();
    let public = pair.public_key();
    TestKey {
        pem: format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            lines.join("\n")
        ),
        n: URL_SAFE_NO_PAD.encode(public.modulus().big_endian_without_leading_zero()),
        e: URL_SAFE_NO_PAD.encode(public.exponent().big_endian_without_leading_zero()),
    }
}
