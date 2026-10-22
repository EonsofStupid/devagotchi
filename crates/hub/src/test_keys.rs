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
    /// PKCS#1 PEM (`BEGIN RSA PRIVATE KEY`), the form Nomen's key files carry.
    pub pem: String,
    /// The same key as PKCS#8 PEM (`BEGIN PRIVATE KEY`).
    pub pkcs8_pem: String,
    /// Public modulus and exponent, base64url, as a JWK carries them.
    pub n: String,
    pub e: String,
}

pub fn rsa() -> TestKey {
    let pair = KeyPair::generate(KeySize::Rsa2048).expect("generate RSA key");
    let pkcs8 = AsDer::<aws_lc_rs::encoding::Pkcs8V1Der>::as_der(&pair).expect("PKCS#8");
    let pkcs8 = pkcs8.as_ref();
    let public = pair.public_key();
    TestKey {
        pem: pem("RSA PRIVATE KEY", pkcs1_of(pkcs8)),
        pkcs8_pem: pem("PRIVATE KEY", pkcs8),
        n: URL_SAFE_NO_PAD.encode(public.modulus().big_endian_without_leading_zero()),
        e: URL_SAFE_NO_PAD.encode(public.exponent().big_endian_without_leading_zero()),
    }
}

fn pem(label: &str, der: &[u8]) -> String {
    let body = STANDARD.encode(der);
    let lines: Vec<&str> = body
        .as_bytes()
        .chunks(64)
        .map(|chunk| std::str::from_utf8(chunk).expect("base64 is ASCII"))
        .collect();
    format!(
        "-----BEGIN {label}-----\n{}\n-----END {label}-----\n",
        lines.join("\n")
    )
}

/// The RSAPrivateKey inside a PKCS#8 PrivateKeyInfo:
/// SEQUENCE { INTEGER version, SEQUENCE algorithm, OCTET STRING privateKey }.
fn pkcs1_of(pkcs8: &[u8]) -> &[u8] {
    fn element(der: &[u8]) -> (u8, &[u8], &[u8]) {
        let (tag, first) = (der[0], der[1] as usize);
        let (length, start) = if first < 0x80 {
            (first, 2)
        } else {
            let count = first & 0x7f;
            let length = der[2..2 + count]
                .iter()
                .fold(0usize, |acc, b| (acc << 8) | *b as usize);
            (length, 2 + count)
        };
        (tag, &der[start..start + length], &der[start + length..])
    }
    let (_, info, _) = element(pkcs8);
    let (_, _, rest) = element(info);
    let (_, _, rest) = element(rest);
    let (tag, private_key, _) = element(rest);
    assert_eq!(tag, 0x04, "PKCS#8 private key is an OCTET STRING");
    private_key
}
