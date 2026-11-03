//! The one task that writes what is counted: batches of visits and milestones off the
//! queue, each batch one transaction, and every hour the daily totals of each finished day
//! and the deletion of raw visits past `analytics.retention_days`. Nothing here is on a
//! request's path: a failed write is said in the log and the hub goes on.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use devagotchi_contracts::BridgeResult;
use sqlx::{AnyConnection, Row};
use tokio::sync::mpsc;

use super::{DAY, Owner, Record, day_of, funnel};
use crate::{
    devices::now_secs,
    store::{Failed, Lookup, Store, transaction, unsecs},
};

/// The most records one transaction writes.
const BATCH: usize = 500;
/// How long a batch waits for more before it is written.
const GATHER: Duration = Duration::from_secs(1);
/// How often the daily totals and retention run (and once at start).
const MAINTAIN_EVERY: Duration = Duration::from_secs(3600);
/// How long after midnight UTC a day counts as finished: what was queued in its last
/// moments is written by then.
const SETTLE_SECS: u64 = 600;
/// The most days one maintenance pass rolls up.
const MOST_DAYS: i64 = 4000;

pub(super) async fn run(
    store: Store,
    mut received: mpsc::Receiver<Record>,
    retention_days: u32,
    dropped: Arc<AtomicU64>,
) {
    let mut maintain_tick = tokio::time::interval(MAINTAIN_EVERY);
    maintain_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut said_dropped = 0u64;
    loop {
        tokio::select! {
            first = received.recv() => {
                let Some(first) = first else { break };
                let mut batch = vec![first];
                let until = tokio::time::Instant::now() + GATHER;
                while batch.len() < BATCH && !flushing(&batch) {
                    match tokio::time::timeout_at(until, received.recv()).await {
                        Ok(Some(record)) => batch.push(record),
                        _ => break,
                    }
                }
                if let Err(error) = write(&store, &batch).await {
                    eprintln!(
                        "devagotchi hub: analytics: {} records not written: {}",
                        batch.len(),
                        error.message
                    );
                }
                answer(batch);
            }
            _ = maintain_tick.tick() => {
                if let Err(error) = maintain(&store, retention_days, now_secs()).await {
                    eprintln!("devagotchi hub: analytics: daily totals: {}", error.message);
                }
            }
        }
        let now_dropped = dropped.load(Ordering::Relaxed);
        if now_dropped > said_dropped {
            eprintln!(
                "devagotchi hub: analytics: {} records dropped since start (the queue was full)",
                now_dropped
            );
            said_dropped = now_dropped;
        }
    }
}

#[cfg(test)]
fn flushing(batch: &[Record]) -> bool {
    matches!(batch.last(), Some(Record::Flush(_)))
}

#[cfg(not(test))]
fn flushing(_: &[Record]) -> bool {
    false
}

#[cfg(test)]
fn answer(batch: Vec<Record>) {
    for record in batch {
        if let Record::Flush(done) = record {
            let _ = done.send(());
        }
    }
}

#[cfg(not(test))]
fn answer(_: Vec<Record>) {}

/// Write one batch in one transaction.
async fn write(store: &Store, batch: &[Record]) -> BridgeResult<()> {
    transaction!(store, |connection| {
        for record in batch {
            match record {
                Record::View(view) => {
                    sqlx::query(
                        "INSERT INTO hub_analytics_views
                           (day, hour, path, utm_source, utm_medium, utm_campaign, referrer, device, visitor)
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                    )
                    .bind(view.day)
                    .bind(view.hour)
                    .bind(view.path)
                    .bind(&view.utm[0])
                    .bind(&view.utm[1])
                    .bind(&view.utm[2])
                    .bind(&view.referrer)
                    .bind(view.device)
                    .bind(&view.visitor)
                    .execute(&mut *connection)
                    .await?;
                }
                Record::Event { owner, kind, at } => {
                    let tenant = match owner {
                        Owner::Tenant(tenant) => Some(tenant.clone()),
                        Owner::Account(account) => {
                            store
                                .tenant_of(connection, Lookup::Account(account))
                                .await?
                        }
                    };
                    if let Some(tenant) = tenant {
                        funnel::note_in(connection, &tenant, kind, "", *at).await?;
                    }
                }
                #[cfg(test)]
                Record::Flush(_) => {}
            }
        }
        Ok::<(), Failed>(())
    })
}

/// One day's totals, as `(metric, item, views, visitors)`: from the raw visits of `day` and
/// the milestones that fell on it. What the daily table keeps, and what the operator page
/// shows for today.
pub async fn totals_in(
    connection: &mut AnyConnection,
    day: i64,
) -> Result<Vec<(String, String, i64, i64)>, sqlx::Error> {
    const TOTALS: [&str; 8] = [
        "SELECT 'total', '', COUNT(*), COUNT(DISTINCT visitor)
         FROM hub_analytics_views WHERE day = $1",
        "SELECT 'path', path, COUNT(*), COUNT(DISTINCT visitor)
         FROM hub_analytics_views WHERE day = $1 GROUP BY path",
        "SELECT 'referrer', referrer, COUNT(*), COUNT(DISTINCT visitor)
         FROM hub_analytics_views WHERE day = $1 AND referrer <> '' GROUP BY referrer",
        "SELECT 'campaign', utm_source || '|' || utm_medium || '|' || utm_campaign, COUNT(*),
           COUNT(DISTINCT visitor)
         FROM hub_analytics_views
         WHERE day = $1 AND (utm_source <> '' OR utm_medium <> '' OR utm_campaign <> '')
         GROUP BY utm_source, utm_medium, utm_campaign",
        "SELECT 'device', device, COUNT(*), COUNT(DISTINCT visitor)
         FROM hub_analytics_views WHERE day = $1 GROUP BY device",
        "SELECT 'event', kind, COUNT(*), CAST(0 AS BIGINT)
         FROM hub_analytics_events WHERE at >= $1 AND at < $1 + 86400 GROUP BY kind",
        "SELECT 'client', detail, COUNT(*), CAST(0 AS BIGINT)
         FROM hub_analytics_events WHERE kind = 'made' AND at >= $1 AND at < $1 + 86400
         GROUP BY detail",
        "SELECT 'sign_in', detail, COUNT(*), CAST(0 AS BIGINT)
         FROM hub_analytics_events WHERE kind = 'secured' AND at >= $1 AND at < $1 + 86400
         GROUP BY detail",
    ];
    let mut out = Vec::new();
    for sql in TOTALS {
        for row in sqlx::query(sql)
            .bind(day)
            .fetch_all(&mut *connection)
            .await?
        {
            out.push((
                row.try_get::<String, _>(0)?,
                row.try_get::<String, _>(1)?,
                row.try_get::<i64, _>(2)?,
                row.try_get::<i64, _>(3)?,
            ));
        }
    }
    Ok(out)
}

/// Roll each finished day not yet rolled up into `hub_analytics_daily`, then delete raw
/// visits older than `retention_days` (only days already rolled up). Safe to run again,
/// and from several hub instances: a day's totals are replaced whole.
pub async fn maintain(store: &Store, retention_days: u32, now: u64) -> BridgeResult<()> {
    // The last day surely finished: yesterday, once today is SETTLE_SECS old.
    let last = day_of(now.saturating_sub(SETTLE_SECS)) - DAY;
    let (rolled, first) = transaction!(store, |connection| {
        let rolled: Option<i64> =
            sqlx::query_scalar("SELECT MAX(day) FROM hub_analytics_daily WHERE metric = 'total'")
                .fetch_one(&mut *connection)
                .await?;
        let first_view: Option<i64> =
            sqlx::query_scalar("SELECT MIN(day) FROM hub_analytics_views")
                .fetch_one(&mut *connection)
                .await?;
        let first_event: Option<i64> =
            sqlx::query_scalar("SELECT MIN(at) FROM hub_analytics_events")
                .fetch_one(&mut *connection)
                .await?;
        let first = [first_view, first_event.map(|at| day_of(unsecs(at)))]
            .into_iter()
            .flatten()
            .min();
        Ok::<_, Failed>((rolled, first))
    })?;
    let from = match (rolled, first) {
        (Some(rolled), _) => Some(rolled + DAY),
        (None, first) => first,
    };
    if let Some(from) = from {
        let mut day = from.max(last - MOST_DAYS * DAY);
        while day <= last {
            transaction!(store, |connection| {
                let totals = totals_in(connection, day).await?;
                sqlx::query("DELETE FROM hub_analytics_daily WHERE day = $1")
                    .bind(day)
                    .execute(&mut *connection)
                    .await?;
                for (metric, item, views, visitors) in &totals {
                    sqlx::query(
                        "INSERT INTO hub_analytics_daily (day, metric, item, views, visitors)
                         VALUES ($1, $2, $3, $4, $5)",
                    )
                    .bind(day)
                    .bind(metric)
                    .bind(item)
                    .bind(views)
                    .bind(visitors)
                    .execute(&mut *connection)
                    .await?;
                }
                Ok::<(), Failed>(())
            })?;
            day += DAY;
        }
    }
    let keep_from = day_of(now) - i64::from(retention_days) * DAY;
    transaction!(store, |connection| {
        sqlx::query(
            "DELETE FROM hub_analytics_views WHERE day < $1 AND day <= (
               SELECT COALESCE(MAX(day), -1) FROM hub_analytics_daily WHERE metric = 'total')",
        )
        .bind(keep_from)
        .execute(&mut *connection)
        .await?;
        Ok::<(), Failed>(())
    })
}
