use super::*;

fn store() -> (tempfile::TempDir, Accounts) {
    let dir = tempfile::tempdir().unwrap();
    let accounts = Accounts::open(&dir.path().join("accounts.sqlite3")).unwrap();
    (dir, accounts)
}

const A: &str = "acct_aaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "acct_bbbbbbbbbbbbbbbbbbbbbbbbbb";

/// A challenge for registering a passkey on `account`, made now.
fn register(accounts: &Accounts, account: &str) -> Vec<u8> {
    accounts.challenge(Ceremony::Register, Some(account))
}

fn sign_in(accounts: &Accounts) -> Vec<u8> {
    accounts.challenge(Ceremony::SignIn, None)
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
    let owner = accounts.create(A, "Jesse", true).unwrap();
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
    // Everything the store wrote, the write-ahead log included, holds the link's hash
    // and never the link.
    accounts
        .connection
        .lock()
        .unwrap()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    let mut raw = std::fs::read(&path).unwrap();
    raw.extend(std::fs::read(dir.path().join("accounts.sqlite3-wal")).unwrap_or_default());
    let contains = |needle: &[u8]| raw.windows(needle.len()).any(|w| w == needle);
    assert!(contains(hash(&token).as_bytes()), "the hash is stored");
    assert!(!contains(token.as_bytes()), "the link itself is not");
    accounts
        .add_passkey(
            &owner.id,
            &credential(1),
            "phone",
            Some((LinkPurpose::Owner, &token)),
            &register(&accounts, &owner.id),
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
                Some((LinkPurpose::Owner, &token)),
                &register(&accounts, &owner.id),
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
    let owner = accounts.create(A, "Jesse", true).unwrap();
    let plain = accounts.create(B, "Friend", false).unwrap();
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
fn a_refused_passkey_leaves_the_link_unspent() {
    let (_dir, accounts) = store();
    let owner = accounts.create(A, "Jesse", true).unwrap();
    let friend = accounts.create(B, "Friend", false).unwrap();
    accounts
        .add_passkey(
            &friend.id,
            &credential(1),
            "theirs",
            None,
            &register(&accounts, B),
        )
        .unwrap();
    let link = accounts
        .issue_link(LinkPurpose::Owner, &owner.id, [3; 32])
        .unwrap();
    let with_link = |account: &str, id: u8| {
        accounts
            .add_passkey(
                account,
                &credential(id),
                "phone",
                Some((LinkPurpose::Owner, &link)),
                &register(&accounts, account),
            )
            .map_err(|error| error.code)
    };
    // A passkey already registered elsewhere: refused, and the link still works.
    assert_eq!(with_link(A, 1), Err("passkey_exists".into()));
    // The owner's link spent on someone else's account: refused, and still the owner's.
    assert_eq!(with_link(B, 2), Err("invalid_link".into()));
    assert_eq!(accounts.check_link(LinkPurpose::Owner, &link).unwrap(), A);
    assert_eq!(with_link(A, 3), Ok(()));

    // A link that ran out between opening the page and finishing.
    let late = accounts
        .issue_link_at(LinkPurpose::Owner, &owner.id, [4; 32], 1_000)
        .unwrap();
    let challenge =
        accounts.challenge_at(Ceremony::Register, Some(A), [4; 16], 1_000 + LINK_TTL_SECS);
    assert_eq!(
        accounts
            .add_passkey_at(
                A,
                &credential(4),
                "phone",
                Some((LinkPurpose::Owner, &late)),
                &challenge,
                1_000 + LINK_TTL_SECS,
            )
            .unwrap_err()
            .code,
        "invalid_link"
    );
}

#[test]
fn challenges_are_the_hubs_own_bound_to_their_purpose_and_spent_once() {
    let (dir, accounts) = store();
    accounts.create(A, "Jesse", true).unwrap();
    accounts.create(B, "Friend", false).unwrap();
    let add = |account: &str, id: u8, challenge: &[u8], now: u64| {
        accounts
            .add_passkey_at(account, &credential(id), "phone", None, challenge, now)
            .map_err(|error| error.code)
    };
    let refused = Err("invalid_ceremony".to_string());

    let challenge = accounts.challenge_at(Ceremony::Register, Some(A), [1; 16], 100);
    assert!(challenge.len() >= 32, "at least as long as WebAuthn asks");
    assert_eq!(add(A, 1, &challenge, 101), Ok(()));
    assert_eq!(add(A, 2, &challenge, 102), refused, "used once");
    // For another account, for signing in, or after five minutes: refused.
    let challenge = accounts.challenge_at(Ceremony::Register, Some(A), [2; 16], 100);
    assert_eq!(add(B, 3, &challenge, 101), refused);
    let sign_in = accounts.challenge_at(Ceremony::SignIn, None, [3; 16], 100);
    assert_eq!(add(A, 4, &sign_in, 101), refused);
    assert_eq!(add(A, 5, &challenge, 100 + CHALLENGE_TTL_SECS), refused);
    // Made up, altered, or made by another hub: refused.
    let mut altered = challenge.clone();
    let last = altered.len() - 1;
    altered[last] ^= 1;
    assert_eq!(add(A, 6, &altered, 101), refused);
    assert_eq!(add(A, 7, &[0u8; 57], 101), refused);
    assert_eq!(add(A, 8, &[], 101), refused);
    let (_other_dir, other) = store();
    let foreign = other.challenge_at(Ceremony::Register, Some(A), [2; 16], 100);
    assert_eq!(add(A, 9, &foreign, 101), refused);
    assert_eq!(
        add(A, 10, &challenge, 101),
        Ok(()),
        "still good after all that"
    );

    // Asking for challenges writes nothing: a flood of unanswered sign-ins costs no storage
    // and blocks nobody.
    let before = std::fs::metadata(dir.path().join("accounts.sqlite3-wal"))
        .map(|m| m.len())
        .unwrap_or(0);
    for i in 0..1_000u16 {
        let mut nonce = [0u8; 16];
        nonce[..2].copy_from_slice(&i.to_be_bytes());
        accounts.challenge_at(Ceremony::SignIn, None, nonce, 100);
    }
    let after = std::fs::metadata(dir.path().join("accounts.sqlite3-wal"))
        .map(|m| m.len())
        .unwrap_or(0);
    assert_eq!(before, after);
}

#[test]
fn passkeys_belong_to_one_account() {
    let (_dir, accounts) = store();
    let jesse = accounts.create(A, "Jesse", true).unwrap();
    let friend = accounts.create(B, "Friend", false).unwrap();
    let add = |account: &str, id: u8, label: &str| {
        accounts.add_passkey(
            account,
            &credential(id),
            label,
            None,
            &register(&accounts, account),
        )
    };
    add(&jesse.id, 1, "phone").unwrap();
    add(&jesse.id, 2, "laptop").unwrap();
    assert_eq!(
        add(&friend.id, 1, "stolen").unwrap_err().code,
        "passkey_exists"
    );
    assert_eq!(
        accounts
            .add_passkey(
                "acct_cccccccccccccccccccccccccc",
                &credential(3),
                "x",
                None,
                &register(&accounts, "acct_cccccccccccccccccccccccccc"),
            )
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
fn signing_in_moves_the_counter_forward_once_and_only_for_a_live_account() {
    let (_dir, accounts) = store();
    accounts.create(A, "Jesse", true).unwrap();
    accounts.create(B, "Friend", false).unwrap();
    let mut counted = credential(1);
    counted.backup_eligible = false;
    accounts
        .add_passkey(A, &counted, "key", None, &register(&accounts, A))
        .unwrap();
    accounts
        .add_passkey(A, &credential(2), "synced", None, &register(&accounts, A))
        .unwrap();
    let go = |id: u8, count: u32, handle: Option<&[u8]>| {
        accounts
            .sign_in(&credential(id).id, count, handle, &sign_in(&accounts))
            .map(|account| account.id)
            .map_err(|error| error.code)
    };

    assert_eq!(go(1, 7, Some(A.as_bytes())), Ok(A.to_string()));
    assert_eq!(
        accounts
            .passkey(&credential(1).id)
            .unwrap()
            .unwrap()
            .sign_count,
        7
    );
    // Two sign-ins that both read 7: the second to store finds the counter moved.
    assert_eq!(go(1, 7, None), Err("signature_counter".into()));
    assert_eq!(go(1, 6, None), Err("signature_counter".into()));
    assert_eq!(go(1, 8, None), Ok(A.to_string()));
    // A passkey that keeps no counter signs in with 0 every time.
    assert_eq!(go(2, 0, None), Ok(A.to_string()));
    assert_eq!(go(2, 0, None), Ok(A.to_string()));
    // The user handle the authenticator returns must be the passkey's own account.
    assert_eq!(go(2, 0, Some(B.as_bytes())), Err("user_handle".into()));
    // One challenge, one sign-in; a registration challenge is not a sign-in one.
    let challenge = sign_in(&accounts);
    accounts
        .sign_in(&credential(2).id, 0, None, &challenge)
        .unwrap();
    assert_eq!(
        accounts
            .sign_in(&credential(2).id, 0, None, &challenge)
            .unwrap_err()
            .code,
        "invalid_ceremony"
    );
    assert_eq!(
        accounts
            .sign_in(&credential(2).id, 0, None, &register(&accounts, A))
            .unwrap_err()
            .code,
        "invalid_ceremony"
    );
    // A disabled account, a removed passkey, or one never registered: no sign-in.
    accounts
        .connection
        .lock()
        .unwrap()
        .execute(
            "UPDATE accounts SET disabled_at = 1 WHERE id = ?1",
            rusqlite::params![A],
        )
        .unwrap();
    assert_eq!(go(2, 0, None), Err("account_disabled".into()));
    assert_eq!(
        accounts
            .add_passkey(A, &credential(5), "new", None, &register(&accounts, A))
            .unwrap_err()
            .code,
        "account_disabled"
    );
    accounts
        .connection
        .lock()
        .unwrap()
        .execute(
            "UPDATE accounts SET disabled_at = NULL WHERE id = ?1",
            rusqlite::params![A],
        )
        .unwrap();
    assert!(accounts.remove_passkey(A, &credential(2).id).unwrap());
    assert_eq!(go(2, 0, None), Err("invalid_passkey".into()));
    assert_eq!(go(9, 1, None), Err("invalid_passkey".into()));
}

#[test]
fn the_store_and_its_challenges_survive_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("accounts.sqlite3");
    let pending = {
        let accounts = Accounts::open(&path).unwrap();
        let owner = accounts.create(A, "Jesse", true).unwrap();
        accounts
            .add_passkey(
                &owner.id,
                &credential(1),
                "phone",
                None,
                &register(&accounts, A),
            )
            .unwrap();
        sign_in(&accounts)
    };
    let accounts = Accounts::open(&path).unwrap();
    assert_eq!(accounts.owner().unwrap().unwrap().name, "Jesse");
    assert!(accounts.passkey(&credential(1).id).unwrap().is_some());
    // A sign-in begun before a restart finishes after it.
    assert_eq!(
        accounts
            .sign_in(&credential(1).id, 1, None, &pending)
            .unwrap()
            .id,
        A
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "accounts.sqlite3 is the hub's alone");
    }
}
