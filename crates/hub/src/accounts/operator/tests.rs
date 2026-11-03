//! What the operator page reads, on every store this run has (SQLite always, Postgres when
//! the test server is set, as in CI).

use std::{net::IpAddr, sync::Arc};

use super::*;
use crate::{
    accounts::{
        access::{ADMITTED, WAITING},
        identities::Proven,
    },
    config::{TenancyConfig, TenancyMode},
    site::{SiteCell, tests::neutral},
    store::testing,
};

fn proven(provider: &str, subject: &str, email: Option<&str>) -> Proven {
    Proven {
        provider: provider.into(),
        subject: subject.into(),
        verified_email: email.map(str::to_string),
        name: None,
    }
}

#[tokio::test]
async fn each_tenant_is_shown_with_its_sign_ins_its_maker_and_its_activity() {
    // The neutral site has sign-up on early access.
    let cell = Arc::new(SiteCell::new(Arc::new(neutral())));
    for test in testing::stores().await {
        let accounts = Accounts::open(test.store.clone())
            .await
            .unwrap()
            .with_tenancy(
                TenancyConfig {
                    mode: TenancyMode::Multi,
                    starts_per_hour: 100,
                    ..TenancyConfig::default()
                },
                vec!["github:42".into()],
            )
            .with_site(cell.clone(), "https://hub.example.org");
        let operator = accounts
            .sign_in_identity(&proven("github", "42", None))
            .await
            .unwrap();
        let (first, _) = accounts
            .start_unsecured(IpAddr::from([198, 51, 100, 1]), "Claude")
            .await
            .unwrap();
        accounts
            .record(&first.tenant, &first.id, "client.connected", "Claude")
            .await
            .unwrap();
        let (second, _) = accounts
            .start_unsecured(IpAddr::from([198, 51, 100, 2]), "Codex")
            .await
            .unwrap();
        accounts
            .link_identity(
                &second.id,
                &proven("google", "g-2", Some("sam@example.org")),
            )
            .await
            .unwrap();

        let waiting = accounts.tenant_views(WAITING, 10).await.unwrap();
        let tenants: Vec<&str> = waiting.iter().map(|v| v.tenant.as_str()).collect();
        assert_eq!(tenants.len(), 2);
        assert!(
            tenants.contains(&first.tenant.as_str()) && tenants.contains(&second.tenant.as_str())
        );
        let one = waiting.iter().find(|v| v.tenant == first.tenant).unwrap();
        assert!(one.access.waiting && one.access.waiting_since.is_some());
        assert_eq!(one.accounts, [first.id.clone()]);
        assert!(!one.secured && one.sign_ins.is_empty());
        assert_eq!(one.made_by.as_deref(), Some("Claude"));
        assert!(one.last_active.is_some());
        let two = waiting.iter().find(|v| v.tenant == second.tenant).unwrap();
        assert!(two.secured);
        assert_eq!(two.sign_ins.len(), 1);
        assert_eq!(two.sign_ins[0].provider, "google");
        assert_eq!(two.sign_ins[0].email.as_deref(), Some("sam@example.org"));
        assert_eq!(two.made_by, None, "no AI app connected yet");
        assert_eq!(accounts.tenant_views(WAITING, 1).await.unwrap().len(), 1);

        // The operator's own tenant is never on either list.
        assert!(
            accounts
                .tenant_views(ADMITTED, 10)
                .await
                .unwrap()
                .is_empty()
        );
        accounts
            .admit(
                std::slice::from_ref(&first.tenant),
                Some("team"),
                &operator.id,
            )
            .await
            .unwrap();
        let admitted = accounts.tenant_views(ADMITTED, 10).await.unwrap();
        assert_eq!(admitted.len(), 1);
        assert_eq!(admitted[0].tenant, first.tenant);
        assert_eq!(admitted[0].access.plan.as_deref(), Some("team"));
        assert_eq!(
            admitted[0].access.admitted_by.as_deref(),
            Some(operator.id.as_str())
        );
        assert!(admitted.iter().all(|v| v.tenant != operator.tenant));
        assert_eq!(accounts.tenant_views(WAITING, 10).await.unwrap().len(), 1);
    }
}
