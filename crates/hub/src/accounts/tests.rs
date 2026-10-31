use super::*;

fn store() -> (tempfile::TempDir, Accounts) {
    let dir = tempfile::tempdir().unwrap();
    let accounts = Accounts::open(&dir.path().join("accounts.sqlite3")).unwrap();
    (dir, accounts)
}

fn credential(id: u8) -> NewCredential {
    NewCredential {
        id: vec![id; 16],
        public_key: vec![0x30, id],
        alg: Alg::Es256,
        sign_count: 0,
        backup_eligible: true,
    }
}

#[test]
fn account_ids_are_safe_names() {
    let id = new_account_id([0xff; 16]);
    assert!(id.starts_with("acct_") && id.len() == 5 + 26, "{id}");
    assert!(
        id[5..]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
    );
    assert_ne!(new_account_id([1; 16]), new_account_id([2; 16]));
    assert!(valid_account_id(&id));
    // A subject an identity provider issued earlier stays a valid id (the Texas owner).
    assert!(valid_account_id("395290028818825218"));
    for bad in [
        "",
        "../etc",
        "a/b",
        "acct_short",
        "acct_UPPERCASEUPPERCASEUPPERCA",
        "has space",
        &"x".repeat(65),
    ] {
        assert!(!valid_account_id(bad), "{bad:?} must be refused");
    }
}

#[test]
fn one_owner_and_unique_ids() {
    let (_dir, accounts) = store();
    assert!(accounts.owner().unwrap().is_none());
    let owner = accounts
        .create("acct_aaaaaaaaaaaaaaaaaaaaaaaaaa", "Jesse", true)
        .unwrap();
    assert!(owner.owner);
    assert_eq!(accounts.owner().unwrap().unwrap().id, owner.id);
    assert_eq!(accounts.get(&owner.id).unwrap().unwrap().name, "Jesse");
    assert_eq!(
        accounts
            .create("acct_bbbbbbbbbbbbbbbbbbbbbbbbbb", "Two", true)
            .unwrap_err()
            .code,
        "owner_exists"
    );
    assert_eq!(
        accounts.create(&owner.id, "Again", false).unwrap_err().code,
        "account_exists"
    );
    assert_eq!(
        accounts.create("../x", "Bad", false).unwrap_err().code,
        "invalid_account"
    );
    assert!(
        accounts
            .get("acct_cccccccccccccccccccccccccc")
            .unwrap()
            .is_none()
    );
    assert!(
        !accounts
            .create("395290028818825218", "Legacy", false)
            .unwrap()
            .owner
    );
}

#[test]
fn owner_link_is_one_time_and_stored_only_as_a_hash() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("accounts.sqlite3");
    let accounts = Accounts::open(&path).unwrap();
    let owner = accounts
        .create("acct_aaaaaaaaaaaaaaaaaaaaaaaaaa", "Jesse", true)
        .unwrap();
    let token = accounts
        .issue_link(LinkPurpose::Owner, &owner.id, [7; 32])
        .unwrap();
    assert!(token.starts_with("dgl_"));
    // Checking does not spend it, so a link preview cannot burn it.
    assert_eq!(
        accounts.check_link(LinkPurpose::Owner, &token).unwrap(),
        owner.id
    );
    assert_eq!(
        accounts.check_link(LinkPurpose::Owner, &token).unwrap(),
        owner.id
    );
    let raw = std::fs::read(&path).unwrap();
    assert!(
        !raw.windows(token.len()).any(|w| w == token.as_bytes()),
        "the token itself must not be in the database"
    );
    accounts
        .add_passkey(
            &owner.id,
            &credential(1),
            "phone",
            Some((LinkPurpose::Owner, &token)),
        )
        .unwrap();
    assert_eq!(
        accounts
            .check_link(LinkPurpose::Owner, &token)
            .unwrap_err()
            .code,
        "invalid_link"
    );
    assert_eq!(
        accounts
            .add_passkey(
                &owner.id,
                &credential(2),
                "again",
                Some((LinkPurpose::Owner, &token))
            )
            .unwrap_err()
            .code,
        "invalid_link"
    );
    assert!(
        accounts.passkey(&credential(2).id).unwrap().is_none(),
        "nothing added on refusal"
    );
}

#[test]
fn owner_links_expire_and_supersede() {
    let (_dir, accounts) = store();
    let owner = accounts
        .create("acct_aaaaaaaaaaaaaaaaaaaaaaaaaa", "Jesse", true)
        .unwrap();
    let plain = accounts
        .create("acct_bbbbbbbbbbbbbbbbbbbbbbbbbb", "Friend", false)
        .unwrap();
    assert_eq!(
        accounts
            .issue_link(LinkPurpose::Owner, &plain.id, [1; 32])
            .unwrap_err()
            .code,
        "not_owner"
    );
    let old = accounts
        .issue_link_at(LinkPurpose::Owner, &owner.id, [1; 32], 1_000)
        .unwrap();
    assert_eq!(
        accounts
            .check_link_at(LinkPurpose::Owner, &old, 1_000 + LINK_TTL_SECS - 1)
            .unwrap(),
        owner.id
    );
    assert_eq!(
        accounts
            .check_link_at(LinkPurpose::Owner, &old, 1_000 + LINK_TTL_SECS)
            .unwrap_err()
            .code,
        "invalid_link"
    );
    let new = accounts
        .issue_link_at(LinkPurpose::Owner, &owner.id, [2; 32], 1_010)
        .unwrap();
    assert_eq!(
        accounts
            .check_link_at(LinkPurpose::Owner, &old, 1_020)
            .unwrap_err()
            .code,
        "invalid_link"
    );
    assert_eq!(
        accounts
            .check_link_at(LinkPurpose::Owner, &new, 1_020)
            .unwrap(),
        owner.id
    );
    assert_eq!(
        accounts
            .check_link(LinkPurpose::Owner, "dgl_nonsense")
            .unwrap_err()
            .code,
        "invalid_link"
    );
}

#[test]
fn a_ceremony_is_used_once_for_its_purpose() {
    let (_dir, accounts) = store();
    let id = accounts
        .begin_ceremony_at(
            Ceremony::Register,
            Some("acct_aaaaaaaaaaaaaaaaaaaaaaaaaa"),
            [1; 16],
            [9; 32],
            100,
        )
        .unwrap();
    assert_eq!(
        accounts
            .finish_ceremony_at(Ceremony::SignIn, &id, 101)
            .unwrap_err()
            .code,
        "invalid_ceremony"
    );
    assert_eq!(
        accounts
            .finish_ceremony_at(Ceremony::Register, &id, 101)
            .unwrap(),
        (Some("acct_aaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()), [9; 32])
    );
    assert_eq!(
        accounts
            .finish_ceremony_at(Ceremony::Register, &id, 102)
            .unwrap_err()
            .code,
        "invalid_ceremony"
    );
    let late = accounts
        .begin_ceremony_at(Ceremony::SignIn, None, [2; 16], [8; 32], 100)
        .unwrap();
    assert_eq!(
        accounts
            .finish_ceremony_at(Ceremony::SignIn, &late, 100 + CEREMONY_TTL_SECS)
            .unwrap_err()
            .code,
        "invalid_ceremony"
    );
}

#[test]
fn passkeys_belong_to_one_account() {
    let (_dir, accounts) = store();
    let jesse = accounts
        .create("acct_aaaaaaaaaaaaaaaaaaaaaaaaaa", "Jesse", true)
        .unwrap();
    let friend = accounts
        .create("acct_bbbbbbbbbbbbbbbbbbbbbbbbbb", "Friend", false)
        .unwrap();
    accounts
        .add_passkey(&jesse.id, &credential(1), "phone", None)
        .unwrap();
    accounts
        .add_passkey(&jesse.id, &credential(2), "laptop", None)
        .unwrap();
    assert_eq!(
        accounts
            .add_passkey(&friend.id, &credential(1), "stolen", None)
            .unwrap_err()
            .code,
        "passkey_exists"
    );
    assert_eq!(
        accounts
            .add_passkey("acct_cccccccccccccccccccccccccc", &credential(3), "x", None)
            .unwrap_err()
            .code,
        "no_account"
    );
    let found = accounts.passkey(&credential(1).id).unwrap().unwrap();
    assert_eq!(
        (found.account.as_str(), found.label.as_str()),
        (jesse.id.as_str(), "phone")
    );
    assert_eq!(found.alg, Alg::Es256);
    accounts.used_passkey(&credential(1).id, 7).unwrap();
    assert_eq!(
        accounts
            .passkey(&credential(1).id)
            .unwrap()
            .unwrap()
            .sign_count,
        7
    );
    assert_eq!(accounts.passkeys_of(&jesse.id).unwrap().len(), 2);
    assert!(
        !accounts
            .remove_passkey(&friend.id, &credential(2).id)
            .unwrap(),
        "not the friend's"
    );
    assert!(
        accounts
            .remove_passkey(&jesse.id, &credential(2).id)
            .unwrap()
    );
    assert_eq!(accounts.passkeys_of(&jesse.id).unwrap().len(), 1);
}

#[test]
fn the_store_survives_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("accounts.sqlite3");
    {
        let accounts = Accounts::open(&path).unwrap();
        let owner = accounts
            .create("acct_aaaaaaaaaaaaaaaaaaaaaaaaaa", "Jesse", true)
            .unwrap();
        accounts
            .add_passkey(&owner.id, &credential(1), "phone", None)
            .unwrap();
    }
    let accounts = Accounts::open(&path).unwrap();
    assert_eq!(accounts.owner().unwrap().unwrap().name, "Jesse");
    assert!(accounts.passkey(&credential(1).id).unwrap().is_some());
}

#[test]
fn ceremonies_are_capped() {
    let (_dir, accounts) = store();
    {
        let mut connection = accounts.connection.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        for i in 0..MAX_CEREMONIES {
            transaction
                .execute(
                    "INSERT INTO ceremonies (hash, purpose, account, challenge, expires_at)
                     VALUES (?1, 'sign_in', NULL, x'00', 9999999999)",
                    rusqlite::params![format!("h{i}")],
                )
                .unwrap();
        }
        transaction.commit().unwrap();
    }
    // A flood of unanswered sign-ins cannot grow the store without bound.
    assert_eq!(
        accounts
            .begin_ceremony_at(Ceremony::SignIn, None, [1; 16], [1; 32], 100)
            .unwrap_err()
            .code,
        "busy"
    );
}
