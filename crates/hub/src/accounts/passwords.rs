//! Email and password sign-in on a hosted hub (spec first-connect, A1). Sign-up makes the
//! account and tenant as any sign-in does (`identities.rs`), as the sign-in (`email`,
//! <address>) that also leads a sign-in to its tenant, with the argon2id hash beside it in
//! the same transaction (`passwords`, migration 0013). A wrong password and an unknown
//! address answer the same after the same work; ten failures per address in 15 minutes
//! and it waits. Neither the password nor its hash is ever logged.

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use super::{email::normalize_email, identities::Proven, *};

const MIN_LEN: usize = 8;
const FAILURES_MAX: usize = 10; // per address in FAILURES_SECS
const FAILURES_SECS: u64 = 15 * 60;

fn wrong() -> BridgeError { BridgeError::new("password_wrong", "email or password is wrong") }
fn taken() -> BridgeError { BridgeError::new("email_taken", "that email already has an account") }

fn checked(password: &str) -> BridgeResult<()> {
    if password.len() < MIN_LEN {
        return Err(BridgeError::new("password_short", "use at least 8 characters"));
    }
    Ok(())
}

fn hash_with(password: &[u8], salt: &[u8]) -> Option<String> {
    let salt = SaltString::encode_b64(salt).ok()?;
    Argon2::default().hash_password(password, &salt).ok().map(|hash| hash.to_string())
}

/// The argon2id PHC string of `password`, under a fresh salt, off the async threads.
async fn hash_of(password: &str) -> BridgeResult<String> {
    let (password, salt) = (password.to_string(), crate::link::random::<16>());
    tokio::task::spawn_blocking(move || hash_with(password.as_bytes(), &salt))
        .await
        .ok()
        .flatten()
        .ok_or_else(|| BridgeError::new("password_hash", "could not hash the password"))
}

/// Whether `password` is the one `hash` holds; with no hash, the same work against a dummy.
async fn matches(hash: Option<String>, password: &str) -> bool {
    static DUMMY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let password = password.to_string();
    tokio::task::spawn_blocking(move || {
        let dummy = DUMMY.get_or_init(|| hash_with(b"not anyone's password", &[7; 16]).unwrap_or_default());
        let parsed = PasswordHash::new(hash.as_deref().unwrap_or(dummy));
        let ok = parsed.is_ok_and(|p| Argon2::default().verify_password(password.as_bytes(), &p).is_ok());
        ok && hash.is_some()
    })
    .await
    .unwrap_or(false)
}

impl Accounts {
    /// A new account and tenant for `email`, signed in by `password` from now on.
    pub async fn sign_up_password(&self, email: &str, password: &str) -> BridgeResult<Account> {
        let email = normalize_email(email).ok_or_else(|| BridgeError::new("invalid_email", "not an email address"))?;
        checked(password)?;
        let proven = Proven { provider: "email".into(), subject: email.clone(), verified_email: None, name: None };
        if self.is_operator_identity(&proven) {
            return Err(taken());
        }
        let (hash, now) = (hash_of(password).await?, now_secs());
        let (tenant_random, account_random) = (crate::link::random::<16>(), crate::link::random::<16>());
        transaction!(&self.store, |connection| {
            // Any account this address already reaches, by password or email link, is taken.
            if self.enter(connection, Lookup::Identity("email", &email)).await?.is_some() {
                return Err(taken().into());
            }
            let account = self.sign_in_identity_in(connection, &proven, None, tenant_random, account_random, now).await?;
            sqlx::query(
                "INSERT INTO passwords (account_id, tenant_id, email, hash, created_at, updated_at)
                 VALUES ($1, $2, $3, $4, $5, $5)",
            )
            .bind(&account.id).bind(&account.tenant).bind(&email).bind(&hash).bind(secs(now))
            .execute(&mut *connection).await?;
            Ok(account)
        })
    }

    /// The account `email` and `password` reach; `password_wrong` for an unknown address
    /// or a wrong password alike, `slow_down` after too many failures.
    pub async fn sign_in_password(&self, email: &str, password: &str) -> BridgeResult<Account> {
        let email = normalize_email(email).unwrap_or_default();
        let now = now_secs();
        {
            let mut failures = self.password_failures.lock().unwrap_or_else(|e| e.into_inner());
            failures.retain(|_, times| {
                times.retain(|at| now.saturating_sub(*at) < FAILURES_SECS);
                !times.is_empty()
            });
            if failures.get(&email).is_some_and(|times| times.len() >= FAILURES_MAX) {
                return Err(BridgeError::new("slow_down", "too many tries; wait a few minutes"));
            }
        }
        let found: Option<(Account, String)> = transaction!(&self.store, |connection| {
            if email.is_empty() || self.enter(connection, Lookup::Identity("email", &email)).await?.is_none() {
                return Ok(None);
            }
            let row = sqlx::query("SELECT account_id, hash FROM passwords WHERE email = $1").bind(&email);
            let Some(row) = row.fetch_optional(&mut *connection).await? else { return Ok(None) };
            let (id, hash): (String, String) = (row.try_get(0)?, row.try_get(1)?);
            Ok(account_in(connection, &id).await?.map(|account| (account, hash)))
        })?;
        let (account, hash) = found.map_or((None, None), |(a, h)| (Some(a), Some(h)));
        if !matches(hash, password).await {
            let mut failures = self.password_failures.lock().unwrap_or_else(|e| e.into_inner());
            failures.entry(email).or_default().push(now);
            return Err(wrong());
        }
        let account = account.ok_or_else(wrong)?;
        account.disabled_at.map_or(Ok(()), |_| Err(account_disabled(&account.id)))?;
        Ok(account)
    }

    /// Set `account`'s password, or change it (Forgot password, the account page), for the
    /// address its email sign-in proves, else the first address a provider verified for it.
    pub async fn set_password(&self, account: &str, password: &str) -> BridgeResult<()> {
        checked(password)?;
        let (hash, now) = (hash_of(password).await?, now_secs());
        transaction!(&self.store, |connection| {
            self.enter(connection, Lookup::Account(account)).await?;
            let holder = account_in(connection, account).await?
                .ok_or_else(|| BridgeError::new("no_account", format!("no account {account}")))?;
            let first = |sql| sqlx::query_scalar::<_, String>(sql).bind(account);
            let mut email = first("SELECT subject FROM auth_identities WHERE account = $1 AND provider = 'email' ORDER BY created_at LIMIT 1")
                .fetch_optional(&mut *connection).await?;
            if email.is_none() {
                let verified = first("SELECT email FROM auth_identities WHERE account = $1 AND email IS NOT NULL ORDER BY created_at LIMIT 1")
                    .fetch_optional(&mut *connection).await?
                    .ok_or_else(|| BridgeError::new("no_email", "this account has no email to sign in with"))?;
                // Its email sign-in, which is how a password sign-in finds the account.
                if self.store.tenant_of(connection, Lookup::Identity("email", &verified)).await?.is_some() {
                    return Err(taken().into());
                }
                let proven = Proven { provider: "email".into(), subject: verified.clone(), verified_email: Some(verified.clone()), name: None };
                super::identities::insert_identity(connection, &holder, &proven, self.counting, now).await?;
                email = Some(verified);
            }
            sqlx::query(
                "INSERT INTO passwords (account_id, tenant_id, email, hash, created_at, updated_at)
                 VALUES ($1, $2, $3, $4, $5, $5)
                 ON CONFLICT (account_id) DO UPDATE SET hash = excluded.hash, updated_at = excluded.updated_at",
            )
            .bind(&holder.id).bind(&holder.tenant).bind(&email).bind(&hash).bind(secs(now))
            .execute(&mut *connection).await?;
            Ok(())
        })
    }
}
