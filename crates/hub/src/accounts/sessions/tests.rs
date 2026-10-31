use super::*;

const A: &str = "acct_aaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "acct_bbbbbbbbbbbbbbbbbbbbbbbbbb";
const T: u64 = 1_000_000;
const DAY: u64 = 86_400;

fn store() -> (tempfile::TempDir, Accounts) {
    let dir = tempfile::tempdir().unwrap();
    let accounts = Accounts::open(&dir.path().join("accounts.sqlite3")).unwrap();
    accounts.create(A, "Jesse", true).unwrap();
    accounts.create(B, "Friend", false).unwrap();
    (dir, accounts)
}

/// A credential as `verify_registration` returns it, for a register challenge made now.
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

/// A passkey for `account`, stored without a link (test set-up only).
fn passkey(accounts: &Accounts, account: &str, id: u8) -> Vec<u8> {
    accounts
        .store_passkey(
            account,
            &credential(accounts, account, id),
            "phone",
            None,
            now_secs(),
        )
        .unwrap();
    vec![id; 16]
}

fn who(accounts: &Accounts, token: &str, now: u64) -> Option<String> {
    accounts
        .session_at(token, now)
        .unwrap()
        .map(|session| session.account.id)
}

#[test]
fn a_session_names_its_account_and_is_stored_only_as_a_hash() {
    let (dir, accounts) = store();
    let key = passkey(&accounts, A, 1);
    let token = accounts.open_session_at(A, Some(&key), [5; 32], T).unwrap();
    assert!(token.starts_with("dgs_") && token.len() <= 128, "{token}");
    assert_eq!(who(&accounts, &token, T + 1), Some(A.to_string()));
    let session = accounts.session_at(&token, T + 1).unwrap().unwrap();
    assert_eq!(session.account.name, "Jesse");
    assert_eq!(session.created_at, T);

    accounts
        .connection
        .lock()
        .unwrap()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    let mut raw = std::fs::read(dir.path().join("accounts.sqlite3")).unwrap();
    raw.extend(std::fs::read(dir.path().join("accounts.sqlite3-wal")).unwrap_or_default());
    let contains = |needle: &[u8]| raw.windows(needle.len()).any(|w| w == needle);
    assert!(contains(hash(&token).as_bytes()));
    assert!(
        !contains(token.as_bytes()),
        "the session id itself is never stored"
    );

    for wrong in ["", "dgs_", "dgs_nonsense", &token[..token.len() - 1]] {
        assert_eq!(who(&accounts, wrong, T + 1), None, "{wrong:?}");
    }
}

#[test]
fn a_session_ends_when_idle_at_its_limit_or_when_ended() {
    let (_dir, accounts) = store();
    let idle = accounts.open_session_at(A, None, [1; 32], T).unwrap();
    let used = accounts.open_session_at(A, None, [2; 32], T).unwrap();
    // Used every 20 days: never idle, but over at 90 days however it is used.
    let mut now = T;
    while now + 20 * DAY < T + SESSION_MAX_SECS {
        now += 20 * DAY;
        assert_eq!(
            who(&accounts, &used, now),
            Some(A.to_string()),
            "day {}",
            (now - T) / DAY
        );
    }
    assert_eq!(who(&accounts, &used, T + SESSION_MAX_SECS), None);
    assert_eq!(
        who(&accounts, &idle, T + SESSION_IDLE_SECS),
        None,
        "30 days unused"
    );

    let one = accounts.open_session_at(A, None, [3; 32], T).unwrap();
    let two = accounts.open_session_at(A, None, [4; 32], T).unwrap();
    let theirs = accounts.open_session_at(B, None, [6; 32], T).unwrap();
    assert!(accounts.end_session(&one).unwrap());
    assert!(!accounts.end_session(&one).unwrap(), "already ended");
    assert_eq!(who(&accounts, &one, T + 1), None);
    assert_eq!(who(&accounts, &two, T + 1), Some(A.to_string()));
    assert_eq!(accounts.end_sessions(A).unwrap(), 1);
    assert_eq!(who(&accounts, &two, T + 1), None);
    assert_eq!(
        who(&accounts, &theirs, T + 1),
        Some(B.to_string()),
        "only A's ended"
    );
}

#[test]
fn removing_a_passkey_or_disabling_the_account_ends_its_sessions() {
    let (_dir, accounts) = store();
    let phone = passkey(&accounts, A, 1);
    let laptop = passkey(&accounts, A, 2);
    let by_phone = accounts
        .open_session_at(A, Some(&phone), [1; 32], T)
        .unwrap();
    let by_laptop = accounts
        .open_session_at(A, Some(&laptop), [2; 32], T)
        .unwrap();
    assert!(accounts.remove_passkey(A, &phone).unwrap());
    assert_eq!(who(&accounts, &by_phone, T + 1), None);
    assert_eq!(who(&accounts, &by_laptop, T + 1), Some(A.to_string()));

    accounts
        .connection
        .lock()
        .unwrap()
        .execute(
            "UPDATE accounts SET disabled_at = 1 WHERE id = ?1",
            rusqlite::params![A],
        )
        .unwrap();
    assert_eq!(who(&accounts, &by_laptop, T + 1), None);
    assert_eq!(
        accounts
            .open_session_at(A, Some(&laptop), [3; 32], T)
            .unwrap_err()
            .code,
        "account_disabled"
    );
}

#[test]
fn spending_a_link_ends_the_accounts_other_sessions() {
    // `devagotchi hub owner` run again: the new passkey's session is the owner's only one.
    let (_dir, accounts) = store();
    let now = now_secs();
    let before = accounts.open_session_at(A, None, [1; 32], now).unwrap();
    let theirs = accounts.open_session_at(B, None, [2; 32], now).unwrap();
    let link = accounts.issue_link(LinkPurpose::Owner, A).unwrap();
    accounts
        .add_passkey(
            A,
            &credential(&accounts, A, 7),
            "new phone",
            Authority::Link(LinkPurpose::Owner, &link),
        )
        .unwrap();
    assert_eq!(who(&accounts, &before, now), None);
    assert_eq!(who(&accounts, &theirs, now), Some(B.to_string()));
}

#[test]
fn an_account_keeps_a_bounded_number_of_sessions() {
    let (_dir, accounts) = store();
    let mut tokens = Vec::new();
    for i in 0..MAX_SESSIONS_PER_ACCOUNT {
        let mut random = [0u8; 32];
        random[..8].copy_from_slice(&(i as u64).to_be_bytes());
        tokens.push(
            accounts
                .open_session_at(A, None, random, T + i as u64)
                .unwrap(),
        );
    }
    // The oldest is used again, so the least recently used one makes room.
    assert_eq!(who(&accounts, &tokens[0], T + 10_000), Some(A.to_string()));
    let newest = accounts
        .open_session_at(A, None, [0xee; 32], T + 10_001)
        .unwrap();
    assert_eq!(who(&accounts, &newest, T + 10_002), Some(A.to_string()));
    assert_eq!(who(&accounts, &tokens[0], T + 10_002), Some(A.to_string()));
    assert_eq!(
        who(&accounts, &tokens[1], T + 10_002),
        None,
        "least recently used"
    );
}

#[test]
fn a_link_adds_the_passkey_and_opens_its_session_together_or_not_at_all() {
    let (_dir, accounts) = store();
    let link = accounts.issue_link(LinkPurpose::Owner, A).unwrap();
    // The session cannot be stored: nothing else happens either.
    accounts
        .connection
        .lock()
        .unwrap()
        .execute_batch(
            "CREATE TEMP TRIGGER no_sessions BEFORE INSERT ON sessions
             BEGIN SELECT RAISE(ABORT, 'no sessions'); END;",
        )
        .unwrap();
    assert!(
        accounts
            .register_with_link(
                A,
                &credential(&accounts, A, 7),
                "phone",
                LinkPurpose::Owner,
                &link
            )
            .is_err()
    );
    assert_eq!(
        accounts.check_link(LinkPurpose::Owner, &link).unwrap(),
        A,
        "still live"
    );
    assert!(accounts.passkeys_of(A).unwrap().is_empty());
    accounts
        .connection
        .lock()
        .unwrap()
        .execute_batch("DROP TRIGGER no_sessions;")
        .unwrap();
    let token = accounts
        .register_with_link(
            A,
            &credential(&accounts, A, 7),
            "phone",
            LinkPurpose::Owner,
            &link,
        )
        .unwrap();
    assert_eq!(who(&accounts, &token, now_secs()), Some(A.to_string()));
}

#[test]
fn a_counter_that_goes_back_ends_the_sessions_that_passkey_opened() {
    // The race the verifier cannot see: the store holds a newer counter than the one the
    // page read. The sign-in is refused and the passkey's sessions end; others stay.
    let (_dir, accounts) = store();
    let mut counted = credential(&accounts, A, 1);
    counted.backup_eligible = false;
    accounts
        .store_passkey(A, &counted, "key", None, now_secs())
        .unwrap();
    let other = passkey(&accounts, A, 2);
    let by_key = accounts.open_session(A, Some(&[1; 16])).unwrap();
    let by_other = accounts.open_session(A, Some(&other)).unwrap();
    let at = |count: u32| Asserted {
        id: vec![1; 16],
        public_key: vec![0x30, 1],
        alg: Alg::Es256,
        backup_eligible: false,
        sign_count: count,
        user_handle: A.as_bytes().to_vec(),
        challenge: accounts.challenge(Ceremony::SignIn, None),
    };
    accounts.sign_in(&at(5)).unwrap();
    let copy = at(5);
    assert_eq!(
        accounts.sign_in(&copy).unwrap_err().code,
        "signature_counter"
    );
    assert_eq!(who(&accounts, &by_key, now_secs()), None);
    assert_eq!(who(&accounts, &by_other, now_secs()), Some(A.to_string()));
    // Its challenge was spent all the same: replaying it ends nothing more.
    assert_eq!(
        accounts.sign_in(&copy).unwrap_err().code,
        "invalid_ceremony"
    );
}
