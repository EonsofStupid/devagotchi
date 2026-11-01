use super::*;
use crate::store::{
    Backend,
    testing::{self, TestStore},
};

/// The accounts in every store this run has: SQLite always, Postgres when the test server is
/// set (CI). Each test body runs on each.
async fn stores() -> Vec<(TestStore, Accounts)> {
    let mut out = Vec::new();
    for store in testing::stores().await {
        let accounts = Accounts::open(store.store.clone()).await.unwrap();
        out.push((store, accounts));
    }
    out
}

/// The same store opened again, as another hub instance or after a restart would.
async fn reopen(store: &TestStore) -> Accounts {
    Accounts::open(Store::open(&store.url).await.unwrap())
        .await
        .unwrap()
}

/// Run SQL under the API, for set-ups the API refuses to make.
async fn sql(accounts: &Accounts, query: &'static str, binds: &[Option<i64>]) {
    let mut query = sqlx::query(query);
    for bind in binds {
        query = query.bind(*bind);
    }
    query.execute(accounts.store().pool()).await.unwrap();
}

const A: &str = "acct_aaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "acct_bbbbbbbbbbbbbbbbbbbbbbbbbb";

/// A credential as `verify_registration` returns it, registered against `challenge`.
fn verified(id: u8, challenge: Vec<u8>) -> NewCredential {
    NewCredential {
        id: vec![id; 16],
        public_key: vec![0x30, id],
        alg: Alg::Es256,
        sign_count: 0,
        backup_eligible: true,
        challenge,
    }
}

/// Credential `id`, verified against a registration challenge for `account` made now.
fn credential(accounts: &Accounts, account: &str, id: u8) -> NewCredential {
    verified(id, accounts.challenge(Ceremony::Register, Some(account)))
}

/// Store a passkey with no link: what a test needs to set up, never a way in. (The public
/// `add_passkey` always takes an authority.)
async fn seed(accounts: &Accounts, account: &str, id: u8, label: &str) -> BridgeResult<()> {
    accounts
        .store_passkey(
            account,
            &credential(accounts, account, id),
            label,
            None,
            now_secs(),
        )
        .await
}

/// A sign-in as `check_sign_in` returns it: passkey `id` (its key, as `verified` makes
/// it), counter `count`, user handle `handle`, for a sign-in challenge made now.
fn asserted(accounts: &Accounts, id: u8, count: u32, handle: &str) -> Asserted {
    Asserted {
        id: vec![id; 16],
        public_key: vec![0x30, id],
        alg: Alg::Es256,
        backup_eligible: true,
        sign_count: count,
        user_handle: handle.as_bytes().to_vec(),
        challenge: accounts.challenge(Ceremony::SignIn, None),
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

#[tokio::test]
async fn one_owner_and_unique_ids() {
    for (_store, accounts) in stores().await {
        assert!(accounts.owner().await.unwrap().is_none());
        let owner = accounts.create(A, "Jesse", true).await.unwrap();
        assert!(owner.owner);
        assert_eq!(accounts.owner().await.unwrap().unwrap().id, owner.id);
        assert_eq!(accounts.get(&owner.id).await.unwrap().unwrap().name, "Jesse");
        assert_eq!(
            accounts.create(B, "Two", true).await.unwrap_err().code,
            "owner_exists"
        );
        assert_eq!(
            accounts.create(&owner.id, "Again", false).await.unwrap_err().code,
            "account_exists"
        );
        assert_eq!(
            accounts.create("../x", "Bad", false).await.unwrap_err().code,
            "invalid_account"
        );
        assert!(
            accounts
                .get("acct_cccccccccccccccccccccccccc")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            !accounts
                .create("395290028818825218", "Legacy", false)
                .await
                .unwrap()
                .owner
        );
    }
}

#[tokio::test]
async fn owner_link_is_one_time_and_stored_only_as_a_hash() {
    for (store, accounts) in stores().await {
        let owner = accounts.create(A, "Jesse", true).await.unwrap();
        let token = accounts.issue_link(LinkPurpose::Owner, &owner.id).await.unwrap();
        assert!(
            token.starts_with("dgl_") && token.len() == 4 + 43,
            "{token}"
        );
        // Checking does not spend it, so a link preview cannot burn it.
        for _ in 0..2 {
            assert_eq!(
                accounts.check_link(LinkPurpose::Owner, &token).await.unwrap(),
                owner.id
            );
        }
        // The store holds the link's hash and never the link.
        let stored: Vec<String> = sqlx::query_scalar("SELECT hash FROM links")
            .fetch_all(accounts.store().pool())
            .await
            .unwrap();
        assert_eq!(stored, vec![hash(&token)]);
        if store.store.backend() == Backend::Sqlite {
            // Everything SQLite wrote, the write-ahead log included.
            sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                .execute(accounts.store().pool())
                .await
                .unwrap();
            let file = store.dir.path().join("accounts.sqlite3");
            let mut raw = std::fs::read(&file).unwrap();
            raw.extend(std::fs::read(store.dir.path().join("accounts.sqlite3-wal")).unwrap_or_default());
            let contains = |needle: &[u8]| raw.windows(needle.len()).any(|w| w == needle);
            assert!(contains(hash(&token).as_bytes()), "the hash is stored");
            assert!(!contains(token.as_bytes()), "the link itself is not");
        }
        accounts
            .add_passkey(
                &owner.id,
                &credential(&accounts, A, 1),
                "phone",
                Authority::Link(LinkPurpose::Owner, &token),
            )
            .await
            .unwrap();
        assert_eq!(
            accounts
                .check_link(LinkPurpose::Owner, &token)
                .await
                .unwrap_err()
                .code,
            "invalid_link"
        );
        assert_eq!(
            accounts
                .add_passkey(
                    &owner.id,
                    &credential(&accounts, A, 2),
                    "again",
                    Authority::Link(LinkPurpose::Owner, &token),
                )
                .await
                .unwrap_err()
                .code,
            "invalid_link"
        );
        assert!(
            accounts.passkey(&[2; 16]).await.unwrap().is_none(),
            "nothing added on refusal"
        );
    }
}

#[tokio::test]
async fn owner_links_expire_and_supersede() {
    for (_store, accounts) in stores().await {
        let owner = accounts.create(A, "Jesse", true).await.unwrap();
        let plain = accounts.create(B, "Friend", false).await.unwrap();
        assert_eq!(
            accounts
                .issue_link(LinkPurpose::Owner, &plain.id)
                .await
                .unwrap_err()
                .code,
            "not_owner"
        );
        let old = accounts
            .issue_link_at(LinkPurpose::Owner, &owner.id, [1; 32], 1_000)
            .await
            .unwrap();
        assert_eq!(
            accounts
                .check_link_at(LinkPurpose::Owner, &old, 1_000 + LINK_TTL_SECS - 1)
                .await
                .unwrap(),
            owner.id
        );
        assert_eq!(
            accounts
                .check_link_at(LinkPurpose::Owner, &old, 1_000 + LINK_TTL_SECS)
                .await
                .unwrap_err()
                .code,
            "invalid_link"
        );
        let new = accounts
            .issue_link_at(LinkPurpose::Owner, &owner.id, [2; 32], 1_010)
            .await
            .unwrap();
        assert_eq!(
            accounts
                .check_link_at(LinkPurpose::Owner, &old, 1_020)
                .await
                .unwrap_err()
                .code,
            "invalid_link"
        );
        assert_eq!(
            accounts
                .check_link_at(LinkPurpose::Owner, &new, 1_020)
                .await
                .unwrap(),
            owner.id
        );
        assert_eq!(
            accounts
                .check_link(LinkPurpose::Owner, "dgl_nonsense")
                .await
                .unwrap_err()
                .code,
            "invalid_link"
        );
    }
}

#[tokio::test]
async fn a_refused_passkey_leaves_the_link_unspent() {
    for (_store, accounts) in stores().await {
        let owner = accounts.create(A, "Jesse", true).await.unwrap();
        accounts.create(B, "Friend", false).await.unwrap();
        seed(&accounts, B, 1, "theirs").await.unwrap();
        let link = accounts.issue_link(LinkPurpose::Owner, &owner.id).await.unwrap();
        let with_link = async |account: &str, id: u8| {
            accounts
                .add_passkey(
                    account,
                    &credential(&accounts, account, id),
                    "phone",
                    Authority::Link(LinkPurpose::Owner, &link),
                )
                .await
                .map_err(|error| error.code)
        };
        // A passkey already registered elsewhere: refused, and the link still works.
        assert_eq!(with_link(A, 1).await, Err("passkey_exists".into()));
        // The owner's link spent on someone else's account: refused, and still the owner's.
        assert_eq!(with_link(B, 2).await, Err("invalid_link".into()));
        assert_eq!(
            accounts.check_link(LinkPurpose::Owner, &link).await.unwrap(),
            A
        );
        assert_eq!(with_link(A, 3).await, Ok(()));

        // A link that ran out between opening the page and finishing.
        let late = accounts
            .issue_link_at(LinkPurpose::Owner, &owner.id, [4; 32], 1_000)
            .await
            .unwrap();
        let challenge =
            accounts.challenge_at(Ceremony::Register, Some(A), [4; 16], 1_000 + LINK_TTL_SECS);
        assert_eq!(
            accounts
                .store_passkey(
                    A,
                    &verified(4, challenge),
                    "phone",
                    Some((LinkPurpose::Owner, &late)),
                    1_000 + LINK_TTL_SECS,
                )
                .await
                .unwrap_err()
                .code,
            "invalid_link"
        );
    }
}

#[tokio::test]
async fn challenges_are_the_hubs_own_bound_to_their_purpose_and_spent_once() {
    for (_store, accounts) in stores().await {
        accounts.create(A, "Jesse", true).await.unwrap();
        accounts.create(B, "Friend", false).await.unwrap();
        let add = async |account: &str, id: u8, challenge: &[u8], now: u64| {
            accounts
                .store_passkey(
                    account,
                    &verified(id, challenge.to_vec()),
                    "phone",
                    None,
                    now,
                )
                .await
                .map_err(|error| error.code)
        };
        let refused = Err("invalid_ceremony".to_string());

        let challenge = accounts.challenge_at(Ceremony::Register, Some(A), [1; 16], 100);
        assert!(challenge.len() >= 32, "at least as long as WebAuthn asks");
        assert_eq!(add(A, 1, &challenge, 101).await, Ok(()));
        assert_eq!(add(A, 2, &challenge, 102).await, refused, "used once");
        // For another account, for signing in, before it was made, or after five minutes.
        let challenge = accounts.challenge_at(Ceremony::Register, Some(A), [2; 16], 100);
        assert_eq!(add(B, 3, &challenge, 101).await, refused);
        let sign_in = accounts.challenge_at(Ceremony::SignIn, None, [3; 16], 100);
        assert_eq!(add(A, 4, &sign_in, 101).await, refused);
        assert_eq!(add(A, 5, &challenge, 100 + CHALLENGE_TTL_SECS).await, refused);
        assert_eq!(add(A, 11, &challenge, 99).await, refused, "issued in the future");
        // Made up, altered, or made by another hub: refused.
        let mut altered = challenge.clone();
        let last = altered.len() - 1;
        altered[last] ^= 1;
        assert_eq!(add(A, 6, &altered, 101).await, refused);
        assert_eq!(add(A, 7, &[0u8; 57], 101).await, refused);
        assert_eq!(add(A, 8, &[], 101).await, refused);
        let other_store = testing::sqlite().await;
        let other = Accounts::open(other_store.store.clone()).await.unwrap();
        let foreign = other.challenge_at(Ceremony::Register, Some(A), [2; 16], 100);
        assert_eq!(add(A, 9, &foreign, 101).await, refused);
        // Its last valid second, after all that.
        assert_eq!(
            add(A, 10, &challenge, 100 + CHALLENGE_TTL_SECS - 1).await,
            Ok(())
        );

        // Asking for challenges writes nothing: a flood of unanswered sign-ins costs no
        // storage and blocks nobody. Only the two ceremonies that succeeded left a record.
        for i in 0..1_000u16 {
            let mut nonce = [0u8; 16];
            nonce[..2].copy_from_slice(&i.to_be_bytes());
            accounts.challenge_at(Ceremony::SignIn, None, nonce, 100);
        }
        let spent: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM spent_challenges")
            .fetch_one(accounts.store().pool())
            .await
            .unwrap();
        assert_eq!(spent, 2);
    }
}

#[tokio::test]
async fn a_spent_challenge_stays_spent_across_a_clock_step() {
    for (_store, accounts) in stores().await {
        accounts.create(A, "Jesse", true).await.unwrap();
        seed(&accounts, A, 1, "synced").await.unwrap();
        let at = |nonce: u8, issued: u64| {
            let mut sign_in = asserted(&accounts, 1, 0, A);
            sign_in.challenge =
                accounts.challenge_at(Ceremony::SignIn, None, [nonce; 16], issued);
            sign_in
        };
        let c = at(1, 1_000);
        accounts.sign_in_at(&c, 1_010).await.unwrap();
        // The clock jumps ahead and another sign-in clears out old records, c's among them.
        accounts.sign_in_at(&at(2, 5_000), 5_000).await.unwrap();
        // Set back: c would be inside its five minutes again, but it was spent.
        assert_eq!(
            accounts.sign_in_at(&c, 1_020).await.unwrap_err().code,
            "invalid_ceremony"
        );
        // A challenge made after the step back still works.
        assert_eq!(
            accounts.sign_in_at(&at(3, 1_100), 1_101).await.unwrap().id,
            A
        );
    }
}

#[tokio::test]
async fn a_clock_set_back_does_not_lock_everyone_out() {
    for (_store, accounts) in stores().await {
        accounts.create(A, "Jesse", true).await.unwrap();
        seed(&accounts, A, 1, "synced").await.unwrap();
        let at = |nonce: u8, issued: u64| {
            let mut sign_in = asserted(&accounts, 1, 0, A);
            sign_in.challenge =
                accounts.challenge_at(Ceremony::SignIn, None, [nonce; 16], issued);
            sign_in
        };
        accounts.sign_in_at(&at(1, 5_000), 5_000).await.unwrap();
        let before = at(5, 5_350);
        // Clears the first record: everything expiring by 5_300 is behind the horizon.
        accounts.sign_in_at(&at(2, 5_400), 5_400).await.unwrap();
        // Set back about an hour. The ceremony that finds it fails; challenges made before
        // the step fail too, so none whose record was cleared can come back.
        assert_eq!(
            accounts.sign_in_at(&at(3, 1_800), 1_801).await.unwrap_err().code,
            "invalid_ceremony"
        );
        assert_eq!(
            accounts.sign_in_at(&before, 1_802).await.unwrap_err().code,
            "invalid_ceremony"
        );
        // A fresh one works at once, not an hour later.
        assert_eq!(
            accounts.sign_in_at(&at(4, 1_810), 1_811).await.unwrap().id,
            A
        );
    }
}

#[tokio::test]
async fn a_damaged_horizon_is_refused_not_ignored() {
    for (_store, accounts) in stores().await {
        accounts.create(A, "Jesse", true).await.unwrap();
        seed(&accounts, A, 1, "synced").await.unwrap();
        sqlx::query("INSERT INTO secrets (name, value) VALUES ('spent_horizon', $1)")
            .bind(vec![1u8])
            .execute(accounts.store().pool())
            .await
            .unwrap();
        assert_eq!(
            accounts
                .sign_in(&asserted(&accounts, 1, 0, A))
                .await
                .unwrap_err()
                .code,
            "account_store"
        );
    }
}

#[tokio::test]
async fn passkeys_belong_to_one_account() {
    for (_store, accounts) in stores().await {
        let jesse = accounts.create(A, "Jesse", true).await.unwrap();
        let friend = accounts.create(B, "Friend", false).await.unwrap();
        seed(&accounts, A, 1, "phone").await.unwrap();
        seed(&accounts, A, 2, "laptop").await.unwrap();
        assert_eq!(
            seed(&accounts, B, 1, "stolen").await.unwrap_err().code,
            "passkey_exists"
        );
        let nobody = "acct_cccccccccccccccccccccccccc";
        assert_eq!(
            seed(&accounts, nobody, 3, "x").await.unwrap_err().code,
            "no_account"
        );
        let found = accounts.passkey(&[1; 16]).await.unwrap().unwrap();
        assert_eq!(
            (found.account.as_str(), found.label.as_str()),
            (jesse.id.as_str(), "phone")
        );
        assert_eq!(found.alg, Alg::Es256);
        assert_eq!(accounts.passkeys_of(&jesse.id).await.unwrap().len(), 2);
        assert!(
            !accounts.remove_passkey(&friend.id, &[2; 16]).await.unwrap(),
            "not the friend's"
        );
        assert!(accounts.remove_passkey(&jesse.id, &[2; 16]).await.unwrap());
        assert_eq!(accounts.passkeys_of(&jesse.id).await.unwrap().len(), 1);
        // The last one stays: removing it would lock the account out.
        assert_eq!(
            accounts
                .remove_passkey(&jesse.id, &[1; 16])
                .await
                .unwrap_err()
                .code,
            "last_passkey"
        );
        assert!(accounts.passkey(&[1; 16]).await.unwrap().is_some());
    }
}

#[tokio::test]
async fn signing_in_moves_the_counter_forward_once_and_only_for_a_live_account() {
    for (_store, accounts) in stores().await {
        accounts.create(A, "Jesse", true).await.unwrap();
        accounts.create(B, "Friend", false).await.unwrap();
        let mut counted = credential(&accounts, A, 1);
        counted.backup_eligible = false;
        accounts
            .store_passkey(A, &counted, "key", None, now_secs())
            .await
            .unwrap();
        seed(&accounts, A, 2, "synced").await.unwrap();
        seed(&accounts, A, 3, "spare").await.unwrap();
        let go = async |id: u8, count: u32, handle: &str| {
            let mut sign_in = asserted(&accounts, id, count, handle);
            // Passkey 1 is the one that keeps a counter and is not synced.
            sign_in.backup_eligible = id != 1;
            accounts
                .sign_in(&sign_in)
                .await
                .map(|account| account.id)
                .map_err(|error| error.code)
        };

        assert_eq!(go(1, 7, A).await, Ok(A.to_string()));
        assert_eq!(
            accounts.passkey(&[1; 16]).await.unwrap().unwrap().sign_count,
            7
        );
        // Two sign-ins that both read 7: the second to store finds the counter moved.
        assert_eq!(go(1, 7, A).await, Err("signature_counter".into()));
        assert_eq!(go(1, 6, A).await, Err("signature_counter".into()));
        assert_eq!(go(1, 8, A).await, Ok(A.to_string()));
        // A passkey that keeps no counter signs in with 0 every time.
        assert_eq!(go(2, 0, A).await, Ok(A.to_string()));
        assert_eq!(go(2, 0, A).await, Ok(A.to_string()));
        // The user handle the authenticator returns must be the passkey's own account.
        assert_eq!(go(2, 0, B).await, Err("user_handle".into()));
        assert_eq!(go(2, 0, "").await, Err("user_handle".into()));
        // One challenge, one sign-in; a registration challenge is not a sign-in one.
        let once = asserted(&accounts, 2, 0, A);
        accounts.sign_in(&once).await.unwrap();
        assert_eq!(
            accounts.sign_in(&once).await.unwrap_err().code,
            "invalid_ceremony"
        );
        let mut registering = asserted(&accounts, 2, 0, A);
        registering.challenge = accounts.challenge(Ceremony::Register, Some(A));
        assert_eq!(
            accounts.sign_in(&registering).await.unwrap_err().code,
            "invalid_ceremony"
        );
        // A disabled account, a removed passkey, or one never registered: no sign-in.
        sql(&accounts, "UPDATE accounts SET disabled_at = $1 WHERE id = 'acct_aaaaaaaaaaaaaaaaaaaaaaaaaa'", &[Some(1)]).await;
        assert_eq!(go(2, 0, A).await, Err("account_disabled".into()));
        assert_eq!(
            seed(&accounts, A, 5, "new").await.unwrap_err().code,
            "account_disabled"
        );
        sql(&accounts, "UPDATE accounts SET disabled_at = $1 WHERE id = 'acct_aaaaaaaaaaaaaaaaaaaaaaaaaa'", &[None]).await;
        assert!(accounts.remove_passkey(A, &[2; 16]).await.unwrap());
        assert_eq!(go(2, 0, A).await, Err("invalid_passkey".into()));
        assert_eq!(go(9, 1, A).await, Err("invalid_passkey".into()));
    }
}

#[tokio::test]
async fn the_store_signs_in_only_the_passkey_that_was_verified() {
    // A page that verified Mallory's own assertion, with her own key, but named Jesse's
    // passkey to the store: the store checks the key it was verified with is that passkey's.
    for (_store, accounts) in stores().await {
        accounts.create(A, "Jesse", true).await.unwrap();
        accounts.create(B, "Mallory", false).await.unwrap();
        seed(&accounts, A, 1, "jesse").await.unwrap();
        seed(&accounts, B, 2, "mallory").await.unwrap();
        let mut swapped = asserted(&accounts, 1, 0, A);
        swapped.public_key = vec![0x30, 2];
        assert_eq!(
            accounts.sign_in(&swapped).await.unwrap_err().code,
            "invalid_passkey"
        );
        // Checked as another algorithm, or with another backup eligibility, than was stored.
        let mut other_alg = asserted(&accounts, 1, 0, A);
        other_alg.alg = Alg::EdDsa;
        assert_eq!(
            accounts.sign_in(&other_alg).await.unwrap_err().code,
            "invalid_passkey"
        );
        let mut not_synced = asserted(&accounts, 1, 0, A);
        not_synced.backup_eligible = false;
        assert_eq!(
            accounts.sign_in(&not_synced).await.unwrap_err().code,
            "invalid_passkey"
        );
        assert_eq!(
            accounts
                .sign_in(&asserted(&accounts, 1, 0, A))
                .await
                .unwrap()
                .id,
            A
        );
    }
}

#[tokio::test]
async fn the_store_and_its_challenges_survive_a_reopen() {
    for (store, accounts) in stores().await {
        accounts.create(A, "Jesse", true).await.unwrap();
        seed(&accounts, A, 1, "phone").await.unwrap();
        let pending = asserted(&accounts, 1, 1, A);
        drop(accounts);
        let accounts = reopen(&store).await;
        assert_eq!(accounts.owner().await.unwrap().unwrap().name, "Jesse");
        assert!(accounts.passkey(&[1; 16]).await.unwrap().is_some());
        // A sign-in begun before a restart finishes after it.
        assert_eq!(accounts.sign_in(&pending).await.unwrap().id, A);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_sqlite_store_is_the_hubs_alone_new_or_found() {
    use std::os::unix::fs::PermissionsExt;
    let names = [
        "accounts.sqlite3",
        "accounts.sqlite3-wal",
        "accounts.sqlite3-shm",
    ];
    let modes = |dir: &std::path::Path| -> Vec<(String, u32)> {
        names
            .iter()
            .filter_map(|name| {
                let meta = std::fs::metadata(dir.join(name)).ok()?;
                Some((name.to_string(), meta.permissions().mode() & 0o777))
            })
            .collect()
    };
    // Made here.
    let store = testing::sqlite().await;
    let accounts = Accounts::open(store.store.clone()).await.unwrap();
    accounts.create(A, "Jesse", true).await.unwrap();
    let made = modes(store.dir.path());
    assert_eq!(made.len(), 3, "{made:?}");
    assert!(made.iter().all(|(_, mode)| *mode == 0o600), "{made:?}");
    // Found readable by others (a restored backup, a store made by hand, a write-ahead
    // log left behind): made private.
    for name in names {
        let path = store.dir.path().join(name);
        if !path.exists() {
            std::fs::write(&path, b"").unwrap();
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let _accounts = reopen(&store).await;
    let found = modes(store.dir.path());
    assert_eq!(found.len(), 3, "{found:?}");
    assert!(found.iter().all(|(_, mode)| *mode == 0o600), "{found:?}");
}

#[tokio::test]
async fn a_challenge_key_that_is_not_32_bytes_stops_the_store() {
    for (store, accounts) in stores().await {
        sqlx::query("UPDATE secrets SET value = $1 WHERE name = 'challenge_key'")
            .bind(Vec::<u8>::new())
            .execute(accounts.store().pool())
            .await
            .unwrap();
        drop(accounts);
        let opened = Accounts::open(Store::open(&store.url).await.unwrap()).await;
        assert_eq!(
            opened.err().map(|e| e.code),
            Some("account_store".to_string())
        );
    }
}

#[test]
fn an_authority_never_prints_its_link() {
    let shown = format!("{:?}", Authority::Link(LinkPurpose::Owner, "dgl_secret"));
    assert!(
        !shown.contains("dgl_secret") && shown.contains("Owner"),
        "{shown}"
    );
}

#[tokio::test]
async fn a_second_instance_uses_the_key_the_store_holds_now() {
    // Two hub instances on one store: the first finds the clock set back and replaces the
    // key, clearing every spent record. The second, holding the old key, must not take a
    // challenge that was spent before.
    for (store, one) in stores().await {
        let two = reopen(&store).await;
        one.create(A, "Jesse", true).await.unwrap();
        seed(&one, A, 1, "synced").await.unwrap();
        let at = |store: &Accounts, nonce: u8, issued: u64| {
            let mut sign_in = asserted(store, 1, 0, A);
            sign_in.challenge = store.challenge_at(Ceremony::SignIn, None, [nonce; 16], issued);
            sign_in
        };
        let spent = at(&two, 1, 5_000);
        two.sign_in_at(&spent, 5_001).await.unwrap();
        one.sign_in_at(&at(&one, 2, 5_400), 5_400).await.unwrap();
        assert!(
            one.sign_in_at(&at(&one, 3, 1_800), 1_801).await.is_err(),
            "the key is replaced"
        );
        // Back in the second instance, its clock right: the old challenge is still spent.
        assert_eq!(
            two.sign_in_at(&spent, 5_002).await.unwrap_err().code,
            "invalid_ceremony"
        );
        // And it takes up the new key for what it makes from here.
        assert_eq!(
            two.sign_in_at(&at(&two, 4, 1_810), 1_811).await.unwrap().id,
            A
        );
    }
}

#[tokio::test]
async fn the_owner_is_made_once_and_keeps_a_configured_id() {
    // Texas: the subject every board, machine and audit row is keyed by becomes the
    // owner's account id, so nothing is rewritten.
    for (_store, accounts) in stores().await {
        let owner = accounts
            .ensure_owner(Some("395290028818825218"), "Jesse", [1; 16])
            .await
            .unwrap();
        assert_eq!(
            (owner.id.as_str(), owner.owner),
            ("395290028818825218", true)
        );
        // Run again, with or without the config's id: the same owner.
        for configured in [Some("395290028818825218"), None] {
            assert_eq!(
                accounts
                    .ensure_owner(configured, "Other", [2; 16])
                    .await
                    .unwrap()
                    .id,
                owner.id
            );
        }
        assert_eq!(
            accounts
                .ensure_owner(Some(A), "Someone", [2; 16])
                .await
                .unwrap_err()
                .code,
            "owner_mismatch"
        );
    }
    // A fresh hub with no owner in its config: a new account id.
    for (_store, fresh) in stores().await {
        let made = fresh.ensure_owner(None, "Owner", [3; 16]).await.unwrap();
        assert_eq!(made.id, new_account_id([3; 16]));
        assert!(made.owner);
    }
}

#[tokio::test]
async fn two_instances_racing_for_one_link_register_once() {
    // Two hub instances finish a registration with the same owner link at the same moment:
    // one passkey is stored, the other ceremony is refused, and the link is spent.
    for (store, one) in stores().await {
        let two = reopen(&store).await;
        let owner = one.create(A, "Jesse", true).await.unwrap();
        let link = one.issue_link(LinkPurpose::Owner, &owner.id).await.unwrap();
        let (one_key, two_key) = (credential(&one, A, 1), credential(&two, A, 2));
        let (first, second) = tokio::join!(
            one.register_with_link(A, &one_key, "one", LinkPurpose::Owner, &link),
            two.register_with_link(A, &two_key, "two", LinkPurpose::Owner, &link),
        );
        assert_eq!(
            usize::from(first.is_ok()) + usize::from(second.is_ok()),
            1,
            "{:?} / {:?}",
            first.map(|_| ()),
            second.map(|_| ())
        );
        assert_eq!(one.passkeys_of(A).await.unwrap().len(), 1);
        assert!(one.check_link(LinkPurpose::Owner, &link).await.is_err());
    }
}
