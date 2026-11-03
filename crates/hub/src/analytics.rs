//! First-party analytics for a hosted hub's operator (increment 66): the hub counts what it
//! serves itself, with no script in the page, no cookie, no beacon and nobody else's code.
//! Only a hosted hub (`tenancy.mode multi`) with `analytics.enabled` counts anything
//! (`HubConfig::counts_visits`); its operator page is the one place the numbers are read.
//!
//! - **Visits** (`visit.rs`): each `GET` of a public page that the hub answered `200`, as
//!   the web router serves it (`web/counted.rs`). Kept: the UTC day and hour, the path, the
//!   link's `utm_source`, `utm_medium` and `utm_campaign`, the referring site's host (never
//!   this hub's own), the device class, and the visitor: an HMAC of the client's address
//!   (`tenancy.trusted_proxies` decides which) and User-Agent under a key that lives only in
//!   this process's memory and is replaced, the old one dropped, when the UTC day changes.
//!   So a visitor counts once a day and cannot be followed from one day to the next; no
//!   address and no User-Agent is ever written. Not counted: a request with `Sec-GPC: 1`
//!   or `DNT: 1`, one with no User-Agent or a robot's (`bots.txt`), the signed-in
//!   operator's, and anything that is not a public page (assets, `/healthz`, the APIs).
//! - **Milestones** (`funnel.rs`): each tenant's, once, where it happens. Made (with the
//!   AI client family), secured (with the sign-in), waiting and admitted are written in the
//!   account store's own transaction that does them (`accounts/`); the first machine
//!   (`link.rs`), the first operation (`hub.rs`, with the latest kept a day at a time) and
//!   the first board (`board/mcp.rs`) through the queue below.
//! - **Writes never slow or fail a page**: the queue is bounded, a full one drops the
//!   record and counts it (`dropped`, said in the log), and one task writes what arrives in
//!   batches (`writer.rs`). It also rolls each finished day into daily totals
//!   (`hub_analytics_daily`, kept) and deletes raw visits older than
//!   `analytics.retention_days`.
//!
//! The tables are migration 0009's (`hub_analytics_*`): hub-scoped, like `hub_secrets`.

pub mod funnel;
pub mod report;
pub mod visit;
mod writer;

#[cfg(test)]
mod tests;

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use aws_lc_rs::hmac;
use tokio::sync::mpsc;

use crate::{config::AnalyticsConfig, store::Store};

pub use visit::View;

/// Seconds in a day: days are kept as Unix seconds at 00:00 UTC.
pub const DAY: i64 = 86_400;
/// Records waiting to be written; more are dropped and counted.
const QUEUE: usize = 10_000;
/// Distinct milestones remembered as sent, before the memory is cleared.
const REMEMBERED: usize = 100_000;

/// The UTC day `secs` falls in, as Unix seconds at its midnight.
pub fn day_of(secs: u64) -> i64 {
    let secs = crate::store::secs(secs);
    secs - secs.rem_euclid(DAY)
}

/// Put on a request by the web bar when the signed-in operator made it: not counted.
#[derive(Debug, Clone, Copy)]
pub struct OperatorVisit;

/// Whose milestone it is: a tenant, or an account (its tenant is looked up when written).
#[derive(Debug, Clone)]
pub enum Owner {
    Tenant(String),
    Account(String),
}

enum Record {
    View(View),
    Event {
        owner: Owner,
        kind: &'static str,
        at: u64,
    },
    /// Answered once everything queued before it is written.
    #[cfg(test)]
    Flush(tokio::sync::oneshot::Sender<()>),
}

/// The day's key for visitor ids, in memory only.
struct Salt {
    day: i64,
    key: hmac::Key,
}

pub struct Analytics {
    queue: mpsc::Sender<Record>,
    dropped: Arc<AtomicU64>,
    salt: Mutex<Option<Salt>>,
    /// Milestones already queued, and the day they were: each is sent once (an operation
    /// once a day, for its latest).
    sent: Mutex<HashMap<(String, &'static str), i64>>,
    retention_days: u32,
    /// This hub's own host, lowercase, never counted as a referrer.
    own_host: String,
}

impl std::fmt::Debug for Analytics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Analytics(retention {} days)", self.retention_days)
    }
}

impl Analytics {
    /// Count for the hub at `public_url` into `store`: the writer runs on this runtime
    /// until the last handle is dropped.
    pub fn start(store: Store, config: AnalyticsConfig, public_url: &str) -> Arc<Self> {
        let (queue, received) = mpsc::channel(QUEUE);
        let dropped = Arc::new(AtomicU64::new(0));
        tokio::spawn(writer::run(
            store,
            received,
            config.retention_days,
            dropped.clone(),
        ));
        Arc::new(Self {
            queue,
            dropped,
            salt: Mutex::new(None),
            sent: Mutex::new(HashMap::new()),
            retention_days: config.retention_days,
            own_host: visit::host_of(public_url).unwrap_or_default(),
        })
    }

    /// How many records were dropped because the queue was full, since the hub started.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Days raw visits are kept (`analytics.retention_days`).
    pub fn retention_days(&self) -> u32 {
        self.retention_days
    }

    pub(crate) fn own_host(&self) -> &str {
        &self.own_host
    }

    fn queue(&self, record: Record) {
        if let Err(mpsc::error::TrySendError::Full(_)) = self.queue.try_send(record) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Count a visit (`visit::seen` made it).
    pub fn page_view(&self, view: View) {
        self.queue(Record::View(view));
    }

    /// The visitor id of `address` and `agent` on `day`: an HMAC under that day's key, made
    /// when the day is first seen and dropped when another day's is made.
    pub fn visitor(&self, day: i64, address: IpAddr, agent: &str) -> String {
        let address = match address {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(address),
            v4 => v4,
        };
        let mut salt = self.salt.lock().expect("analytics salt");
        if salt.as_ref().is_none_or(|salt| salt.day != day) {
            let mut bytes = crate::link::random::<32>();
            let key = hmac::Key::new(hmac::HMAC_SHA256, &bytes);
            bytes.fill(0);
            std::hint::black_box(&bytes);
            // The day before's key goes here, with every id it could make.
            *salt = Some(Salt { day, key });
        }
        let key = &salt.as_ref().expect("a key for the day").key;
        let tag = hmac::sign(key, format!("{address}\0{agent}").as_bytes());
        hex::encode(&tag.as_ref()[..16])
    }

    /// A tenant's milestone `kind` (`funnel.rs`), now.
    pub fn tenant_event(&self, tenant: &str, kind: &'static str) {
        self.event(
            Owner::Tenant(tenant.to_string()),
            kind,
            crate::devices::now_secs(),
        );
    }

    /// An account's milestone `kind`, now: written for the account's tenant.
    pub fn account_event(&self, account: &str, kind: &'static str) {
        self.event(
            Owner::Account(account.to_string()),
            kind,
            crate::devices::now_secs(),
        );
    }

    pub(crate) fn event(&self, owner: Owner, kind: &'static str, at: u64) {
        let id = match &owner {
            Owner::Tenant(id) | Owner::Account(id) => id.clone(),
        };
        if id.is_empty() {
            return;
        }
        // Once each, or (an operation) once a day: the store keeps the first and the latest.
        let day = if kind == funnel::OPERATION {
            day_of(at)
        } else {
            0
        };
        {
            let mut sent = self.sent.lock().expect("analytics sent");
            if sent.get(&(id.clone(), kind)) == Some(&day) {
                return;
            }
            if sent.len() >= REMEMBERED {
                sent.clear();
            }
            sent.insert((id, kind), day);
        }
        self.queue(Record::Event { owner, kind, at });
    }

    /// Wait until everything queued so far is written.
    #[cfg(test)]
    pub async fn flush(&self) {
        let (done, wait) = tokio::sync::oneshot::channel();
        self.queue
            .send(Record::Flush(done))
            .await
            .expect("the analytics writer");
        wait.await.expect("the analytics writer");
    }
}
