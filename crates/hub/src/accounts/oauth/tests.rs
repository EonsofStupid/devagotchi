use super::*;
use crate::{
    config::TokenConfig,
    store::{
        Backend,
        testing::{self, TestStore},
    },
};

const A: &str = "acct_aaaaaaaaaaaaaaaaaaaaaaaaaa";
const RESOURCE: &str = "https://hub.example.com/mcp";
const REDIRECT: &str = "https://ai.example.net/oauth/callback";
/// RFC 7636 appendix B's verifier.
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";

/// The owner account in every store this run has, with the default token lifetimes.
async fn stores() -> Vec<(TestStore, Accounts)> {
    let mut out = Vec::new();
    for store in testing::stores().await {
        let accounts = Accounts::open(store.store.clone()).await.unwrap();
        accounts.create(A, "Jesse", true).await.unwrap();
        out.push((store, accounts));
    }
    out
}

async fn client(accounts: &Accounts) -> Client {
    accounts
        .register_client("An AI", &[REDIRECT.to_string()])
        .await
        .unwrap()
}

async fn code(accounts: &Accounts, client: &Client) -> String {
    accounts
        .issue_code(&client.id, A, REDIRECT, &s256(VERIFIER), RESOURCE, "mcp offline_access")
        .await
        .unwrap()
}

async fn connect(accounts: &Accounts, client: &Client) -> Tokens {
    accounts
        .exchange_code(&code(accounts, client).await, &client.id, REDIRECT, VERIFIER, Some(RESOURCE))
        .await
        .unwrap()
}

#[test]
fn pkce_is_rfc_7636s_example() {
    assert_eq!(s256(VERIFIER), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
}

#[test]
fn redirect_uris_are_https_or_loopback() {
    for good in [
        REDIRECT,
        "https://other.example.org/cb?x=1",
        "http://localhost:33418/callback",
        "http://127.0.0.1:5173/cb",
        "http://[::1]:8080/cb",
    ] {
        assert!(valid_redirect_uri(good), "{good}");
    }
    for bad in [
        "http://evil.example/cb",
        "https://ai.example.net/cb#frag",
        "javascript:alert(1)",
        "app://callback",
        "/relative",
        "https://ai.example.net/a b",
        "",
    ] {
        assert!(!valid_redirect_uri(bad), "{bad}");
    }
}

#[tokio::test]
async fn clients_register_with_valid_redirects_and_the_oldest_unused_make_room() {
    for (_store, accounts) in stores().await {
        assert_eq!(
            accounts
                .register_client("x", &["http://evil.example/cb".into()])
                .await
                .unwrap_err()
                .code,
            "invalid_redirect_uri"
        );
        assert!(accounts.register_client("x", &[]).await.is_err());
        let first = client(&accounts).await;
        assert!(first.id.starts_with("dgc_"));
        assert_eq!(accounts.client(&first.id).await.unwrap().unwrap(), first);
        assert!(accounts.client("dgc_nobody").await.unwrap().is_none());

        // A hub that keeps two clients: a third pushes out the oldest one never used.
        let accounts = accounts.with_tokens(TokenConfig {
            max_clients: 2,
            ..TokenConfig::default()
        });
        let used = client(&accounts).await;
        connect(&accounts, &used).await;
        let third = client(&accounts).await;
        assert!(accounts.client(&first.id).await.unwrap().is_none(), "never used, oldest");
        assert!(accounts.client(&used.id).await.unwrap().is_some());
        assert!(accounts.client(&third.id).await.unwrap().is_some());
    }
}

#[tokio::test]
async fn a_code_becomes_tokens_once_for_its_client_redirect_verifier_and_resource() {
    for (_store, accounts) in stores().await {
        let client = client(&accounts).await;
        let other = accounts
            .register_client("Other", &[REDIRECT.to_string()])
            .await
            .unwrap();
        let refused = async |code: &str, client: &str, redirect: &str, verifier: &str, resource: &str| {
            accounts
                .exchange_code(code, client, redirect, verifier, Some(resource))
                .await
                .unwrap_err()
                .code
        };
        let short = "x".repeat(42);
        let wrong = "x".repeat(43);
        // Each wrong part refuses, and the code is gone with the first try.
        for (who, redirect, verifier, resource) in [
            (other.id.as_str(), REDIRECT, VERIFIER, RESOURCE),
            (client.id.as_str(), "https://ai.example.net/other", VERIFIER, RESOURCE),
            (client.id.as_str(), REDIRECT, wrong.as_str(), RESOURCE),
            (client.id.as_str(), REDIRECT, short.as_str(), RESOURCE),
            (client.id.as_str(), REDIRECT, VERIFIER, "https://evil.example/mcp"),
        ] {
            let c = code(&accounts, &client).await;
            assert_eq!(
                refused(&c, who, redirect, verifier, resource).await,
                "invalid_grant"
            );
            assert_eq!(
                refused(&c, &client.id, REDIRECT, VERIFIER, RESOURCE).await,
                "invalid_grant",
                "a refused code is spent"
            );
        }
        let c = code(&accounts, &client).await;
        let tokens = accounts
            .exchange_code(&c, &client.id, REDIRECT, VERIFIER, Some(RESOURCE))
            .await
            .unwrap();
        assert!(tokens.access.starts_with("dgh_") && tokens.refresh.starts_with("dgr_"));
        assert_eq!(tokens.expires_in, TokenConfig::default().access_secs);
        assert_eq!(tokens.scope, "mcp offline_access");
        let grant = accounts.verify_access(&tokens.access).await.unwrap().unwrap();
        assert_eq!(
            (grant.account.id.as_str(), grant.client.as_str()),
            (A, client.id.as_str())
        );
        assert_eq!(
            refused(&c, &client.id, REDIRECT, VERIFIER, RESOURCE).await,
            "invalid_grant",
            "once"
        );
        // A code past its lifetime.
        let limits = TokenConfig::default();
        let late = code(&accounts, &client).await;
        assert_eq!(
            accounts
                .exchange_code_at(&late, &client.id, REDIRECT, VERIFIER, Some(RESOURCE), now_secs() + limits.code_secs)
                .await
                .unwrap_err()
                .code,
            "invalid_grant"
        );
    }
}

#[tokio::test]
async fn lifetimes_come_from_the_config() {
    for (_store, accounts) in stores().await {
        let limits = TokenConfig {
            access_secs: 300,
            refresh_secs: 3600,
            code_secs: 30,
            max_clients: 10,
        };
        let accounts = accounts.with_tokens(limits);
        let client = client(&accounts).await;
        let now = now_secs();
        let c = accounts
            .issue_code_at(&client.id, A, REDIRECT, &s256(VERIFIER), RESOURCE, "mcp", now)
            .await
            .unwrap();
        assert!(
            accounts
                .exchange_code_at(&c, &client.id, REDIRECT, VERIFIER, Some(RESOURCE), now + 30)
                .await
                .is_err(),
            "the code lives 30 seconds"
        );
        let tokens = connect(&accounts, &client).await;
        assert_eq!(tokens.expires_in, 300);
        let now = now_secs();
        assert!(accounts.verify_access_at(&tokens.access, now + 299).await.unwrap().is_some());
        assert!(accounts.verify_access_at(&tokens.access, now + 301).await.unwrap().is_none());
        assert!(
            accounts
                .refresh_at(&tokens.refresh, &client.id, Some(RESOURCE), now + 3601)
                .await
                .is_err(),
            "the connection lives an hour unused"
        );
    }
}

#[tokio::test]
async fn refresh_tokens_rotate_and_a_spent_one_ends_the_family() {
    for (_store, accounts) in stores().await {
        let client = client(&accounts).await;
        let first = connect(&accounts, &client).await;
        let second = accounts.refresh(&first.refresh, &client.id, Some(RESOURCE)).await.unwrap();
        assert_ne!(second.access, first.access);
        assert!(accounts.verify_access(&first.access).await.unwrap().is_none(), "rotated out");
        assert!(accounts.verify_access(&second.access).await.unwrap().is_some());
        // The spent refresh token again: the whole family ends, the new pair included.
        assert_eq!(
            accounts
                .refresh(&first.refresh, &client.id, Some(RESOURCE))
                .await
                .unwrap_err()
                .code,
            "invalid_grant"
        );
        assert!(accounts.verify_access(&second.access).await.unwrap().is_none());
        assert!(accounts.refresh(&second.refresh, &client.id, Some(RESOURCE)).await.is_err());
        // Another client, another resource, or past its lifetime unused: refused.
        let third = connect(&accounts, &client).await;
        assert!(accounts.refresh(&third.refresh, "dgc_other", Some(RESOURCE)).await.is_err());
        assert!(
            accounts
                .refresh(&third.refresh, &client.id, Some("https://evil.example/mcp"))
                .await
                .is_err()
        );
        let lifetime = TokenConfig::default().refresh_secs;
        assert!(
            accounts
                .refresh_at(&third.refresh, &client.id, Some(RESOURCE), now_secs() + lifetime)
                .await
                .is_err()
        );
        assert!(accounts.refresh(&third.refresh, &client.id, Some(RESOURCE)).await.is_ok());
    }
}

#[tokio::test]
async fn two_instances_refreshing_one_token_at_once_get_one_new_pair() {
    // A client retrying a refresh against two hub instances: one rotation counts, the
    // other is refused, and the family stays alive with exactly one live access token.
    for (store, one) in stores().await {
        let two = Accounts::open(Store::open(&store.url).await.unwrap())
            .await
            .unwrap();
        let client = client(&one).await;
        let tokens = connect(&one, &client).await;
        let (a, b) = tokio::join!(
            one.refresh(&tokens.refresh, &client.id, Some(RESOURCE)),
            two.refresh(&tokens.refresh, &client.id, Some(RESOURCE)),
        );
        assert!(a.is_ok() || b.is_ok(), "{:?} / {:?}", a.as_ref().err(), b.as_ref().err());
        let live: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM oauth_tokens WHERE kind = 'access'",
        )
        .fetch_one(one.store().pool())
        .await
        .unwrap();
        // Both succeeding would leave two; a refused second may have ended the family (0).
        assert!(live <= 1, "{live} live access tokens");
    }
}

#[tokio::test]
async fn access_tokens_expire_die_with_their_account_and_can_be_revoked() {
    for (_store, accounts) in stores().await {
        let client = client(&accounts).await;
        let tokens = connect(&accounts, &client).await;
        let lifetime = TokenConfig::default().access_secs;
        assert!(
            accounts
                .verify_access_at(&tokens.access, now_secs() + lifetime)
                .await
                .unwrap()
                .is_none()
        );
        for wrong in ["", "dgh_", "dgh_nonsense", &tokens.refresh] {
            assert!(accounts.verify_access(wrong).await.unwrap().is_none(), "{wrong:?}");
        }
        let disable = async |at: Option<i64>| {
            sqlx::query("UPDATE accounts SET disabled_at = $1 WHERE id = $2")
                .bind(at)
                .bind(A)
                .execute(accounts.store().pool())
                .await
                .unwrap();
        };
        disable(Some(1)).await;
        assert!(accounts.verify_access(&tokens.access).await.unwrap().is_none());
        assert_eq!(
            accounts
                .refresh(&tokens.refresh, &client.id, Some(RESOURCE))
                .await
                .unwrap_err()
                .code,
            "account_disabled"
        );
        disable(None).await;
        assert!(accounts.verify_access(&tokens.access).await.unwrap().is_some());
        accounts.revoke_token(&tokens.refresh).await.unwrap();
        assert!(
            accounts.verify_access(&tokens.access).await.unwrap().is_none(),
            "the family ends"
        );
        accounts.revoke_token("dgh_unknown").await.unwrap();
        let again = connect(&accounts, &client).await;
        assert_eq!(accounts.revoke_connections(A).await.unwrap(), 2);
        assert!(accounts.verify_access(&again.access).await.unwrap().is_none());
    }
}

#[tokio::test]
async fn tokens_and_codes_are_stored_only_as_hashes() {
    for (store, accounts) in stores().await {
        let client = client(&accounts).await;
        let pending = code(&accounts, &client).await;
        let tokens = connect(&accounts, &client).await;
        let hashes: Vec<String> = sqlx::query_scalar(
            "SELECT hash FROM oauth_tokens UNION ALL SELECT hash FROM oauth_codes",
        )
        .fetch_all(accounts.store().pool())
        .await
        .unwrap();
        for secret in [&tokens.access, &tokens.refresh, &pending] {
            assert!(hashes.contains(&hash(secret)));
            assert!(!hashes.contains(secret));
        }
        if store.store.backend() == Backend::Sqlite {
            sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                .execute(accounts.store().pool())
                .await
                .unwrap();
            let mut raw = std::fs::read(store.dir.path().join("accounts.sqlite3")).unwrap();
            raw.extend(
                std::fs::read(store.dir.path().join("accounts.sqlite3-wal")).unwrap_or_default(),
            );
            for secret in [&tokens.access, &tokens.refresh, &pending] {
                assert!(!raw.windows(secret.len()).any(|w| w == secret.as_bytes()));
            }
        }
    }
}
