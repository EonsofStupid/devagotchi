//! The site's offers in the store (migration 0012): one row per tenant whose person said yes,
//! with their words as they said them, the offerings shown then, and whether the account is
//! linked to its account on the network yet. Saying yes and being linked are each on the
//! tenant's chain, in the transaction that records them. A one-tap link finds its tenant
//! only through `Lookup::Offer` (`devagotchi.offer_tenant` on Postgres). The engine is
//! `crate::offers`.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

use super::*;

/// The longest consent kept: the person's own words, a sentence or two.
pub const CONSENT_MAX: usize = 1000;

/// What a tenant said yes to, and where linking stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferRow {
    pub tenant: String,
    /// The account whose AI asked.
    pub account: String,
    /// The person's words, exactly as passed.
    pub consent: String,
    pub offerings: Vec<String>,
    pub accepted_at: u64,
    /// `pending` or `linked`.
    pub state: String,
    /// `provisioned` or `sign_in`, once linked.
    pub linked_via: Option<String>,
    pub linked_at: Option<u64>,
    /// When the one-tap link stops working, while there is one.
    pub link_expires_at: Option<u64>,
}

impl OfferRow {
    pub fn linked(&self) -> bool {
        self.state == "linked"
    }

    /// Whether its one-tap link may still link the account at `now`.
    pub fn live(&self, now: u64) -> bool {
        !self.linked() && self.link_expires_at.is_some_and(|until| now < until)
    }
}

/// A one-tap link's token: `dgo_` and 256 random bits, base64url. Only its SHA-256 is kept.
pub fn new_offer_token(random: [u8; 32]) -> String {
    format!("dgo_{}", URL_SAFE_NO_PAD.encode(random))
}

/// A token's shape, checked before anything is looked up.
pub fn valid_offer_token(token: &str) -> bool {
    token.strip_prefix("dgo_").is_some_and(|rest| {
        rest.len() == 43
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
}

/// The person's words as they may be kept: not empty, not too long, no control characters
/// but line breaks and tabs. Never changed: refused instead.
pub fn check_consent(consent: &str) -> BridgeResult<()> {
    let bad = |message: &str| Err(BridgeError::new("invalid_consent", message));
    if consent.trim().is_empty() {
        return bad("consent is the user's own words saying yes; it is empty");
    }
    if consent.len() > CONSENT_MAX {
        return bad("consent is at most 1000 bytes: the user's own words, as they said them");
    }
    if consent
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return bad("consent holds control characters; pass the user's words as they said them");
    }
    Ok(())
}

macro_rules! offer_columns {
    () => {
        "tenant_id, account, consent, offerings, accepted_at, link_state, linked_via, linked_at, \
         link_expires_at"
    };
}

fn offer_row(row: &AnyRow) -> Result<OfferRow, sqlx::Error> {
    let offerings: String = row.try_get(3)?;
    Ok(OfferRow {
        tenant: row.try_get(0)?,
        account: row.try_get(1)?,
        consent: row.try_get(2)?,
        offerings: serde_json::from_str(&offerings).unwrap_or_default(),
        accepted_at: unsecs(row.try_get(4)?),
        state: row.try_get(5)?,
        linked_via: row.try_get(6)?,
        linked_at: row.try_get::<Option<i64>, _>(7)?.map(unsecs),
        link_expires_at: row.try_get::<Option<i64>, _>(8)?.map(unsecs),
    })
}

async fn offer_in(
    connection: &mut AnyConnection,
    tenant: &str,
) -> Result<Option<OfferRow>, sqlx::Error> {
    sqlx::query(concat!(
        "SELECT ",
        offer_columns!(),
        " FROM tenancy_offers WHERE tenant_id = $1"
    ))
    .bind(tenant)
    .fetch_optional(&mut *connection)
    .await?
    .as_ref()
    .map(offer_row)
    .transpose()
}

/// Mark `tenant`'s offer linked, once, on its chain; inside the caller's transaction.
async fn linked_in(
    connection: &mut AnyConnection,
    tenant: &str,
    actor: &str,
    via: &str,
    now: u64,
) -> Result<bool, Failed> {
    let changed = sqlx::query(
        "UPDATE tenancy_offers SET link_state = 'linked', linked_via = $1, linked_at = $2
         WHERE tenant_id = $3 AND link_state = 'pending'",
    )
    .bind(via)
    .bind(secs(now))
    .bind(tenant)
    .execute(&mut *connection)
    .await?
    .rows_affected();
    if changed == 1 {
        tenants::record_in(connection, tenant, actor, "offers.linked", via, now).await?;
    }
    Ok(changed == 1)
}

impl Accounts {
    /// What `tenant` said yes to, if it did.
    pub async fn offer(&self, tenant: &str) -> BridgeResult<Option<OfferRow>> {
        transaction!(&self.store, |connection| {
            self.store.scope(connection, tenant).await?;
            Ok(offer_in(connection, tenant).await?)
        })
    }

    /// `account` of `tenant` said yes, in `consent` (their words, kept exactly), to
    /// `offerings`: kept, and on the tenant's chain with the time, in one transaction. A
    /// tenant already linked stays linked; one still pending takes these words, and its
    /// earlier one-tap link stops working.
    pub async fn accept_offers(
        &self,
        tenant: &str,
        account: &str,
        consent: &str,
        offerings: &[String],
        now: u64,
    ) -> BridgeResult<OfferRow> {
        check_consent(consent)?;
        let offerings = serde_json::to_string(offerings).unwrap_or_else(|_| "[]".into());
        transaction!(&self.store, |connection| {
            self.store.scope(connection, tenant).await?;
            let holder = account_in(connection, account)
                .await?
                .filter(|holder| holder.tenant == tenant)
                .ok_or_else(|| BridgeError::new("no_account", format!("no account {account}")))?;
            if holder.disabled_at.is_some() {
                return Err(account_disabled(account).into());
            }
            sqlx::query(
                "INSERT INTO tenancy_offers (tenant_id, account, consent, offerings, accepted_at)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (tenant_id) DO UPDATE SET account = $2, consent = $3,
                   offerings = $4, accepted_at = $5, link_hash = NULL, link_expires_at = NULL
                 WHERE tenancy_offers.link_state = 'pending'",
            )
            .bind(tenant)
            .bind(account)
            .bind(consent)
            .bind(&offerings)
            .bind(secs(now))
            .execute(&mut *connection)
            .await?;
            tenants::record_in(connection, tenant, account, "offers.accepted", consent, now)
                .await?;
            offer_in(connection, tenant)
                .await?
                .ok_or_else(|| Failed::Store(sqlx::Error::RowNotFound))
        })
    }

    /// A one-tap link for `tenant`'s pending offer, working for `ttl` seconds; it replaces
    /// any earlier one. Returns the token.
    pub async fn offer_link(
        &self,
        tenant: &str,
        random: [u8; 32],
        ttl: u64,
        now: u64,
    ) -> BridgeResult<String> {
        let token = new_offer_token(random);
        let key = hash(&token);
        transaction!(&self.store, |connection| {
            self.store.scope(connection, tenant).await?;
            let set = sqlx::query(
                "UPDATE tenancy_offers SET link_hash = $1, link_expires_at = $2
                 WHERE tenant_id = $3 AND link_state = 'pending'",
            )
            .bind(&key)
            .bind(secs(now.saturating_add(ttl)))
            .bind(tenant)
            .execute(&mut *connection)
            .await?
            .rows_affected();
            if set != 1 {
                return Err(BridgeError::new("not_pending", "nothing waits to be linked").into());
            }
            Ok(token.clone())
        })
    }

    /// The offer a one-tap link names, whether or not it still works (`live`); `None` for
    /// no such link, or one a later yes replaced.
    pub async fn offer_by_link(&self, token: &str) -> BridgeResult<Option<OfferRow>> {
        if !valid_offer_token(token) {
            return Ok(None);
        }
        let key = hash(token);
        transaction!(&self.store, |connection| {
            let Some(tenant) = self.enter(connection, Lookup::Offer(&key)).await? else {
                return Ok(None);
            };
            Ok(offer_in(connection, &tenant).await?)
        })
    }

    /// `tenant`'s offer is linked now (`via`: `provisioned` or `sign_in`): kept, and on the
    /// chain, once. Returns whether this call did it.
    pub async fn offers_linked(
        &self,
        tenant: &str,
        actor: &str,
        via: &str,
        now: u64,
    ) -> BridgeResult<bool> {
        transaction!(&self.store, |connection| {
            self.store.scope(connection, tenant).await?;
            linked_in(connection, tenant, actor, via, now).await
        })
    }

    /// A session of the account of a live one-tap link, for an account nobody has secured
    /// yet (a secured one signs in instead), so its browser can go on to link the network's
    /// sign-in. On the chain. Returns the session's token.
    pub async fn offer_session(&self, token: &str, now: u64) -> BridgeResult<String> {
        let refused = |code: &str, message: &str| Failed::Refused(BridgeError::new(code, message));
        if !valid_offer_token(token) {
            return Err(BridgeError::new("expired", "the link no longer works"));
        }
        let key = hash(token);
        let random = crate::link::random::<32>();
        transaction!(&self.store, |connection| {
            let Some(tenant) = self.enter(connection, Lookup::Offer(&key)).await? else {
                return Err(refused("expired", "the link no longer works"));
            };
            let Some(row) = offer_in(connection, &tenant).await? else {
                return Err(refused("expired", "the link no longer works"));
            };
            if !row.live(now) {
                return Err(refused("expired", "the link no longer works"));
            }
            let holder = account_in(connection, &row.account)
                .await?
                .ok_or_else(|| BridgeError::new("no_account", "the offer's account is gone"))?;
            if holder.disabled_at.is_some() {
                return Err(account_disabled(&holder.id).into());
            }
            if !holder.provisional {
                return Err(refused("secured", "this account is secured: sign in to it"));
            }
            let session =
                sessions::insert_session(connection, &tenant, &holder.id, None, random, now)
                    .await?;
            tenants::record_in(
                connection,
                &tenant,
                &holder.id,
                "session.opened",
                "a browser to link the account through the offers link",
                now,
            )
            .await?;
            Ok(session)
        })
    }
}
