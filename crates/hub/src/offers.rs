//! The site's offers (increment 73): the network a site belongs to and its free offerings
//! (`site.json` `offers`, `site/offers.rs`), offered by the AI once, in the chat, and taken
//! up with one word.
//!
//! - **The AI offers once, never pushes.** `offers` gives it the offerings and the one
//!   question to ask, as the site words it; its description and the MCP instructions say to
//!   ask once, after the person's first machine works (`mcp.rs`).
//! - **Yes is the person's own words, kept.** `offers_accept {consent}` takes what they
//!   said, exactly, and keeps it with the time on the tenant's chain (`offers.accepted`)
//!   and in its row (`accounts/offers.rs`, migration 0012), in one transaction.
//! - **Then the account is linked, with no click where the hub can.** With
//!   `shippin.provisioning` (`config/shippin.rs`) and an account whose email the hub has
//!   verified, the hub finds that person on Nomen, or makes them, and links the account to
//!   them at once (`nomen.rs`). Otherwise the AI hands over one link, `/offers/<token>`
//!   (`web/offers.rs`): one tap signs in with the network's sign-in (`shippin.provider`)
//!   and links it the way the account page does; `offers` then says it is linked.
//! - **Off is gone.** With the site's offers off (or none listed), or on a hub with no
//!   accounts of its own, the tools are not listed or served, the landing section and its
//!   words are not drawn, and the instructions say nothing of it.

pub(crate) mod mcp;
pub mod nomen;

use std::sync::Arc;

use devagotchi_contracts::{BridgeError, BridgeResult};

use crate::{accounts::Accounts, accounts::offers::OfferRow, config::HubConfig, hub::Hub};

/// The tools that exist only while the site's offers show.
pub const TOOLS: [&str; 2] = ["offers", "offers_accept"];

/// The offers of a hub with its own accounts: its store, how it links, and for how long a
/// one-tap link works.
pub struct Offers {
    accounts: Arc<Accounts>,
    /// The sign-in whose accounts are the network's (`shippin.provider`).
    provider: String,
    /// Whether this hub signs in with it: the one-tap link needs that.
    can_link: bool,
    nomen: Option<nomen::Nomen>,
    link_ttl: u64,
}

impl Offers {
    /// From the config: a provisioning token that is missing stops the start.
    pub fn from_config(config: &HubConfig, accounts: Arc<Accounts>) -> Result<Self, String> {
        let provider = config.shippin.provider.clone();
        let can_link = config
            .sign_ins()
            .0
            .iter()
            .any(|sign_in| sign_in.name() == provider);
        let nomen = config
            .shippin
            .provisioning
            .as_ref()
            .map(nomen::Nomen::from_config)
            .transpose()?;
        Ok(Self {
            accounts,
            provider,
            can_link,
            nomen,
            link_ttl: config.workflows.link_ttl_secs,
        })
    }

    pub fn accounts(&self) -> &Arc<Accounts> {
        &self.accounts
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn can_link(&self) -> bool {
        self.can_link
    }

    /// Whether `account` signs in with the network's sign-in: it is linked.
    pub async fn has_sign_in(&self, account: &str) -> BridgeResult<bool> {
        Ok(self
            .accounts
            .identities_of(account)
            .await?
            .iter()
            .any(|linked| linked.provider == self.provider))
    }

    /// The email the hub verified for `account`, from a sign-in that proved it.
    pub async fn verified_email(&self, account: &str) -> BridgeResult<Option<String>> {
        Ok(self
            .accounts
            .identities_of(account)
            .await?
            .into_iter()
            .find_map(|linked| linked.email))
    }

    /// `row` as it stands now: a pending one whose account signs in with the network now is
    /// linked (`sign_in`), once, on the chain.
    pub async fn settle(&self, row: OfferRow, now: u64) -> BridgeResult<OfferRow> {
        if row.linked() || !self.has_sign_in(&row.account).await? {
            return Ok(row);
        }
        self.accounts
            .offers_linked(&row.tenant, &row.account, "sign_in", now)
            .await?;
        Ok(self.accounts.offer(&row.tenant).await?.unwrap_or(row))
    }

    pub fn link_ttl(&self) -> u64 {
        self.link_ttl
    }
}

impl Hub {
    /// Whether the offers tools are there: the site's offers show, on a hub with accounts.
    pub(crate) fn offered(&self) -> bool {
        self.offers.is_some() && self.config.site().info.offers().is_some()
    }

    pub(crate) fn offers_engine(&self) -> Result<&Arc<Offers>, BridgeError> {
        match &self.offers {
            Some(offers) if self.config.site().info.offers().is_some() => Ok(offers),
            _ => Err(BridgeError::new(
                "offers_unavailable",
                "this hub offers nothing to link an account to",
            )),
        }
    }
}
