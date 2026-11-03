//! Early access and plans in the store, on every store this run has (SQLite always,
//! Postgres when the test server is set, as in CI).

use std::{net::IpAddr, sync::Mutex};

use super::*;
use crate::{
    accounts::identities::Proven,
    config::{TenancyConfig, TenancyMode, tenancy::UnsecuredLimits},
    site::{SiteCell, tests::neutral_with},
    store::testing::{self, TestStore},
};

const HUB: &str = "https://hub.example.org";

/// The neutral site (plans `free` and `team`) with sign-up `mode`; its free plan connects
/// one AI app at a time.
fn site(mode: &str) -> Arc<Site> {
    let mode = mode.to_string();
    Arc::new(neutral_with(move |dir| {
        let path = dir.join("site.json");
        let mut value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        value["early_access"]["mode"] = mode.into();
        value["plans"][0]["limits"]["clients"] = 1.into();
        std::fs::write(&path, value.to_string()).unwrap();
    }))
}

fn hosted() -> TenancyConfig {
    TenancyConfig {
        mode: TenancyMode::Multi,
        starts_per_hour: 100,
        max_unsecured: 100,
        unsecured: UnsecuredLimits {
            machines: 10,
            clients: 10,
            idle_days: 14,
        },
        ..TenancyConfig::default()
    }
}

/// Accounts on every store, a hosted hub reading `cell`, `operators` its operators.
async fn stores(cell: &Arc<SiteCell>, operators: &[&str]) -> Vec<(TestStore, Arc<Accounts>)> {
    let mut out = Vec::new();
    for store in testing::stores().await {
        let accounts = Accounts::open(store.store.clone())
            .await
            .unwrap()
            .with_tenancy(hosted(), operators.iter().map(|o| o.to_string()).collect())
            .with_site(cell.clone(), HUB);
        out.push((store, Arc::new(accounts)));
    }
    out
}

fn ip(n: u8) -> IpAddr {
    IpAddr::from([198, 51, 100, n])
}

fn proven(provider: &str, subject: &str, email: Option<&str>) -> Proven {
    Proven {
        provider: provider.into(),
        subject: subject.into(),
        verified_email: email.map(str::to_string),
        name: None,
    }
}

/// A person's first connect: their account (with its access) and its browser session.
async fn first_connect(accounts: &Accounts, n: u8) -> (Account, String) {
    accounts.start_unsecured(ip(n), "Claude").await.unwrap()
}

/// What a request of `session`'s sees: its access, settled now.
async fn seen(accounts: &Accounts, session: &str) -> Access {
    accounts
        .session(session)
        .await
        .unwrap()
        .expect("a live session")
        .account
        .access
        .expect("an account in a tenant has its access")
}

fn events(list: &[tenants::Event], name: &str) -> Vec<tenants::Event> {
    list.iter().filter(|e| e.event == name).cloned().collect()
}

#[tokio::test]
async fn a_new_tenant_waits_in_early_access_and_is_let_in_when_sign_up_opens() {
    let cell = Arc::new(SiteCell::new(site("early_access")));
    for (_test, accounts) in stores(&cell, &[]).await {
        cell.set(site("early_access"));
        let home = accounts.home_tenant().await.unwrap();
        let (made, session) = first_connect(&accounts, 1).await;
        let access = made.access.clone().unwrap();
        assert!(
            access.waiting && access.waiting_since.is_some(),
            "{access:?}"
        );
        assert_eq!(access.plan.as_deref(), Some("free"), "the default plan");
        assert_eq!(seen(&accounts, &session).await, access, "kept, unchanged");
        let chain = accounts.events(&made.tenant, 20).await.unwrap();
        assert_eq!(events(&chain, "tenant.waiting").len(), 1);
        assert!(events(&chain, "tenant.admitted").is_empty());
        // The account is secured as ever while it waits.
        let secured = accounts
            .link_identity(&made.id, &proven("github", "7", None))
            .await
            .unwrap();
        assert!(!secured.provisional);
        assert!(seen(&accounts, &session).await.waiting);

        // Sign-up opens: its next request lets it in, as open sign-up, in both chains.
        cell.set(site("open"));
        let access = seen(&accounts, &session).await;
        assert!(!access.waiting, "{access:?}");
        assert_eq!(access.admitted_by.as_deref(), Some(OPEN_SIGN_UP));
        assert!(access.admitted_at.is_some());
        let chain = accounts.events(&made.tenant, 20).await.unwrap();
        let admitted = events(&chain, "tenant.admitted");
        assert_eq!(admitted.len(), 1);
        assert_eq!(
            (admitted[0].actor.as_str(), admitted[0].detail.as_str()),
            ("hub", OPEN_SIGN_UP)
        );
        let at_home = events(
            &accounts.events(&home, 50).await.unwrap(),
            "tenant.admitted",
        );
        assert_eq!(at_home.len(), 1);
        assert!(at_home[0].detail.starts_with(&made.tenant), "{at_home:?}");
        // A tenant made now is in at once.
        let (open, _) = first_connect(&accounts, 2).await;
        assert!(!open.access.clone().unwrap().waiting);

        // Early access again: nobody already in is put back; a new tenant waits.
        cell.set(site("early_access"));
        assert!(!seen(&accounts, &session).await.waiting);
        let (later, _) = first_connect(&accounts, 3).await;
        assert!(later.access.clone().unwrap().waiting);
        for tenant in [&made.tenant, &open.tenant, &later.tenant, &home] {
            assert!(accounts.chain_intact(tenant).await.unwrap(), "{tenant}");
        }
    }
}

#[tokio::test]
async fn the_operators_home_tenant_is_always_in() {
    let cell = Arc::new(SiteCell::new(site("early_access")));
    for (_test, accounts) in stores(&cell, &["github:42"]).await {
        let home = accounts.home_tenant().await.unwrap();
        let kept = accounts.access_of(&home).await.unwrap().unwrap();
        assert!(!kept.waiting);
        let owner = accounts
            .sign_in_identity(&proven("github", "42", None))
            .await
            .unwrap();
        assert!(owner.owner && owner.tenant == home);
        // Even marked waiting under the API, its next request lets it in.
        sqlx::query("UPDATE tenancy_access SET state = 'waiting' WHERE tenant_id = $1")
            .bind(&home)
            .execute(accounts.store().admin())
            .await
            .unwrap();
        assert_eq!(
            accounts
                .account_access(&owner.id)
                .await
                .unwrap()
                .map(|a| a.waiting),
            Some(false)
        );
        // A stranger's sign-in makes a tenant of its own, waiting.
        let stranger = accounts
            .sign_in_identity(&proven("google", "g-1", Some("sam@example.org")))
            .await
            .unwrap();
        assert!(
            accounts
                .access_of(&stranger.tenant)
                .await
                .unwrap()
                .unwrap()
                .waiting
        );
    }
}

#[tokio::test]
async fn tenants_from_before_early_access_are_in() {
    let mut urls = Vec::new();
    let dir = tempfile::tempdir().unwrap();
    urls.push(crate::store::sqlite_url(
        &dir.path().join("accounts.sqlite3"),
    ));
    if let Some(url) = testing::empty_postgres().await {
        urls.push(url);
    }
    for url in urls {
        // A hosted hub's store as migration 7 left it, with a person's tenant in it.
        testing::migrated_to(&url, 7).await;
        let mut old = <sqlx::AnyConnection as sqlx::Connection>::connect(&url)
            .await
            .unwrap();
        let prefix = if url.starts_with("postgres") {
            "devagotchi."
        } else {
            ""
        };
        for sql in [
            format!(
                "INSERT INTO {prefix}tenancy_tenants (id, home, created_at) VALUES ('t-old', 0, 5)"
            ),
            format!(
                "INSERT INTO {prefix}auth_accounts (tenant_id, id, name, owner, created_at, provisional)
                 VALUES ('t-old', 'acct_old', 'Old', 0, 5, 0)"
            ),
        ] {
            sqlx::query(sqlx::AssertSqlSafe(sql))
                .execute(&mut old)
                .await
                .unwrap();
        }
        <sqlx::AnyConnection as sqlx::Connection>::close(old)
            .await
            .unwrap();
        let accounts = Accounts::open(crate::store::Store::open(&url).await.unwrap())
            .await
            .unwrap()
            .with_tenancy(hosted(), Vec::new())
            .with_site(Arc::new(SiteCell::new(site("early_access"))), HUB);
        let access = accounts.access_of("t-old").await.unwrap().unwrap();
        assert!(!access.waiting, "{access:?}");
        assert_eq!(access.admitted_by.as_deref(), Some("before early access"));
        assert_eq!(access.admitted_at, Some(5));
        assert_eq!(access.plan, None, "the site's default plan");
        let home = accounts.home_tenant().await.unwrap();
        assert!(!accounts.access_of(&home).await.unwrap().unwrap().waiting);
        assert_eq!(
            accounts
                .account_access("acct_old")
                .await
                .unwrap()
                .map(|a| a.waiting),
            Some(false)
        );
    }
}

#[tokio::test]
async fn an_operator_lets_tenants_in_on_a_plan_and_changes_plans_on_the_record() {
    let cell = Arc::new(SiteCell::new(site("early_access")));
    for (_test, accounts) in stores(&cell, &["github:42"]).await {
        let heard = Arc::new(Mutex::new(Vec::new()));
        let into = heard.clone();
        accounts.on_admitted(Arc::new(move |admitted| {
            into.lock().unwrap().push(admitted)
        }));
        let operator = accounts
            .sign_in_identity(&proven("github", "42", None))
            .await
            .unwrap();
        let home = operator.tenant.clone();
        // Three waiting, oldest first; one with a verified address.
        let mut waiting = Vec::new();
        for n in 1..=3u8 {
            let (made, _) = first_connect(&accounts, n).await;
            if n == 2 {
                accounts
                    .link_identity(&made.id, &proven("google", "g-2", Some("Two@Example.org")))
                    .await
                    .unwrap();
            }
            sqlx::query("UPDATE tenancy_access SET waiting_since = $2 WHERE tenant_id = $1")
                .bind(&made.tenant)
                .bind(i64::from(n) * 100)
                .execute(accounts.store().admin())
                .await
                .unwrap();
            waiting.push(made.tenant);
        }
        assert_eq!(accounts.tenants_in(WAITING, 10).await.unwrap(), waiting);
        assert_eq!(accounts.tenants_in(WAITING, 2).await.unwrap(), waiting[..2]);
        assert_eq!(
            accounts.tenants_in(ADMITTED, 10).await.unwrap(),
            [home.clone()]
        );

        // Unknown plans and tenants are refused, and nothing changes.
        let unknown = accounts
            .admit(&waiting[..1], Some("gold"), &operator.id)
            .await
            .unwrap_err();
        assert_eq!(unknown.code, "unknown_plan");
        let nobody = accounts
            .admit(&[waiting[0].clone(), "t-nobody".into()], None, &operator.id)
            .await
            .unwrap_err();
        assert_eq!(nobody.code, "unknown_tenant");
        assert_eq!(accounts.tenants_in(WAITING, 10).await.unwrap(), waiting);

        // Two let in at once on the team plan; the third still waits.
        let admitted = accounts
            .admit(&waiting[..2], Some("team"), &operator.id)
            .await
            .unwrap();
        assert_eq!(admitted.len(), 2);
        assert_eq!(admitted[1].email.as_deref(), Some("two@example.org"));
        assert_eq!(admitted[0].email, None);
        assert_eq!(
            *heard.lock().unwrap(),
            admitted,
            "each one let in is heard of"
        );
        for tenant in &waiting[..2] {
            let access = accounts.access_of(tenant).await.unwrap().unwrap();
            assert!(!access.waiting);
            assert_eq!(access.admitted_by.as_deref(), Some(operator.id.as_str()));
            assert_eq!(access.plan.as_deref(), Some("team"));
            let chain = events(
                &accounts.events(tenant, 20).await.unwrap(),
                "tenant.admitted",
            );
            assert_eq!(chain.len(), 1);
            assert_eq!(chain[0].actor, operator.id, "the operator is the actor");
        }
        assert_eq!(
            accounts.tenants_in(WAITING, 10).await.unwrap(),
            waiting[2..]
        );
        // Letting in one already in changes nothing and is heard of no more.
        assert!(
            accounts
                .admit(&waiting[..1], Some("free"), &operator.id)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(heard.lock().unwrap().len(), 2);

        // A plan change, on the record in the tenant's chain and the home tenant's.
        accounts
            .set_plan(&waiting[0], "free", &operator.id)
            .await
            .unwrap();
        assert_eq!(
            accounts
                .access_of(&waiting[0])
                .await
                .unwrap()
                .unwrap()
                .plan
                .as_deref(),
            Some("free")
        );
        assert_eq!(
            accounts
                .set_plan(&waiting[0], "gold", &operator.id)
                .await
                .unwrap_err()
                .code,
            "unknown_plan"
        );
        assert_eq!(
            accounts
                .set_plan("t-nobody", "free", &operator.id)
                .await
                .unwrap_err()
                .code,
            "unknown_tenant"
        );
        let at_home = accounts.events(&home, 50).await.unwrap();
        assert_eq!(events(&at_home, "tenant.admitted").len(), 2);
        let plans = events(&at_home, "tenant.plan");
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].detail, format!("{}: free", waiting[0]));
        assert_eq!(plans[0].actor, operator.id);
        assert_eq!(
            events(
                &accounts.events(&waiting[0], 20).await.unwrap(),
                "tenant.plan"
            )
            .len(),
            1
        );
        for tenant in waiting.iter().chain([&home]) {
            assert!(accounts.chain_intact(tenant).await.unwrap(), "{tenant}");
        }
    }
}

#[tokio::test]
async fn a_plan_holds_a_tenant_to_its_ai_apps_and_a_lost_plan_reads_as_the_default() {
    let cell = Arc::new(SiteCell::new(site("open")));
    for (_test, accounts) in stores(&cell, &[]).await {
        let (made, _) = first_connect(&accounts, 1).await;
        let client = accounts
            .register_client("An AI", &["https://ai.example.net/cb".to_string()])
            .await
            .unwrap();
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = crate::accounts::oauth::s256(verifier);
        let connect = || async {
            let code = accounts
                .issue_code(
                    &client.id,
                    &made.id,
                    "https://ai.example.net/cb",
                    &challenge,
                    "https://hub.example.org/mcp",
                    "mcp",
                )
                .await?;
            accounts
                .exchange_code(
                    &code,
                    &client.id,
                    "https://ai.example.net/cb",
                    verifier,
                    None,
                )
                .await
        };
        // The free plan's one AI app: the second is refused in the site's words.
        connect().await.unwrap();
        let refused = connect().await.unwrap_err();
        assert_eq!(refused.code, "plan_limit");
        assert!(
            refused.message.contains("Starter")
                && refused.message.contains("1 assistant")
                && refused.message.contains("https://hub.example.org/#plans"),
            "{}",
            refused.message
        );
        // The team plan has room for ten.
        accounts
            .set_plan(&made.tenant, "team", "acct_operator")
            .await
            .unwrap();
        connect().await.unwrap();
        // A plan the site no longer lists is the default plan again.
        sqlx::query("UPDATE tenancy_access SET plan = 'retired' WHERE tenant_id = $1")
            .bind(&made.tenant)
            .execute(accounts.store().admin())
            .await
            .unwrap();
        let access = accounts.access_of(&made.tenant).await.unwrap().unwrap();
        assert_eq!(
            accounts.plan_of(&made.tenant, &access).map(|plan| plan.id),
            Some("free".to_string())
        );
        assert_eq!(connect().await.unwrap_err().code, "plan_limit");
    }
}

#[tokio::test]
async fn on_postgres_a_tenants_access_is_its_own_and_the_operators_list_names_tenants_only() {
    let Some(test) = testing::postgres().await else {
        return;
    };
    let accounts = Accounts::open(test.store.clone())
        .await
        .unwrap()
        .with_tenancy(hosted(), Vec::new())
        .with_site(Arc::new(SiteCell::new(site("early_access"))), HUB);
    assert!(accounts.store().enforced());
    let (a, _) = first_connect(&accounts, 1).await;
    let (b, _) = first_connect(&accounts, 2).await;
    let mut hub = accounts.store().pool().begin().await.unwrap();
    let count =
        |filter: &str| sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM tenancy_access {filter}"));
    let unscoped: i64 = sqlx::query_scalar(count(""))
        .fetch_one(&mut *hub)
        .await
        .unwrap();
    assert_eq!(unscoped, 0, "unscoped, no tenant's row");
    accounts.store().scope(&mut hub, &a.tenant).await.unwrap();
    let mine: i64 = sqlx::query_scalar(count(""))
        .fetch_one(&mut *hub)
        .await
        .unwrap();
    assert_eq!(mine, 1, "scoped to A: A's row alone");
    let changed = sqlx::query("UPDATE tenancy_access SET state = 'admitted' WHERE tenant_id = $1")
        .bind(&b.tenant)
        .execute(&mut *hub)
        .await
        .unwrap()
        .rows_affected();
    assert_eq!(changed, 0, "B's row, from A's scope");
    // The operator's list answers ids in one state, across tenants, and nothing else.
    let listed = accounts
        .store()
        .tenants_in(&mut hub, WAITING, 10)
        .await
        .unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed.contains(&a.tenant) && listed.contains(&b.tenant));
    hub.rollback().await.unwrap();
    assert!(
        accounts
            .access_of(&b.tenant)
            .await
            .unwrap()
            .unwrap()
            .waiting
    );
}
