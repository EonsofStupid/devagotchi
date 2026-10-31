//! `auth.mode hub` (D25, §14): the hub signs people in with its own accounts and passkeys,
//! with Nomen ("Sign in with Shippin") an optional extra.

use super::*;

#[cfg(unix)]
const KEY: &str = "/var/lib/devagotchi-hub/hub-key.json";
#[cfg(windows)]
const KEY: &str = r"D:\devagotchi-hub\hub-key.json";

fn hub() -> serde_json::Value {
    serde_json::json!({
        "listen": "127.0.0.1:8787",
        "public_url": "https://app.devagotchi.com",
        "auth": { "mode": "hub" },
        "brokers": [ { "name": "wi1", "link": true } ],
        "grants": []
    })
}

fn parse(value: &serde_json::Value) -> BridgeResult<HubConfig> {
    HubConfig::parse(&value.to_string())
}

fn refused(value: &serde_json::Value) -> String {
    let error = parse(value).expect_err("refused");
    assert_eq!(error.code, "invalid_config");
    error.message
}

#[test]
fn a_hub_needs_no_identity_provider_and_no_owner_in_its_config() {
    let config = parse(&hub()).unwrap();
    assert!(config.has_accounts());
    assert!(config.auth.nomen().is_none());
    assert_eq!(config.owner_id(), None);
    assert_eq!(config.rp_id().as_deref(), Some("app.devagotchi.com"));

    // Pages are part of hub mode: no `web` section is needed, and the Nomen web app's
    // section is refused until "Sign in with Shippin" (32e) uses it.
    let mut web = hub();
    web["web"] = serde_json::json!({ "client_id": "devagotchi-web" });
    assert!(refused(&web).contains("web"));

    let mut extra = hub();
    extra["auth"]["issuer"] = serde_json::json!("https://id.shippin.cloud");
    refused(&extra);
}

#[test]
fn passkeys_need_a_secure_origin_and_belong_to_the_hub_or_a_parent_domain() {
    let mut parent = hub();
    parent["auth"]["rp_id"] = serde_json::json!("devagotchi.com");
    assert_eq!(
        parse(&parent).unwrap().rp_id().as_deref(),
        Some("devagotchi.com")
    );
    for wrong in [
        "example.com",
        "evil-devagotchi.com",
        "com",
        "DEVAGOTCHI.COM",
        "devagotchi.com.",
        "",
    ] {
        let mut config = hub();
        config["auth"]["rp_id"] = serde_json::json!(wrong);
        refused(&config);
    }

    let mut plain = hub();
    plain["public_url"] = serde_json::json!("http://hub.example");
    refused(&plain);
    plain["public_url"] = serde_json::json!("http://localhost:8787");
    assert_eq!(
        parse(&plain).unwrap().rp_id().as_deref(),
        Some("localhost"),
        "plain http only on loopback, for tests"
    );
}

#[test]
fn nomen_is_an_optional_block_checked_like_nomen_mode() {
    let mut with_nomen = hub();
    with_nomen["auth"]["nomen"] = serde_json::json!({
        "issuer": "https://id.shippin.cloud",
        "audiences": ["300000000000000500"],
        "clients": ["300000000000000600"],
        "key_file": KEY,
    });
    let config = parse(&with_nomen).unwrap();
    let nomen = config.auth.nomen().expect("the nomen block");
    assert_eq!(nomen.issuer, "https://id.shippin.cloud");
    assert_eq!(nomen.audiences, ["300000000000000500"]);
    assert_eq!(nomen.key_file, KEY);

    for (field, value) in [
        ("issuer", serde_json::json!("http://id.shippin.cloud")),
        ("audiences", serde_json::json!([])),
        ("clients", serde_json::json!([" "])),
        ("key_file", serde_json::json!("hub-key.json")),
        ("secret", serde_json::json!("x")),
    ] {
        let mut config = with_nomen.clone();
        config["auth"]["nomen"][field] = value;
        refused(&config);
    }
}

#[test]
fn a_configured_owner_keeps_its_id_and_is_the_only_owner_the_hub_learns() {
    // The Texas owner keeps the subject every board, machine and audit row is keyed by.
    let mut texas = hub();
    texas["owner"] = serde_json::json!("395290028818825218");
    let config = parse(&texas).unwrap();
    assert_eq!(config.owner_id(), Some("395290028818825218"));
    assert!(config.learn_owner("395290028818825218").is_ok());
    assert!(
        config
            .learn_owner("acct_aaaaaaaaaaaaaaaaaaaaaaaaaa")
            .is_err()
    );
    assert!(!config.is_owner("acct_aaaaaaaaaaaaaaaaaaaaaaaaaa"));

    let mut email = hub();
    email["owner"] = serde_json::json!("jessay@gmail.com");
    refused(&email);
    let mut path = hub();
    path["owner"] = serde_json::json!("../etc");
    refused(&path);
}

#[test]
fn an_owner_learned_from_the_accounts_store_reaches_everything_at_once() {
    let mut config = hub();
    config["boards"] = serde_json::json!([]);
    let config = parse(&config).unwrap();
    let shared = config.clone();
    assert!(!config.is_owner("acct_aaaaaaaaaaaaaaaaaaaaaaaaaa"));
    config
        .learn_owner("acct_aaaaaaaaaaaaaaaaaaaaaaaaaa")
        .unwrap();
    // Every holder of the config sees the owner the moment it is learned.
    assert!(shared.is_owner("acct_aaaaaaaaaaaaaaaaaaaaaaaaaa"));
    assert!(shared.is_human("acct_aaaaaaaaaaaaaaaaaaaaaaaaaa"));
    assert_eq!(
        shared.brokers_for("acct_aaaaaaaaaaaaaaaaaaaaaaaaaa").len(),
        1
    );
    assert_eq!(
        shared.people_for_board("anything"),
        ["acct_aaaaaaaaaaaaaaaaaaaaaaaaaa"]
    );
    // Once learned, never another.
    assert!(
        config
            .learn_owner("acct_bbbbbbbbbbbbbbbbbbbbbbbbbb")
            .is_err()
    );
    assert!(
        config
            .learn_owner("acct_aaaaaaaaaaaaaaaaaaaaaaaaaa")
            .is_ok()
    );
}

#[test]
fn other_modes_have_no_accounts_of_their_own() {
    let mut dev = hub();
    dev["public_url"] = serde_json::json!("http://127.0.0.1:8787");
    #[cfg(unix)]
    let tokens = "/tmp/tokens.json";
    #[cfg(windows)]
    let tokens = r"D:\tmp\tokens.json";
    dev["auth"] = serde_json::json!({ "mode": "dev", "tokens_file": tokens });
    let config = parse(&dev).unwrap();
    assert!(!config.has_accounts());
    assert_eq!(config.rp_id(), None);
}

#[test]
fn a_hub_without_nomen_takes_no_other_issuers_tokens() {
    // Their `sub` would be compared with the hub's own account ids.
    let mut trusting = hub();
    trusting["trusted_issuers"] =
        serde_json::json!([{ "issuer": "https://x.example", "audiences": ["a"] }]);
    assert!(refused(&trusting).contains("trusted_issuers"));
}
