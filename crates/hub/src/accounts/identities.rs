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

async fn linked_account(
    connection: &mut AnyConnection,
    proven: &Proven,
) -> Result<Option<Account>, Failed> {
    let found: Option<String> =
        sqlx::query("SELECT account FROM identities WHERE provider = $1 AND subject = $2")
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
    now: u64,
) -> Result<(), Failed> {
    sqlx::query(
        "INSERT INTO identities (provider, subject, account, email, created_at)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(&proven.provider)
    .bind(&proven.subject)
    .bind(&account.id)
    .bind(proven.email())
    .bind(secs(now))
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
    tenants::secure_in(connection, account, &proven.provider, now).await?;
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
            crate::link::random::<16>(),
            crate::link::random::<16>(),
            now_secs(),
        )
        .await
    }

    pub(crate) async fn sign_in_identity_at(
        &self,
        proven: &Proven,
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
            if let Some(account) = linked_account(connection, proven).await? {
                if account.disabled_at.is_some() {
                    return Err(account_disabled(&account.id).into());
                }
                return Ok(account);
            }
            if operator {
                // The operator, with no claim: the owner, made in the home tenant if the
                // hub has none yet.
                let owner = match owner_in(connection).await? {
                    Some(owner) => owner,
                    None => {
                        let home = tenants::home_in(connection).await?;
                        create_in(connection, &fresh_id, &name, true, &home, now).await?;
                        account_in(connection, &fresh_id).await?.ok_or_else(|| {
                            BridgeError::new("account_store", "the owner vanished")
                        })?
                    }
                };
                insert_identity(connection, &owner, proven, now).await?;
                return Ok(owner);
            }
            if let Some(email) = &email {
                let reached: Option<String> = sqlx::query(
                    "SELECT account FROM identities WHERE email = $1 ORDER BY created_at LIMIT 1",
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
                    insert_identity(connection, &account, proven, now).await?;
                    return Ok(account);
                }
            }
            if !multi {
                return Err(BridgeError::new(
                    "no_account",
                    "this hub has no account for that sign-in; ask its owner",
                )
                .into());
            }
            sqlx::query("INSERT INTO tenants (id, created_at) VALUES ($1, $2)")
                .bind(&fresh_tenant)
                .bind(secs(now))
                .execute(&mut *connection)
                .await?;
            tenants::record_in(
                connection,
                &fresh_tenant,
                "hub",
                "tenant.created",
                &proven.provider,
                now,
            )
            .await?;
            create_in(connection, &fresh_id, &name, false, &fresh_tenant, now).await?;
            let account = account_in(connection, &fresh_id)
                .await?
                .ok_or_else(|| BridgeError::new("account_store", "the account vanished"))?;
            insert_identity(connection, &account, proven, now).await?;
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
            let holder = account_in(connection, account)
                .await?
                .ok_or_else(|| BridgeError::new("no_account", format!("no account {account}")))?;
            if holder.disabled_at.is_some() {
                return Err(account_disabled(account).into());
            }
            if let Some(linked) = linked_account(connection, proven).await? {
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
            insert_identity(connection, &holder, proven, now).await?;
            Ok(Account {
                provisional: false,
                ..holder
            })
        })
    }

    /// The sign-ins linked to `account`.
    pub async fn identities_of(&self, account: &str) -> BridgeResult<Vec<Linked>> {
        let mut connection = self.store.read().await?;
        sqlx::query("SELECT provider, email FROM identities WHERE account = $1 ORDER BY created_at")
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
                    .collect()
            })
            .map_err(store_error)
    }
}
