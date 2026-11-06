//! Tenants and accounts made on first connect, on every store this run has (SQLite always,
//! Postgres when the test server is set, as in CI).

use std::net::IpAddr;

use super::*;
use crate::{
    accounts::identities::Proven,
    config::{TenancyConfig, TenancyMode, tenancy::UnsecuredLimits},
    store::testing::{self, TestStore},
};

fn hosted(limits: UnsecuredLimits, starts_per_hour: u32, max_unsecured: u32) -> TenancyConfig {
    TenancyConfig {
        mode: TenancyMode::Multi,
        starts_per_hour,
        max_unsecured,
        unsecured: limits,
        ..TenancyConfig::default()
    }
}

async fn stores(tenancy: TenancyConfig, operators: &[&str]) -> Vec<(TestStore, Accounts)> {
    let mut out = Vec::new();
    for store in testing::stores().await {
        let accounts = Accounts::open(store.store.clone())
            .await
            .unwrap()
            .with_tenancy(
                tenancy.clone(),
                operators.iter().map(|o| o.to_string()).collect(),
            );
        out.push((store, accounts));
    }
    out
}

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

#[test]
fn tenant_ids_are_t_and_a_uuid() {
    let id = new_tenant_id([0xab; 16]);
    assert!(valid_tenant_id(&id), "{id}");
    assert_eq!(id.len(), 38);
    assert_eq!(&id[..2], "t-");
    assert_eq!(&id[16..17], "4", "a version 4 UUID: {id}");
    for bad in [
        "t-",
        "acct_x",
        "t-ABCDEF00-0000-4000-8000-000000000000",
        "x-00000000-0000-4000-8000-000000000000",
    ] {
        assert!(!valid_tenant_id(bad), "{bad}");
    }
    assert_eq!(tag(&id), "tabababababab");
}

/// Where a hub from before tenants (migration 4) left its store, with an owner, a passkey
/// and a session: SQLite always, Postgres when the test server is set.
async fn stores_from_before_tenants() -> Vec<(String, tempfile::TempDir)> {
    let mut out = Vec::new();
    let dir = tempfile::tempdir().unwrap();
    out.push((
        crate::store::sqlite_url(&dir.path().join("accounts.sqlite3")),
        dir,
    ));
    if let Some(url) = testing::empty_postgres().await {
        out.push((url, tempfile::tempdir().unwrap()));
    }
    for (url, _) in &out {
        testing::migrated_to(url, 4).await;
        let mut old = <sqlx::AnyConnection as sqlx::Connection>::connect(url)
            .await
            .unwrap();
        for sql in [
            "INSERT INTO accounts (id, name, owner, created_at) VALUES ('395290028818825218', 'Jesse', 1, 1)",
            "INSERT INTO passkeys (id, account, label, public_key, alg, sign_count, backup_eligible, created_at)
             VALUES ($1, '395290028818825218', 'laptop', $1, -7, 0, 1, 1)",
            "INSERT INTO sessions (hash, account, passkey, created_at, last_seen, expires_at)
             VALUES ('s1', '395290028818825218', $1, 1, 1, 9999999999)",
        ] {
            let query = sqlx::query(sql);
            let query = if sql.contains("$1") { query.bind(vec![7_u8; 16]) } else { query };
            query.execute(&mut old).await.unwrap();
        }
        <sqlx::AnyConnection as sqlx::Connection>::close(old)
            .await
            .unwrap();
    }
    out
}

#[tokio::test]
async fn accounts_from_before_tenants_join_the_home_tenant_once() {
    for (url, _dir) in stores_from_before_tenants().await {
        // Migrations 5 to 7 run: the account, its passkey and its session join the home
        // tenant the migration makes, and the tables move and take their prefixed names.
        let accounts = Accounts::open(crate::store::Store::open(&url).await.unwrap())
            .await
            .unwrap();
        let home = accounts.home_tenant().await.unwrap();
        assert!(valid_tenant_id(&home), "{home}");
        let owner = accounts.owner().await.unwrap().unwrap();
        assert_eq!(owner.id, "395290028818825218");
        assert_eq!(owner.tenant, home);
        assert!(
            !owner.provisional,
            "an account made before is never provisional"
        );
        for table in ["auth_passkeys", "auth_sessions"] {
            let tenants: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT tenant_id FROM {table}"
            )))
            .fetch_all(accounts.store().admin())
            .await
            .unwrap();
            assert_eq!(tenants, [home.clone()], "{table}");
        }
        assert_eq!(accounts.passkeys_of(&owner.id).await.unwrap().len(), 1);
        let joined = accounts.events(&home, 10).await.unwrap();
        assert_eq!(
            joined
                .iter()
                .filter(|e| e.event == "tenant.accounts_joined")
                .count(),
            1
        );
        // Opened again: one home tenant, kept; nothing moves and nothing is recorded.
        let again = Accounts::open(crate::store::Store::open(&url).await.unwrap())
            .await
            .unwrap();
        assert_eq!(
            again.home_tenant().await.unwrap(),
            home,
            "one home tenant, kept"
        );
        assert_eq!(again.events(&home, 10).await.unwrap().len(), joined.len());
        assert!(again.chain_intact(&home).await.unwrap());
        // New accounts on a one-owner hub go to the home tenant too.
        let made = again
            .create("acct_cccccccccccccccccccccccccc", "Sam", false)
            .await
            .unwrap();
        assert_eq!(made.tenant, home);
    }
}

#[tokio::test]
async fn the_first_connect_makes_an_account_its_own_tenant_and_a_session() {
    for (_test, accounts) in stores(hosted(UnsecuredLimits::default(), 8, 100), &[]).await {
        let (alice, alice_session) = accounts
            .start_unsecured(ip("198.51.100.1"), "Claude")
            .await
            .unwrap();
        let (bob, _) = accounts
            .start_unsecured(ip("198.51.100.2"), "Codex")
            .await
            .unwrap();
        assert!(alice.provisional && bob.provisional);
        assert!(valid_tenant_id(&alice.tenant) && valid_tenant_id(&bob.tenant));
        assert_ne!(
            alice.tenant, bob.tenant,
            "each person a tenant of their own"
        );
        assert_ne!(alice.tenant, accounts.home_tenant().await.unwrap());
        let session = accounts.session(&alice_session).await.unwrap().unwrap();
        assert_eq!(session.account.id, alice.id);
        assert_eq!(session.account.tenant, alice.tenant);
        // Each tenant's chain holds its own steps only, and is whole.
        let steps: Vec<String> = accounts
            .events(&alice.tenant, 10)
            .await
            .unwrap()
            .into_iter()
            .rev()
            .map(|e| e.event)
            .collect();
        assert_eq!(
            steps,
            [
                "tenant.created",
                "tenant.admitted",
                "account.created",
                "session.opened"
            ]
        );
        assert!(accounts.chain_intact(&alice.tenant).await.unwrap());
        assert!(
            accounts
                .events(&bob.tenant, 10)
                .await
                .unwrap()
                .iter()
                .all(|e| e.actor == "hub" || e.actor == bob.id)
        );
        // An edited row breaks the chain.
        sqlx::query("UPDATE tenancy_events SET detail = 'edited' WHERE tenant_id = $1 AND seq = 2")
            .bind(&alice.tenant)
            .execute(accounts.store().admin())
            .await
            .unwrap();
        assert!(!accounts.chain_intact(&alice.tenant).await.unwrap());
        assert!(accounts.chain_intact(&bob.tenant).await.unwrap());
        // The address is never stored, only its keyed hash.
        let kept: Vec<String> = sqlx::query_scalar("SELECT source FROM auth_account_starts")
            .fetch_all(accounts.store().admin())
            .await
            .unwrap();
        assert!(
            kept.iter().all(|k| !k.contains("198.51") && k.len() == 32),
            "{kept:?}"
        );
    }
}

#[tokio::test]
async fn a_one_owner_hub_makes_no_account_on_first_connect() {
    for (_test, accounts) in stores(TenancyConfig::default(), &[]).await {
        let refused = accounts
            .start_unsecured(ip("198.51.100.1"), "Claude")
            .await
            .unwrap_err();
        assert_eq!(refused.code, "not_hosted");
    }
}

#[tokio::test]
async fn new_accounts_are_rate_limited_per_address_and_capped_on_the_hub() {
    for (_test, accounts) in stores(hosted(UnsecuredLimits::default(), 2, 3), &[]).await {
        for _ in 0..2 {
            accounts
                .start_unsecured(ip("203.0.113.5"), "x")
                .await
                .unwrap();
        }
        let limited = accounts
            .start_unsecured(ip("203.0.113.5"), "x")
            .await
            .unwrap_err();
        assert_eq!(limited.code, "rate_limited");
        // The same address written as IPv4-mapped IPv6 is the same address.
        let mapped = accounts
            .start_unsecured(ip("::ffff:203.0.113.5"), "x")
            .await
            .unwrap_err();
        assert_eq!(mapped.code, "rate_limited");
        // Another address gets in until the hub's cap of unsecured accounts.
        accounts
            .start_unsecured(ip("203.0.113.6"), "x")
            .await
            .unwrap();
        let full = accounts
            .start_unsecured(ip("203.0.113.7"), "x")
            .await
            .unwrap_err();
        assert_eq!(full.code, "capacity");
        // An hour on, the address may start again (the cap still holds).
        let later = now_secs() + 3601;
        let again = accounts
            .start_unsecured_at(ip("203.0.113.5"), "x", [1; 16], [2; 16], [3; 32], later)
            .await
            .unwrap_err();
        assert_eq!(again.code, "capacity");
    }
}

#[tokio::test]
async fn an_unsecured_account_ends_when_unused_and_its_sessions_and_tokens_with_it() {
    let limits = UnsecuredLimits {
        idle_days: 1,
        ..UnsecuredLimits::default()
    };
    for (_test, accounts) in stores(hosted(limits, 8, 100), &[]).await {
        let start = now_secs();
        let (account, session) = accounts
            .start_unsecured_at(
                ip("198.51.100.1"),
                "Claude",
                [4; 16],
                [5; 16],
                [6; 32],
                start,
            )
            .await
            .unwrap();
        assert_eq!(accounts.lapses_at(&account), Some(start + 86_400));
        // Used within the day: still live, and the use moves its clock.
        let used = start + 86_000;
        assert!(accounts.session_at(&session, used).await.unwrap().is_some());
        let moved = accounts.get(&account.id).await.unwrap().unwrap();
        assert_eq!(moved.last_active, Some(used));
        // A day after that, unused: gone, sessions and tokens alike.
        let lapsed = used + 86_400;
        assert!(
            accounts
                .session_at(&session, lapsed)
                .await
                .unwrap()
                .is_none()
        );
        assert!(accounts.lapsed(&moved, lapsed));
        let subject = accounts.subject(&moved, "hub");
        assert_eq!(subject.provisional_until, Some(used + 86_400));
        assert_eq!(subject.tenant.as_deref(), Some(account.tenant.as_str()));
    }
}

#[tokio::test]
async fn an_unsecured_account_connects_only_so_many_ai_apps() {
    let limits = UnsecuredLimits {
        clients: 1,
        ..UnsecuredLimits::default()
    };
    for (_test, accounts) in stores(hosted(limits, 8, 100), &[]).await {
        let (account, _) = accounts
            .start_unsecured(ip("198.51.100.1"), "Claude")
            .await
            .unwrap();
        let client = accounts
            .register_client(
                "Claude",
                &["https://claude.ai/api/mcp/auth_callback".into()],
            )
            .await
            .unwrap();
        let verifier = "v".repeat(43);
        let challenge = crate::accounts::oauth::s256(&verifier);
        let code = accounts
            .issue_code(
                &client.id,
                &account.id,
                &client.redirect_uris[0],
                &challenge,
                "r",
                "mcp",
            )
            .await
            .unwrap();
        accounts
            .exchange_code(&code, &client.id, &client.redirect_uris[0], &verifier, None)
            .await
            .unwrap();
        let second = accounts
            .issue_code(
                &client.id,
                &account.id,
                &client.redirect_uris[0],
                &challenge,
                "r",
                "mcp",
            )
            .await
            .unwrap_err();
        assert_eq!(second.code, "client_limit");
    }
}

fn proven(provider: &str, subject: &str, email: Option<&str>) -> Proven {
    Proven {
        provider: provider.into(),
        subject: subject.into(),
        verified_email: email.map(str::to_string),
        name: Some("Jesse".into()),
    }
}

#[tokio::test]
async fn an_operators_sign_in_is_the_owner_in_the_home_tenant_with_no_claim() {
    let operators = ["email:jesse@example.com", "github:42"];
    for (_test, accounts) in stores(hosted(UnsecuredLimits::default(), 8, 100), &operators).await {
        let home = accounts.home_tenant().await.unwrap();
        assert!(accounts.owner().await.unwrap().is_none());
        let owner = accounts
            .sign_in_identity(&proven("google", "g-1", Some("Jesse@Example.com")))
            .await
            .unwrap();
        assert!(owner.owner && !owner.provisional);
        assert_eq!(owner.tenant, home, "the operator is tenant #1");
        // The operator's other listed sign-in reaches the same account.
        let again = accounts
            .sign_in_identity(&proven("github", "42", None))
            .await
            .unwrap();
        assert_eq!(again.id, owner.id);
        // An unverified email names nobody.
        let stranger = accounts
            .sign_in_identity(&proven("google", "g-2", None))
            .await
            .unwrap();
        assert!(!stranger.owner);
        assert_ne!(
            stranger.tenant, home,
            "a stranger gets a tenant of their own"
        );
        // An operator's sign-in cannot be linked to anyone else's account.
        let taken = accounts
            .link_identity(&stranger.id, &proven("github", "42", None))
            .await
            .unwrap_err();
        assert_eq!(taken.code, "identity_taken");
        let refused = accounts
            .link_identity(
                &stranger.id,
                &proven("email", "jesse@example.com", Some("jesse@example.com")),
            )
            .await
            .unwrap_err();
        assert_eq!(refused.code, "operator_identity");
    }
}

#[tokio::test]
async fn linking_a_sign_in_secures_an_account_made_on_first_connect() {
    for (_test, accounts) in stores(hosted(UnsecuredLimits::default(), 8, 100), &[]).await {
        let (account, _) = accounts
            .start_unsecured(ip("198.51.100.1"), "Claude")
            .await
            .unwrap();
        let linked = accounts
            .link_identity(
                &account.id,
                &proven("google", "g-9", Some("sam@example.com")),
            )
            .await
            .unwrap();
        assert!(!linked.provisional);
        let stored = accounts.get(&account.id).await.unwrap().unwrap();
        assert!(!stored.provisional && accounts.lapses_at(&stored).is_none());
        // The same sign-in later is that account; so is another provider with the same
        // verified email.
        let back = accounts
            .sign_in_identity(&proven("google", "g-9", None))
            .await
            .unwrap();
        assert_eq!(back.id, account.id);
        let github = accounts
            .sign_in_identity(&proven("github", "77", Some("SAM@example.com")))
            .await
            .unwrap();
        assert_eq!(github.id, account.id);
        let kinds: Vec<String> = accounts
            .identities_of(&account.id)
            .await
            .unwrap()
            .into_iter()
            .map(|l| l.provider)
            .collect();
        assert_eq!(kinds, ["google", "github"]);
        let events: Vec<String> = accounts
            .events(&account.tenant, 20)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.event)
            .collect();
        assert_eq!(events.iter().filter(|e| *e == "account.secured").count(), 1);
        assert!(accounts.chain_intact(&account.tenant).await.unwrap());
    }
}

#[tokio::test]
async fn a_one_owner_hub_makes_no_account_for_a_strangers_sign_in() {
    for (_test, accounts) in stores(TenancyConfig::default(), &["github:42"]).await {
        let refused = accounts
            .sign_in_identity(&proven("github", "43", None))
            .await
            .unwrap_err();
        assert_eq!(refused.code, "no_account");
        let owner = accounts
            .sign_in_identity(&proven("github", "42", None))
            .await
            .unwrap();
        assert!(owner.owner);
    }
}

/// The tables row-level security holds to one tenant (migration 0007).
const TENANT_TABLES: [&str; 12] = [
    "auth_accounts",
    "auth_passkeys",
    "auth_sessions",
    "auth_links",
    "auth_identities",
    "oauth_codes",
    "oauth_tokens",
    "tenancy_events",
    "tenancy_access",
    "tenancy_workflows",
    "tenancy_clients",
    "tenancy_offers",
];

#[tokio::test]
async fn on_postgres_the_hubs_role_reads_and_writes_one_tenants_rows_only() {
    // Database-enforced isolation: whatever a query of the hub's forgets, Postgres shows
    // the hub's role (`devagotchi_hub`, no superuser, no BYPASSRLS) the rows of the tenant
    // its transaction is scoped to and no other's, and none at all unscoped.
    let Some(test) = testing::postgres().await else {
        return;
    };
    let accounts = Accounts::open(test.store.clone())
        .await
        .unwrap()
        .with_tenancy(hosted(UnsecuredLimits::default(), 8, 100), Vec::new());
    assert!(accounts.store().enforced());
    // Two people, each with every kind of row: a session and events (first connect), a
    // linked sign-in, a passkey, a one-time link, an OAuth code and tokens.
    let client = accounts
        .register_client("An AI", &["https://ai.example.net/cb".to_string()])
        .await
        .unwrap();
    let mut people = Vec::new();
    for (n, address) in [(1_u8, "198.51.100.1"), (2, "198.51.100.2")] {
        let (account, _session) = accounts
            .start_unsecured(ip(address), "Claude")
            .await
            .unwrap();
        accounts
            .link_identity(&account.id, &proven("github", &format!("{n}"), None))
            .await
            .unwrap();
        accounts
            .store_passkey(
                &account.id,
                &crate::accounts::NewCredential {
                    id: vec![n; 16],
                    public_key: vec![0x30, n],
                    alg: crate::accounts::Alg::Es256,
                    sign_count: 0,
                    backup_eligible: true,
                    challenge: accounts.challenge(Ceremony::Register, Some(&account.id)),
                },
                "laptop",
                None,
                now_secs(),
            )
            .await
            .unwrap();
        // One-time links are the owner's alone; a row set up under the API stands in.
        sqlx::query(
            "INSERT INTO auth_links (hash, purpose, account, expires_at, replaces, tenant_id)
             VALUES ($1, 'owner', $2, 9999999999, 0, $3)",
        )
        .bind(format!("link-{n}"))
        .bind(&account.id)
        .bind(&account.tenant)
        .execute(accounts.store().admin())
        .await
        .unwrap();
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = crate::accounts::oauth::s256(verifier);
        let resource = "https://hub.example.com/mcp";
        let code = accounts
            .issue_code(
                &client.id,
                &account.id,
                "https://ai.example.net/cb",
                &challenge,
                resource,
                "mcp",
            )
            .await
            .unwrap();
        accounts
            .exchange_code(
                &code,
                &client.id,
                "https://ai.example.net/cb",
                verifier,
                Some(resource),
            )
            .await
            .unwrap();
        accounts
            .issue_code(
                &client.id,
                &account.id,
                "https://ai.example.net/cb",
                &challenge,
                resource,
                "mcp",
            )
            .await
            .unwrap();
        // An AI client of its that made a call, and a workflow its AI started.
        accounts
            .note_client_call(&account.tenant, "claude", now_secs())
            .await
            .unwrap();
        // Its yes to the site's offers, and their one-tap link.
        accounts
            .accept_offers(
                &account.tenant,
                &account.id,
                "yes",
                &["An offering".to_string()],
                now_secs(),
            )
            .await
            .unwrap();
        accounts
            .offer_link(&account.tenant, [n; 32], 600, now_secs())
            .await
            .unwrap();
        let site = crate::site::builtin();
        let (_workflow, _link, _) = accounts
            .start_workflow(
                &account.tenant,
                &account.id,
                site.workflows.get("connect-linux").unwrap(),
                None,
                3600,
                [n; 12],
                [n; 32],
                now_secs(),
            )
            .await
            .unwrap();
        people.push(account);
    }
    let (a, b) = (&people[0], &people[1]);
    let count = |table: &str, filter: &str| {
        sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table} {filter}"))
    };
    // As the login role (no row-level security for it): both tenants' rows are there.
    for table in TENANT_TABLES {
        for who in [a, b] {
            let held: i64 = sqlx::query_scalar(count(table, "WHERE tenant_id = $1"))
                .bind(&who.tenant)
                .fetch_one(accounts.store().admin())
                .await
                .unwrap();
            assert!(held > 0, "{table} holds rows of {}", who.tenant);
        }
    }
    let mut hub = accounts.store().pool().begin().await.unwrap();
    let role: (String, bool, bool) = sqlx::query_as(
        "SELECT current_user::text, rolsuper, rolbypassrls FROM pg_roles WHERE rolname = current_user",
    )
    .fetch_one(&mut *hub)
    .await
    .unwrap();
    assert_eq!(role, ("devagotchi_hub".to_string(), false, false));
    // Unscoped: nothing, in every table.
    for table in TENANT_TABLES {
        let seen: i64 = sqlx::query_scalar(count(table, ""))
            .fetch_one(&mut *hub)
            .await
            .unwrap();
        assert_eq!(seen, 0, "{table} unscoped");
    }
    // Scoped to A: A's rows, and not one of B's, even asked for by B's tenant and account.
    accounts.store().scope(&mut hub, &a.tenant).await.unwrap();
    for table in TENANT_TABLES {
        let mine: i64 = sqlx::query_scalar(count(table, ""))
            .fetch_one(&mut *hub)
            .await
            .unwrap();
        assert!(mine > 0, "{table}: A sees its own rows");
        let theirs: i64 = sqlx::query_scalar(count(table, "WHERE tenant_id = $1"))
            .bind(&b.tenant)
            .fetch_one(&mut *hub)
            .await
            .unwrap();
        assert_eq!(theirs, 0, "{table}: B's rows, scoped to A");
    }
    let by_id: i64 = sqlx::query_scalar(count("auth_accounts", "WHERE id = $1"))
        .bind(&b.id)
        .fetch_one(&mut *hub)
        .await
        .unwrap();
    assert_eq!(by_id, 0);
    // Nor can it change them: updates and deletes of B's rows touch nothing, and a row
    // written into B's tenant is refused.
    for table in TENANT_TABLES {
        let changed = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {table} SET tenant_id = tenant_id WHERE tenant_id = $1"
        )))
        .bind(&b.tenant)
        .execute(&mut *hub)
        .await
        .unwrap()
        .rows_affected();
        assert_eq!(changed, 0, "{table}: B's rows updated from A");
    }
    let removed = sqlx::query("DELETE FROM auth_sessions WHERE account = $1")
        .bind(&b.id)
        .execute(&mut *hub)
        .await
        .unwrap()
        .rows_affected();
    assert_eq!(removed, 0);
    let mut savepoint = sqlx::Connection::begin(&mut *hub).await.unwrap();
    let written = sqlx::query(
        "INSERT INTO auth_links (hash, purpose, account, expires_at, replaces, tenant_id)
         VALUES ('planted', 'owner', $1, 9999999999, 0, $2)",
    )
    .bind(&b.id)
    .bind(&b.tenant)
    .execute(&mut *savepoint)
    .await
    .unwrap_err();
    assert_eq!(
        written
            .as_database_error()
            .and_then(|e| e.code())
            .as_deref(),
        Some("42501"),
        "row-level security refuses it: {written}"
    );
    savepoint.rollback().await.unwrap();
    // Moving A's own row into B's tenant is refused the same way.
    let mut savepoint = sqlx::Connection::begin(&mut *hub).await.unwrap();
    let moved = sqlx::query("UPDATE auth_links SET tenant_id = $1 WHERE account = $2")
        .bind(&b.tenant)
        .bind(&a.id)
        .execute(&mut *savepoint)
        .await
        .unwrap_err();
    assert_eq!(
        moved.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("42501"),
        "{moved}"
    );
    savepoint.rollback().await.unwrap();
    // And it makes and changes no tables: the store's schema is the migrations' alone.
    for ddl in [
        "CREATE TABLE devagotchi.planted (id TEXT)",
        "ALTER TABLE auth_accounts DISABLE ROW LEVEL SECURITY",
        "DROP POLICY tenant_only ON auth_sessions",
    ] {
        let mut savepoint = sqlx::Connection::begin(&mut *hub).await.unwrap();
        let refused = sqlx::query(sqlx::AssertSqlSafe(ddl))
            .execute(&mut *savepoint)
            .await
            .unwrap_err();
        assert_eq!(
            refused
                .as_database_error()
                .and_then(|e| e.code())
                .as_deref(),
            Some("42501"),
            "{ddl}: {refused}"
        );
        savepoint.rollback().await.unwrap();
    }
    hub.rollback().await.unwrap();
    // The one cross-tenant lookup answers a tenant id and nothing more.
    let mut connection = accounts.store().pool().acquire().await.unwrap();
    assert_eq!(
        accounts
            .store()
            .tenant_of(&mut connection, crate::store::Lookup::Account(&b.id))
            .await
            .unwrap()
            .as_deref(),
        Some(b.tenant.as_str())
    );
    let unscoped: i64 = sqlx::query_scalar(count("auth_accounts", ""))
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    assert_eq!(unscoped, 0, "a lookup scopes nothing by itself");
    // And the API, scoped per transaction, still reaches each person's own rows.
    for who in [a, b] {
        assert_eq!(
            accounts.get(&who.id).await.unwrap().unwrap().tenant,
            who.tenant
        );
        assert_eq!(accounts.passkeys_of(&who.id).await.unwrap().len(), 1);
        assert_eq!(accounts.identities_of(&who.id).await.unwrap().len(), 1);
        assert!(accounts.chain_intact(&who.tenant).await.unwrap());
    }
}
