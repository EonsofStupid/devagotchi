//! Sign-ins an account is reached by besides its passkeys: Google, GitHub, an email link,
//! Sign in with Shippin (Nomen), any OpenID Connect provider. Each is `(provider, subject)`,
//! the provider's own stable id for the person, never a name or an email (either can
//! change hands). The flows that prove one are the web's (`web/providers.rs`); this is what
//! the hub does with a proven one:
//!
//! - Signing in (`sign_in_identity`): the account it is linked to. Else, when it is one of
//!   the config's `operators`, the hub's owner (the platform operator), made in the home
//!   tenant if there is none yet: no claim. Else the account a verified email already
//!   reaches, linked now. Else, on a hosted hub, a new account in a tenant of its own,
//!   secured from the start. A one-owner hub makes no account for a stranger.
//! - Linking (`link_identity`) to the account a browser is signed in to: how an account
//!   made on first connect is secured (the anonymous-then-link upgrade). A sign-in already
//!   linked to another account, or an operator's sign-in on any account but the owner's, is
//!   refused.
//!
//! An email counts only when the provider says it verified it; only verified ones are kept.

use super::*;
use crate::{analytics::funnel, store::Lookup};

/// A sign-in a provider proved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proven {
    /// `google`, `github`, `email`, `nomen`, or a configured OpenID Connect provider's name.
    pub provider: String,
    /// The provider's stable id for the person (`sub`, GitHub's numeric id, the address
    /// for `email`).
    pub subject: String,
    /// The address, only when the provider verified it.
    pub verified_email: Option<String>,
    /// What to call a new account.
    pub name: Option<String>,
}

impl Proven {
    fn email(&self) -> Option<String> {
        self.verified_email
            .as_deref()
            .map(|email| email.trim().to_ascii_lowercase())
            .filter(|email| email.contains('@') && email.len() <= 254)
    }

    fn check(&self) -> BridgeResult<()> {
        let fine = |text: &str, max: usize| {
            !text.is_empty() && text.len() <= max && !text.chars().any(char::is_control)
        };
        if !fine(&self.provider, 32) || !fine(&self.subject, 256) {
            return Err(BridgeError::new(
                "invalid_identity",
                "a sign-in has no usable id",
            ));
        }
        Ok(())
    }

    fn account_name(&self) -> String {
        let name: String = self
            .name
            .as_deref()
            .or(self.verified_email.as_deref())
            .unwrap_or("Your account")
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .take(64)
            .collect();
        if name.is_empty() {
            "Your account".into()
        } else {
            name
        }
    }
}

/// A sign-in linked to an account, as the account page lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Linked {
    pub provider: String,
    pub email: Option<String>,
}

/// The account a sign-in is linked to, inside the caller's transaction, which is scoped to
/// that account's tenant (to none when the sign-in is linked to nothing).
async fn linked_account(
    accounts: &Accounts,
    connection: &mut AnyConnection,
    proven: &Proven,
) -> Result<Option<Account>, Failed> {
    if accounts
        .enter(
            connection,
            Lookup::Identity(&proven.provider, &proven.subject),
        )
        .await?
        .is_none()
    {
        return Ok(None);
    }
    let found: Option<String> =
        sqlx::query("SELECT account FROM auth_identities WHERE provider = $1 AND subject = $2")
            .bind(&proven.provider)
            .bind(&proven.subject)
            .fetch_optional(&mut *connection)
            .await?
            .map(|row| row.try_get(0))
            .transpose()?;
    match found {
        Some(id) => Ok(account_in(connection, &id).await?),
        None => Ok(None),
    }
}

async fn insert_identity(
    connection: &mut AnyConnection,
    account: &Account,
    proven: &Proven,
    counting: bool,
    now: u64,
) -> Result<(), Failed> {
    sqlx::query(
        "INSERT INTO auth_identities (provider, subject, account, email, created_at, tenant_id)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(&proven.provider)
    .bind(&proven.subject)
    .bind(&account.id)
    .bind(proven.email())
    .bind(secs(now))
    .bind(&account.tenant)
    .execute(&mut *connection)
    .await?;
    tenants::record_in(
        connection,
        &account.tenant,
        &account.id,
        "identity.linked",
        &proven.provider,
        now,
    )
    .await?;
    tenants::secure_in(connection, account, &proven.provider, counting, now).await?;
    Ok(())
}

impl Accounts {
    /// Whether this sign-in is one the config names as the hub's operator.
    pub fn is_operator_identity(&self, proven: &Proven) -> bool {
        crate::config::is_operator(
            &self.operators,
            &proven.provider,
            &proven.subject,
            proven.email().as_deref(),
        )
    }

    /// The account a proven sign-in reaches, made or linked as the module says.
    pub async fn sign_in_identity(&self, proven: &Proven) -> BridgeResult<Account> {
        self.sign_in_identity_at(
            proven,
            None,
            crate::link::random::<16>(),
            crate::link::random::<16>(),
            now_secs(),
        )
        .await
    }

    /// `sign_in_identity` from a browser already signed in to `current`: a sign-in that
    /// reaches no account yet is linked to `current` when nobody has secured it (the
    /// anonymous-then-link upgrade), rather than making another account.
    pub async fn sign_in_identity_from(
        &self,
        proven: &Proven,
        current: Option<&str>,
    ) -> BridgeResult<Account> {
        self.sign_in_identity_at(
            proven,
            current,
            crate::link::random::<16>(),
            crate::link::random::<16>(),
            now_secs(),
        )
        .await
    }

    pub(crate) async fn sign_in_identity_at(
        &self,
        proven: &Proven,
        current: Option<&str>,
        tenant_random: [u8; 16],
        account_random: [u8; 16],
        now: u64,
    ) -> BridgeResult<Account> {
        proven.check()?;
        let operator = self.is_operator_identity(proven);
        let multi = self.tenancy.multi();
        let fresh_id = new_account_id(account_random);
        let fresh_tenant = tenants::new_tenant_id(tenant_random);
        let name = proven.account_name();
        let email = proven.email();
        transaction!(&self.store, |connection| {
            if let Some(account) = linked_account(self, connection, proven).await? {
                if account.disabled_at.is_some() {
                    return Err(account_disabled(&account.id).into());
                }
                return Ok(account);
            }
            if operator {
                // The operator, with no claim: the owner, made in the home tenant if the
                // hub has none yet.
                let owner = match self.owner_in(connection).await? {
                    Some(owner) => owner,
                    None => {
                        let home = tenants::home_in(connection).await?;
                        self.store.scope(connection, &home).await?;
                        create_in(&self.store, connection, &fresh_id, &name, true, &home, now)
                            .await?;
                        account_in(connection, &fresh_id).await?.ok_or_else(|| {
                            BridgeError::new("account_store", "the owner vanished")
                        })?
                    }
                };
                insert_identity(connection, &owner, proven, self.counting, now).await?;
                return Ok(owner);
            }
            // The first account a verified email reached, in whichever tenant holds it.
            if let Some(email) = &email
                && self
                    .enter(connection, Lookup::Email(email))
                    .await?
                    .is_some()
            {
                let reached: Option<String> = sqlx::query(
                    "SELECT account FROM auth_identities WHERE email = $1
                     ORDER BY created_at, provider, subject LIMIT 1",
                )
                .bind(email)
                .fetch_optional(&mut *connection)
                .await?
                .map(|row| row.try_get(0))
                .transpose()?;
                if let Some(id) = reached
                    && let Some(account) = account_in(connection, &id).await?
                    && account.disabled_at.is_none()
                {
                    insert_identity(connection, &account, proven, self.counting, now).await?;
                    return Ok(account);
                }
            }
            if let Some(current) = current
                && self
                    .enter(connection, Lookup::Account(current))
                    .await?
                    .is_some()
                && let Some(account) = account_in(connection, current).await?
                && account.provisional
                && account.disabled_at.is_none()
                && !self.lapsed(&account, now)
            {
                insert_identity(connection, &account, proven, self.counting, now).await?;
                return Ok(Account {
                    provisional: false,
                    ..account
                });
            }
            if !multi {
                return Err(BridgeError::new(
                    "no_account",
                    "this hub has no account for that sign-in; ask its owner",
                )
                .into());
            }
            sqlx::query("INSERT INTO tenancy_tenants (id, created_at) VALUES ($1, $2)")
                .bind(&fresh_tenant)
                .bind(secs(now))
                .execute(&mut *connection)
                .await?;
            self.store.scope(connection, &fresh_tenant).await?;
            tenants::record_in(
                connection,
                &fresh_tenant,
                "hub",
                "tenant.created",
                &proven.provider,
                now,
            )
            .await?;
            super::access::begin_in(connection, &fresh_tenant, &self.site(), self.counting, now)
                .await?;
            create_in(
                &self.store,
                connection,
                &fresh_id,
                &name,
                false,
                &fresh_tenant,
                now,
            )
            .await?;
            let account = account_in(connection, &fresh_id)
                .await?
                .ok_or_else(|| BridgeError::new("account_store", "the account vanished"))?;
            insert_identity(connection, &account, proven, self.counting, now).await?;
            // Made by a sign-in, not an AI client, and secured by it from the start.
            if self.counting {
                funnel::note_in(connection, &fresh_tenant, funnel::MADE, "other", now).await?;
                let how = &proven.provider;
                funnel::note_in(connection, &fresh_tenant, funnel::SECURED, how, now).await?;
            }
            Ok(account)
        })
    }

    /// Link a proven sign-in to `account` (the one a browser is signed in to). Secures an
    /// account made on first connect.
    pub async fn link_identity(&self, account: &str, proven: &Proven) -> BridgeResult<Account> {
        proven.check()?;
        let operator = self.is_operator_identity(proven);
        let now = now_secs();
        transaction!(&self.store, |connection| {
            // Linked already, in any tenant? Then this transaction is scoped to the account's.
            let linked = linked_account(self, connection, proven).await?;
            self.enter(connection, Lookup::Account(account)).await?;
            let holder = account_in(connection, account)
                .await?
                .ok_or_else(|| BridgeError::new("no_account", format!("no account {account}")))?;
            if holder.disabled_at.is_some() {
                return Err(account_disabled(account).into());
            }
            if let Some(linked) = linked {
                return if linked.id == holder.id {
                    Ok(holder)
                } else {
                    Err(BridgeError::new(
                        "identity_taken",
                        "that sign-in already reaches another account here",
                    )
                    .into())
                };
            }
            if operator && !holder.owner {
                return Err(BridgeError::new(
                    "operator_identity",
                    "that sign-in is this hub's operator's; sign out and sign in with it instead",
                )
                .into());
            }
            insert_identity(connection, &holder, proven, self.counting, now).await?;
            Ok(Account {
                provisional: false,
                ..holder
            })
        })
    }

    /// The sign-ins linked to `account`.
    pub async fn identities_of(&self, account: &str) -> BridgeResult<Vec<Linked>> {
        transaction!(&self.store, |connection| {
            self.enter(connection, Lookup::Account(account)).await?;
            Ok(sqlx::query(
                "SELECT provider, email FROM auth_identities WHERE account = $1 ORDER BY created_at",
            )
            .bind(account)
            .fetch_all(&mut *connection)
            .await
            .and_then(|rows| {
                rows.iter()
                    .map(|row| {
                        Ok(Linked {
                            provider: row.try_get(0)?,
                            email: row.try_get(1)?,
                        })
                    })
                    .collect::<Result<Vec<_>, sqlx::Error>>()
            })?)
        })
    }
}
