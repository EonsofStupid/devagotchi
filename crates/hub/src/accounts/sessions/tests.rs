use super::*;
use crate::store::{
    Backend,
    testing::{self, TestStore},
};

const A: &str = "acct_aaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "acct_bbbbbbbbbbbbbbbbbbbbbbbbbb";
const T: u64 = 1_000_000;
const DAY: u64 = 86_400;

/// Accounts A (the owner) and B in every store this run has: SQLite always, Postgres when
/// the test server is set.
async fn stores() -> Vec<(TestStore, Accounts)> {
    let mut out = Vec::new();
    for store in testing::stores().await {
        let accounts = Accounts::open(store.store.clone()).await.unwrap();
        accounts.create(A, "Jesse", true).await.unwrap();
        accounts.create(B, "Friend", false).await.unwrap();
        out.push((store, accounts));
    }
    out
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
async fn passkey(accounts: &Accounts, account: &str, id: u8) -> Vec<u8> {
    accounts
        .store_passkey(
            account,
            &credential(accounts, account, id),
            "phone",
            None,
            now_secs(),
        )
        .await
        .unwrap();
    vec![id; 16]
}

async fn who(accounts: &Accounts, token: &str, now: u64) -> Option<String> {
    accounts
        .session_at(token, now)
        .await
        .unwrap()
        .map(|session| session.account.id)
}

#[tokio::test]
async fn a_session_names_its_account_and_is_stored_only_as_a_hash() {
    for (store, accounts) in stores().await {
        let key = passkey(&accounts, A, 1).await;
        let token = accounts
            .open_session_at(A, Some(&key), [5; 32], T)
            .await
            .unwrap();
        assert!(token.starts_with("dgs_") && token.len() <= 128, "{token}");
        assert_eq!(who(&accounts, &token, T + 1).await, Some(A.to_string()));
        let session = accounts.session_at(&token, T + 1).await.unwrap().unwrap();
        assert_eq!(session.account.name, "Jesse");
        assert_eq!(session.created_at, T);

        let stored: Vec<String> = sqlx::query_scalar("SELECT hash FROM sessions")
            .fetch_all(accounts.store().pool())
            .await
            .unwrap();
        assert_eq!(stored, vec![hash(&token)]);
        if store.store.backend() == Backend::Sqlite {
            sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                .execute(accounts.store().pool())
                .await
                .unwrap();
            let mut raw = std::fs::read(store.dir.path().join("accounts.sqlite3")).unwrap();
            raw.extend(
                std::fs::read(store.dir.path().join("accounts.sqlite3-wal")).unwrap_or_default(),
            );
            let contains = |needle: &[u8]| raw.windows(needle.len()).any(|w| w == needle);
            assert!(contains(hash(&token).as_bytes()));
            assert!(
                !contains(token.as_bytes()),
                "the session id itself is never stored"
            );
        }

        for wrong in ["", "dgs_", "dgs_nonsense", &token[..token.len() - 1]] {
            assert_eq!(who(&accounts, wrong, T + 1).await, None, "{wrong:?}");
        }
    }
}

#[tokio::test]
async fn a_session_ends_when_idle_at_its_limit_or_when_ended() {
    for (_store, accounts) in stores().await {
        let idle = accounts.open_session_at(A, None, [1; 32], T).await.unwrap();
        let used = accounts.open_session_at(A, None, [2; 32], T).await.unwrap();
        // Used every 20 days: never idle, but over at 90 days however it is used.
        let mut now = T;
        while now + 20 * DAY < T + SESSION_MAX_SECS {
            now += 20 * DAY;
            assert_eq!(
                who(&accounts, &used, now).await,
                Some(A.to_string()),
                "day {}",
                (now - T) / DAY
            );
        }
        assert_eq!(who(&accounts, &used, T + SESSION_MAX_SECS).await, None);
        assert_eq!(
            who(&accounts, &idle, T + SESSION_IDLE_SECS).await,
            None,
            "30 days unused"
        );

        let one = accounts.open_session_at(A, None, [3; 32], T).await.unwrap();
        let two = accounts.open_session_at(A, None, [4; 32], T).await.unwrap();
        let theirs = accounts.open_session_at(B, None, [6; 32], T).await.unwrap();
        assert!(accounts.end_session(&one).await.unwrap());
        assert!(!accounts.end_session(&one).await.unwrap(), "already ended");
        assert_eq!(who(&accounts, &one, T + 1).await, None);
        assert_eq!(who(&accounts, &two, T + 1).await, Some(A.to_string()));
        assert_eq!(accounts.end_sessions(A).await.unwrap(), 1);
        assert_eq!(who(&accounts, &two, T + 1).await, None);
        assert_eq!(
            who(&accounts, &theirs, T + 1).await,
            Some(B.to_string()),
            "only A's ended"
        );
    }
}

#[tokio::test]
async fn removing_a_passkey_or_disabling_the_account_ends_its_sessions() {
    for (_store, accounts) in stores().await {
        let phone = passkey(&accounts, A, 1).await;
        let laptop = passkey(&accounts, A, 2).await;
        let by_phone = accounts
            .open_session_at(A, Some(&phone), [1; 32], T)
            .await
            .unwrap();
        let by_laptop = accounts
            .open_session_at(A, Some(&laptop), [2; 32], T)
            .await
            .unwrap();
        assert!(accounts.remove_passkey(A, &phone).await.unwrap());
        assert_eq!(who(&accounts, &by_phone, T + 1).await, None);
        assert_eq!(who(&accounts, &by_laptop, T + 1).await, Some(A.to_string()));

        sqlx::query("UPDATE accounts SET disabled_at = 1 WHERE id = $1")
            .bind(A)
            .execute(accounts.store().pool())
            .await
            .unwrap();
        assert_eq!(who(&accounts, &by_laptop, T + 1).await, None);
        assert_eq!(
            accounts
                .open_session_at(A, Some(&laptop), [3; 32], T)
                .await
                .unwrap_err()
                .code,
            "account_disabled"
        );
    }
}

#[tokio::test]
async fn spending_a_link_ends_the_accounts_other_sessions() {
    // `devagotchi hub owner` run again: the new passkey's session is the owner's only one.
    for (_store, accounts) in stores().await {
        let now = now_secs();
        let before = accounts.open_session_at(A, None, [1; 32], now).await.unwrap();
        let theirs = accounts.open_session_at(B, None, [2; 32], now).await.unwrap();
        let link = accounts.issue_link(LinkPurpose::Owner, A).await.unwrap();
        accounts
            .add_passkey(
                A,
                &credential(&accounts, A, 7),
                "new phone",
                Authority::Link(LinkPurpose::Owner, &link),
            )
            .await
            .unwrap();
        assert_eq!(who(&accounts, &before, now).await, None);
        assert_eq!(who(&accounts, &theirs, now).await, Some(B.to_string()));
    }
}

#[tokio::test]
async fn an_account_keeps_a_bounded_number_of_sessions() {
    for (_store, accounts) in stores().await {
        let mut tokens = Vec::new();
        for i in 0..MAX_SESSIONS_PER_ACCOUNT {
            let mut random = [0u8; 32];
            random[..8].copy_from_slice(&(i as u64).to_be_bytes());
            tokens.push(
                accounts
                    .open_session_at(A, None, random, T + i as u64)
                    .await
                    .unwrap(),
            );
        }
        // The oldest is used again, so the least recently used one makes room.
        assert_eq!(
            who(&accounts, &tokens[0], T + 10_000).await,
            Some(A.to_string())
        );
        let newest = accounts
            .open_session_at(A, None, [0xee; 32], T + 10_001)
            .await
            .unwrap();
        assert_eq!(who(&accounts, &newest, T + 10_002).await, Some(A.to_string()));
        assert_eq!(
            who(&accounts, &tokens[0], T + 10_002).await,
            Some(A.to_string())
        );
        assert_eq!(
            who(&accounts, &tokens[1], T + 10_002).await,
            None,
            "least recently used"
        );
        let held: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE account = $1")
            .bind(A)
            .fetch_one(accounts.store().pool())
            .await
            .unwrap();
        assert_eq!(held, MAX_SESSIONS_PER_ACCOUNT as i64);
    }
}

#[tokio::test]
async fn a_link_adds_the_passkey_and_opens_its_session_together_or_not_at_all() {
    for (store, accounts) in stores().await {
        let link = accounts.issue_link(LinkPurpose::Owner, A).await.unwrap();
        // The session cannot be stored: nothing else happens either.
        let (refuse, allow) = match store.store.backend() {
            Backend::Sqlite => (
                "CREATE TRIGGER no_sessions BEFORE INSERT ON sessions
                 BEGIN SELECT RAISE(ABORT, 'no sessions'); END;",
                "DROP TRIGGER no_sessions;",
            ),
            Backend::Postgres => (
                "CREATE FUNCTION no_sessions() RETURNS trigger LANGUAGE plpgsql AS
                   $$ BEGIN RAISE EXCEPTION 'no sessions'; END $$;
                 CREATE TRIGGER no_sessions BEFORE INSERT ON sessions
                   FOR EACH ROW EXECUTE FUNCTION no_sessions();",
                "DROP TRIGGER no_sessions ON sessions; DROP FUNCTION no_sessions();",
            ),
        };
        sqlx::raw_sql(refuse)
            .execute(accounts.store().pool())
            .await
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
                .await
                .is_err()
        );
        assert_eq!(
            accounts.check_link(LinkPurpose::Owner, &link).await.unwrap(),
            A,
            "still live"
        );
        assert!(accounts.passkeys_of(A).await.unwrap().is_empty());
        sqlx::raw_sql(allow)
            .execute(accounts.store().pool())
            .await
            .unwrap();
        let token = accounts
            .register_with_link(
                A,
                &credential(&accounts, A, 7),
                "phone",
                LinkPurpose::Owner,
                &link,
            )
            .await
            .unwrap();
        assert_eq!(
            who(&accounts, &token, now_secs()).await,
            Some(A.to_string())
        );
    }
}

#[tokio::test]
async fn a_counter_that_goes_back_ends_the_sessions_that_passkey_opened() {
    // The race the verifier cannot see: the store holds a newer counter than the one the
    // page read. The sign-in is refused and the passkey's sessions end; others stay.
    for (_store, accounts) in stores().await {
        let mut counted = credential(&accounts, A, 1);
        counted.backup_eligible = false;
        accounts
            .store_passkey(A, &counted, "key", None, now_secs())
            .await
            .unwrap();
        let other = passkey(&accounts, A, 2).await;
        let by_key = accounts.open_session(A, Some(&[1; 16])).await.unwrap();
        let by_other = accounts.open_session(A, Some(&other)).await.unwrap();
        let at = |count: u32| Asserted {
            id: vec![1; 16],
            public_key: vec![0x30, 1],
            alg: Alg::Es256,
            backup_eligible: false,
            sign_count: count,
            user_handle: A.as_bytes().to_vec(),
            challenge: accounts.challenge(Ceremony::SignIn, None),
        };
        accounts.sign_in(&at(5)).await.unwrap();
        let copy = at(5);
        assert_eq!(
            accounts.sign_in(&copy).await.unwrap_err().code,
            "signature_counter"
        );
        assert_eq!(who(&accounts, &by_key, now_secs()).await, None);
        assert_eq!(
            who(&accounts, &by_other, now_secs()).await,
            Some(A.to_string())
        );
        // Its challenge was spent all the same: replaying it ends nothing more.
        assert_eq!(
            accounts.sign_in(&copy).await.unwrap_err().code,
            "invalid_ceremony"
        );
    }
}
