//! Sign-in links sent by email. A link proves the mailbox only because it is delivered out
//! of band: the hub's answer never contains it (`web/email.rs` mails it). Each is 256
//! random bits, only its SHA-256 kept with the address it went to, spent once, good for
//! 15 minutes. One sent to add the address to an account (securing one made on first
//! connect) names that account and works only in a browser signed in to it.
//!
//! Limits: `auth.email.per_address_per_hour` links to one address, and twenty per client
//! address an hour (kept as an HMAC of the address, never the address).

use std::net::IpAddr;

use super::*;

/// How long an email link works.
pub const EMAIL_LINK_SECS: u64 = 15 * 60;
/// Links one client address may ask for in an hour.
const PER_SOURCE_PER_HOUR: i64 = 20;

/// A link's address, and the account it adds the address to, if it was sent for that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailLink {
    pub email: String,
    pub account: Option<String>,
}

/// An address as the hub keeps it: trimmed, in lowercase, plausible; `None` otherwise.
pub fn normalize_email(typed: &str) -> Option<String> {
    let email = typed.trim().to_ascii_lowercase();
    crate::mail::address_of(&email).filter(|address| *address == email)
}

impl Accounts {
    /// A new sign-in link token for `email`, or for adding it to `account`. `per_address`
    /// is the config's links per address per hour.
    pub async fn issue_email_link(
        &self,
        email: &str,
        account: Option<&str>,
        source: IpAddr,
        per_address: u32,
    ) -> BridgeResult<String> {
        self.issue_email_link_at(
            email,
            account,
            source,
            per_address,
            crate::link::random::<32>(),
            now_secs(),
        )
        .await
    }

    pub(crate) async fn issue_email_link_at(
        &self,
        email: &str,
        account: Option<&str>,
        source: IpAddr,
        per_address: u32,
        random: [u8; 32],
        now: u64,
    ) -> BridgeResult<String> {
        let email = normalize_email(email)
            .ok_or_else(|| BridgeError::new("invalid_email", "that is not an email address"))?;
        let token = format!("dge_{}", URL_SAFE_NO_PAD.encode(random));
        let source = hex::encode(&self.mac("email source", source.to_string().as_bytes())[..16]);
        let hour_ago = secs(now) - 3600;
        transaction!(&self.store, |connection| {
            sqlx::query("DELETE FROM email_sends WHERE at <= $1")
                .bind(hour_ago)
                .execute(&mut *connection)
                .await?;
            sqlx::query("DELETE FROM email_links WHERE expires_at <= $1 AND created_at <= $2")
                .bind(secs(now))
                .bind(hour_ago)
                .execute(&mut *connection)
                .await?;
            let from_source: i64 =
                sqlx::query("SELECT COUNT(*) FROM email_sends WHERE source = $1")
                    .bind(&source)
                    .fetch_one(&mut *connection)
                    .await?
                    .try_get(0)?;
            let to_address: i64 = sqlx::query(
                "SELECT COUNT(*) FROM email_links WHERE email = $1 AND created_at > $2",
            )
            .bind(&email)
            .bind(hour_ago)
            .fetch_one(&mut *connection)
            .await?
            .try_get(0)?;
            if from_source >= PER_SOURCE_PER_HOUR || to_address >= i64::from(per_address) {
                return Err(BridgeError::new(
                    "rate_limited",
                    "too many sign-in links were sent just now; try again in an hour",
                )
                .into());
            }
            sqlx::query("INSERT INTO email_sends (source, at) VALUES ($1, $2)")
                .bind(&source)
                .bind(secs(now))
                .execute(&mut *connection)
                .await?;
            sqlx::query(
                "INSERT INTO email_links (hash, email, account, created_at, expires_at)
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(hash(&token))
            .bind(&email)
            .bind(account)
            .bind(secs(now))
            .bind(secs(now + EMAIL_LINK_SECS))
            .execute(&mut *connection)
            .await?;
            Ok(())
        })?;
        Ok(token)
    }

    /// Spend a link: what it was for, once, while it is live; `invalid_link` otherwise.
    pub async fn spend_email_link(&self, token: &str) -> BridgeResult<EmailLink> {
        self.spend_email_link_at(token, now_secs()).await
    }

    pub(crate) async fn spend_email_link_at(
        &self,
        token: &str,
        now: u64,
    ) -> BridgeResult<EmailLink> {
        let invalid = || BridgeError::new("invalid_link", "this link is used, expired or unknown");
        if !token.starts_with("dge_") || token.len() > 128 {
            return Err(invalid());
        }
        transaction!(&self.store, |connection| {
            let spent = sqlx::query(
                "UPDATE email_links SET used_at = $1
                 WHERE hash = $2 AND used_at IS NULL AND expires_at > $1",
            )
            .bind(secs(now))
            .bind(hash(token))
            .execute(&mut *connection)
            .await?
            .rows_affected();
            if spent != 1 {
                return Err(invalid().into());
            }
            let row = sqlx::query("SELECT email, account FROM email_links WHERE hash = $1")
                .bind(hash(token))
                .fetch_one(&mut *connection)
                .await?;
            Ok(EmailLink {
                email: row.try_get(0)?,
                account: row.try_get(1)?,
            })
        })
    }
}
