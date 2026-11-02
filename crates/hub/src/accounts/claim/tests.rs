use super::*;
use crate::{
    accounts::{Alg, Ceremony, NewCredential},
    store::testing,
};

const NOW: u64 = 1_800_000_000;
const CONFIGURED: &str = "395290028818825218";

async fn stores() -> Vec<(testing::TestStore, Accounts)> {
    let mut out = Vec::new();
    for store in testing::stores().await {
        let accounts = Accounts::open(store.store.clone()).await.unwrap();
        out.push((store, accounts));
    }
    out
}

/// A passkey for `account` as `verify_registration` hands it over, against a challenge
/// made now.
fn credential(accounts: &Accounts, account: &str, id: u8) -> NewCredential {
    NewCredential {
        id: vec![id; 16],
        public_key: vec![0x30, id],
        alg: Alg::Es256,
        sign_count: 0,
        backup_eligible: true,
        challenge: accounts.challenge(Ceremony::Register, Some(account)),
    }
}

async fn issue(accounts: &Accounts, seed: u8, now: u64) -> ClaimCode {
    accounts
        .issue_claim_code_at([seed; CODE_BYTES], now)
        .await
        .unwrap()
        .expect("an unclaimed hub prints a code")
}

async fn claim(accounts: &Accounts, code: &str, now: u64) -> BridgeResult<(Account, String)> {
    accounts
        .claim_at(
            code,
            None,
            "Jesse",
            [7; 16],
            crate::link::random::<32>(),
            now,
        )
        .await
}

async fn rows(accounts: &Accounts) -> i64 {
    sqlx::query("SELECT COUNT(*) FROM claim_codes")
        .fetch_one(accounts.store().pool())
        .await
        .unwrap()
        .try_get::<i64, _>(0)
        .unwrap()
}

#[test]
fn codes_are_crockford_base32_in_groups_and_read_leniently() {
    let code = encode([0; CODE_BYTES]);
    assert_eq!(code, "0000-0000-0000-0000-0000-0000");
    let code = encode([0xff; CODE_BYTES]);
    assert_eq!(code, "ZZZZ-ZZZZ-ZZZZ-ZZZZ-ZZZZ-ZZZZ");
    let code = encode(*b"devagotchi-hub!");
    assert_eq!(code.len(), 29);
    let plain = code.replace('-', "");
    assert_eq!(normalize(&code).as_deref(), Some(plain.as_str()));
    // Lower case, spaces for dashes, none at all, and the letters people mistake.
    assert_eq!(
        normalize(&code.to_lowercase()).as_deref(),
        Some(plain.as_str())
    );
    assert_eq!(
        normalize(&code.replace('-', " ")).as_deref(),
        Some(plain.as_str())
    );
    assert_eq!(
        normalize("oooo-iiii-llll-0000-1111-2222").as_deref(),
        Some("000011111111000011112222")
    );
    for bad in [
        "",
        "0000-0000-0000-0000-0000-000",
        "0000-0000-0000-0000-0000-00000",
        "UUUU-0000-0000-0000-0000-0000",
        "0000-0000-0000-0000-0000-000é",
        "0000_0000_0000_0000_0000_0000",
    ] {
        assert_eq!(normalize(bad), None, "{bad:?}");
    }
}

#[test]
fn a_claim_code_never_prints_itself() {
    let code = ClaimCode {
        code: "SECRET".into(),
        expires_at: NOW,
    };
    assert!(!format!("{code:?}").contains("SECRET"));
}

#[tokio::test]
async fn the_code_claims_the_hub_until_the_owner_has_a_passkey() {
    for (_store, accounts) in stores().await {
        assert!(!accounts.claimed().await.unwrap());
        let printed = issue(&accounts, 1, NOW).await;
        assert_eq!(printed.expires_at, NOW + 24 * 3600);
        assert_eq!(rows(&accounts).await, 1);

        assert_eq!(
            claim(&accounts, &encode([2; CODE_BYTES]), NOW)
                .await
                .unwrap_err()
                .code,
            "wrong_code"
        );
        assert_eq!(
            claim(&accounts, "not a code", NOW).await.unwrap_err().code,
            "wrong_code"
        );
        assert!(
            accounts.owner().await.unwrap().is_none(),
            "a wrong code makes nothing"
        );

        // Typed back in lower case with spaces: still the code.
        let typed = printed.code.to_lowercase().replace('-', " ");
        let (owner, first) = claim(&accounts, &typed, NOW).await.unwrap();
        assert!(owner.owner);
        assert_eq!(owner.name, "Jesse");
        assert_eq!(owner.id, new_account_id([7; 16]));
        assert_eq!(
            accounts
                .check_link_at(LinkPurpose::Owner, &first, NOW)
                .await
                .unwrap(),
            owner.id
        );

        // A cancelled prompt or a closed tab: the same code claims again, under the
        // name given now, and the earlier link stops working.
        let (again, second) = accounts
            .claim_at(&printed.code, None, "  J.  ", [8; 16], [9; 32], NOW + 60)
            .await
            .unwrap();
        assert_eq!(
            (again.id.as_str(), again.name.as_str()),
            (owner.id.as_str(), "J.")
        );
        assert_eq!(accounts.owner().await.unwrap().unwrap().name, "J.");
        assert!(
            accounts
                .check_link_at(LinkPurpose::Owner, &first, NOW + 60)
                .await
                .is_err()
        );

        // The owner's first passkey: claimed, the code gone, and no new one printed.
        let key = credential(&accounts, &owner.id, 1);
        accounts
            .register_with_link(&owner.id, &key, "phone", LinkPurpose::Owner, &second)
            .await
            .unwrap();
        assert!(accounts.claimed().await.unwrap());
        assert_eq!(rows(&accounts).await, 0);
        assert!(
            accounts
                .issue_claim_code_at([3; CODE_BYTES], NOW)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            claim(&accounts, &printed.code, NOW + 120)
                .await
                .unwrap_err()
                .code,
            "claimed"
        );
    }
}

#[tokio::test]
async fn only_the_newest_unexpired_code_works() {
    for (_store, accounts) in stores().await {
        let old = issue(&accounts, 1, NOW).await;
        let new = issue(&accounts, 2, NOW + 10).await;
        assert_eq!(rows(&accounts).await, 1);
        assert_eq!(
            claim(&accounts, &old.code, NOW + 20)
                .await
                .unwrap_err()
                .code,
            "wrong_code"
        );
        assert_eq!(
            claim(&accounts, &new.code, new.expires_at)
                .await
                .unwrap_err()
                .code,
            "wrong_code",
            "a code stops working when it expires"
        );
        assert!(
            claim(&accounts, &new.code, new.expires_at - 1)
                .await
                .is_ok()
        );
    }
}

#[tokio::test]
async fn the_lifetime_is_the_configs() {
    for (store, _) in stores().await {
        let accounts = Accounts::open(store.store.clone())
            .await
            .unwrap()
            .with_tokens(crate::config::TokenConfig {
                claim_secs: 600,
                ..Default::default()
            });
        assert_eq!(issue(&accounts, 1, NOW).await.expires_at, NOW + 600);
    }
}

#[tokio::test]
async fn a_configured_or_existing_owner_keeps_their_id() {
    for (_store, accounts) in stores().await {
        // The Texas case: the config names the owner from an earlier identity provider.
        let code = issue(&accounts, 1, NOW).await.code;
        let (owner, _) = accounts
            .claim_at(&code, Some(CONFIGURED), "Jesse", [7; 16], [1; 32], NOW)
            .await
            .unwrap();
        assert_eq!(owner.id, CONFIGURED);
        // A config naming someone else is refused, and changes nothing.
        let refused = accounts
            .claim_at(
                &code,
                Some("acct_bbbbbbbbbbbbbbbbbbbbbbbbbb"),
                "X",
                [7; 16],
                [2; 32],
                NOW,
            )
            .await
            .unwrap_err();
        assert_eq!(refused.code, "owner_mismatch");
        assert_eq!(accounts.owner().await.unwrap().unwrap().name, "Jesse");
    }
    for (_store, accounts) in stores().await {
        // An owner made by `devagotchi hub owner` who never made a passkey is claimed as is.
        let made = accounts.ensure_owner(None, "Owner", [5; 16]).await.unwrap();
        let code = issue(&accounts, 1, NOW).await.code;
        let (owner, _) = claim(&accounts, &code, NOW).await.unwrap();
        assert_eq!((owner.id, owner.name), (made.id, "Jesse".to_string()));
    }
}

#[tokio::test]
async fn names_are_checked_before_anything_changes() {
    for (_store, accounts) in stores().await {
        let code = issue(&accounts, 1, NOW).await.code;
        for bad in ["", "   ", "a\nb", &"x".repeat(65)] {
            let refused = accounts
                .claim_at(&code, None, bad, [7; 16], [1; 32], NOW)
                .await
                .unwrap_err();
            assert_eq!(refused.code, "invalid_account", "{bad:?}");
        }
        assert!(accounts.owner().await.unwrap().is_none());
    }
}

#[tokio::test]
async fn a_passkey_through_the_owner_command_also_ends_the_code() {
    // `devagotchi hub owner` on the host is the other way to make the first passkey.
    for (_store, accounts) in stores().await {
        let code = issue(&accounts, 1, NOW).await.code;
        let owner = accounts.ensure_owner(None, "Owner", [5; 16]).await.unwrap();
        let link = accounts
            .issue_link(LinkPurpose::Owner, &owner.id)
            .await
            .unwrap();
        let key = credential(&accounts, &owner.id, 1);
        accounts
            .register_with_link(&owner.id, &key, "laptop", LinkPurpose::Owner, &link)
            .await
            .unwrap();
        assert_eq!(rows(&accounts).await, 0);
        assert_eq!(
            claim(&accounts, &code, NOW).await.unwrap_err().code,
            "claimed"
        );
    }
}
