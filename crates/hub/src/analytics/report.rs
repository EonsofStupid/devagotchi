//! What the operator's analytics page shows (`web/analytics.rs`), read from the store: each
//! day's totals over a range (the daily table for finished days, the raw visits for days
//! not rolled up yet, today among them), the funnel, the accounts' AI apps and sign-ins,
//! who waits now and who is active. Never anything of one visitor.

use std::collections::BTreeMap;

use devagotchi_contracts::BridgeResult;
use sqlx::{AnyConnection, Row};

use super::{DAY, day_of, writer::totals_in};
use crate::store::{Failed, Store, transaction};

/// The ranges the page offers, in days.
pub const RANGES: [i64; 3] = [7, 30, 90];
/// The most rows a top list shows.
const TOP: usize = 10;

/// One day's totals: `(day, metric, item, views, visitors)` (`writer::totals_in`).
pub type DailyRow = (i64, String, String, i64, i64);

/// A thing counted over the range: its views, and its visitors summed day by day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Counted {
    pub item: String,
    pub views: i64,
    pub visitors: i64,
}

/// The funnel over a range: visitors in it, the accounts made in it, and how many of those
/// got to each later step so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Funnel {
    pub visitors: i64,
    pub made: i64,
    pub secured: i64,
    pub admitted: i64,
    pub machine: i64,
    pub operation: i64,
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    /// Each day of the range, oldest first: `(day, views, visitors)`.
    pub days: Vec<(i64, i64, i64)>,
    pub pages: Vec<Counted>,
    pub referrers: Vec<Counted>,
    /// `source|medium|campaign`.
    pub campaigns: Vec<Counted>,
    pub devices: Vec<Counted>,
    pub funnel: Funnel,
    /// The accounts made in the range by AI client family, and secured by sign-in.
    pub clients: Vec<(String, i64)>,
    pub sign_ins: Vec<(String, i64)>,
    /// Tenants on the early-access list now.
    pub waiting: i64,
    /// Tenants that ran an operation in the last 7 and the last 30 days.
    pub active_week: i64,
    pub active_month: i64,
}

/// The first day of the `days`-day range that ends today.
pub fn start_of(days: i64, now: u64) -> i64 {
    day_of(now) - (days - 1) * DAY
}

/// Every day's totals from `from` through today: the daily table's, and for days it does
/// not hold yet (the ones after the last rolled up: today, and yesterday until it settles)
/// the raw visits' and milestones', as the writer would roll them.
pub async fn daily_in(
    connection: &mut AnyConnection,
    from: i64,
    now: u64,
) -> Result<Vec<DailyRow>, sqlx::Error> {
    let today = day_of(now);
    let mut rows: Vec<DailyRow> = sqlx::query(
        "SELECT day, metric, item, views, visitors FROM hub_analytics_daily
         WHERE day >= $1 ORDER BY day, metric, item",
    )
    .bind(from)
    .fetch_all(&mut *connection)
    .await?
    .iter()
    .map(|row| {
        Ok((
            row.try_get(0)?,
            row.try_get(1)?,
            row.try_get(2)?,
            row.try_get(3)?,
            row.try_get(4)?,
        ))
    })
    .collect::<Result<_, sqlx::Error>>()?;
    let rolled: Option<i64> =
        sqlx::query_scalar("SELECT MAX(day) FROM hub_analytics_daily WHERE metric = 'total'")
            .fetch_one(&mut *connection)
            .await?;
    let mut day = match rolled {
        Some(rolled) => (rolled + DAY).max(from),
        None => {
            // Nothing rolled up yet: from the first day anything was counted.
            let first: Option<i64> = sqlx::query_scalar("SELECT MIN(day) FROM hub_analytics_views")
                .fetch_one(&mut *connection)
                .await?;
            let first_event: Option<i64> =
                sqlx::query_scalar("SELECT MIN(at) FROM hub_analytics_events")
                    .fetch_one(&mut *connection)
                    .await?;
            let first_event = first_event.map(|at| at - at.rem_euclid(DAY));
            match [first, first_event].into_iter().flatten().min() {
                Some(first) => first.max(from),
                None => today + DAY,
            }
        }
    };
    while day <= today {
        for (metric, item, views, visitors) in totals_in(connection, day).await? {
            rows.push((day, metric, item, views, visitors));
        }
        day += DAY;
    }
    Ok(rows)
}

/// The `days`-day range ending today, as the page shows it.
pub async fn report(store: &Store, days: i64, now: u64) -> BridgeResult<Report> {
    let from = start_of(days, now);
    let now_i = crate::store::secs(now);
    let (rows, milestones, waiting) = transaction!(store, |connection| {
        let rows = daily_in(connection, from, now).await?;
        let mut counts = BTreeMap::new();
        for (name, sql, since) in [
            (
                "made",
                "SELECT COUNT(*) FROM hub_analytics_events WHERE kind = 'made' AND at >= $1",
                from,
            ),
            (
                "active_week",
                "SELECT COUNT(*) FROM hub_analytics_events
                 WHERE kind = 'operation' AND last_at >= $1",
                now_i - 7 * DAY,
            ),
            (
                "active_month",
                "SELECT COUNT(*) FROM hub_analytics_events
                 WHERE kind = 'operation' AND last_at >= $1",
                now_i - 30 * DAY,
            ),
        ] {
            let count: i64 = sqlx::query_scalar(sql)
                .bind(since)
                .fetch_one(&mut *connection)
                .await?;
            counts.insert(name.to_string(), count);
        }
        // How far the accounts made in the range got.
        for row in sqlx::query(
            "SELECT kind, COUNT(*) FROM hub_analytics_events
             WHERE kind IN ('secured', 'admitted', 'machine', 'operation')
               AND tenant_id IN (SELECT tenant_id FROM hub_analytics_events
                                 WHERE kind = 'made' AND at >= $1)
             GROUP BY kind",
        )
        .bind(from)
        .fetch_all(&mut *connection)
        .await?
        {
            counts.insert(row.try_get::<String, _>(0)?, row.try_get::<i64, _>(1)?);
        }
        let mut details = Vec::new();
        for kind in ["made", "secured"] {
            let mut found = Vec::new();
            for row in sqlx::query(
                "SELECT detail, COUNT(*) FROM hub_analytics_events
                 WHERE kind = $1 AND at >= $2 GROUP BY detail ORDER BY 2 DESC, 1",
            )
            .bind(kind)
            .bind(from)
            .fetch_all(&mut *connection)
            .await?
            {
                found.push((row.try_get::<String, _>(0)?, row.try_get::<i64, _>(1)?));
            }
            details.push(found);
        }
        // Who waits now, as the operator page finds them: ids only, across tenants.
        let waiting = store
            .tenants_in(connection, crate::accounts::access::WAITING, 1_000_000)
            .await?
            .len();
        Ok::<_, Failed>((rows, (counts, details), waiting))
    })?;
    let (counts, mut details) = milestones;
    let mut report = Report {
        waiting: i64::try_from(waiting).unwrap_or(i64::MAX),
        active_week: counts.get("active_week").copied().unwrap_or(0),
        active_month: counts.get("active_month").copied().unwrap_or(0),
        sign_ins: details.pop().unwrap_or_default(),
        clients: details.pop().unwrap_or_default(),
        ..Report::default()
    };
    let mut by_day: BTreeMap<i64, (i64, i64)> =
        (0..days).map(|n| (from + n * DAY, (0, 0))).collect();
    let mut lists: BTreeMap<&str, BTreeMap<String, (i64, i64)>> = BTreeMap::new();
    for (day, metric, item, views, visitors) in &rows {
        match metric.as_str() {
            "total" => {
                if let Some(slot) = by_day.get_mut(day) {
                    *slot = (*views, *visitors);
                }
            }
            "path" | "referrer" | "campaign" | "device" => {
                let list = lists.entry(metric_key(metric)).or_default();
                let slot = list.entry(item.clone()).or_default();
                slot.0 += views;
                slot.1 += visitors;
            }
            _ => {}
        }
    }
    report.days = by_day
        .into_iter()
        .map(|(day, (views, visitors))| (day, views, visitors))
        .collect();
    let mut top = |metric: &str, most: usize| -> Vec<Counted> {
        let mut list: Vec<Counted> = lists
            .remove(metric)
            .unwrap_or_default()
            .into_iter()
            .map(|(item, (views, visitors))| Counted {
                item,
                views,
                visitors,
            })
            .collect();
        list.sort_by(|a, b| b.views.cmp(&a.views).then_with(|| a.item.cmp(&b.item)));
        list.truncate(most);
        list
    };
    report.pages = top("path", TOP);
    report.referrers = top("referrer", TOP);
    report.campaigns = top("campaign", TOP);
    report.devices = top("device", usize::MAX);
    report.funnel = Funnel {
        visitors: report.days.iter().map(|(_, _, visitors)| visitors).sum(),
        made: counts.get("made").copied().unwrap_or(0),
        secured: counts.get("secured").copied().unwrap_or(0),
        admitted: counts.get("admitted").copied().unwrap_or(0),
        machine: counts.get("machine").copied().unwrap_or(0),
        operation: counts.get("operation").copied().unwrap_or(0),
    };
    Ok(report)
}

fn metric_key(metric: &str) -> &'static str {
    match metric {
        "path" => "path",
        "referrer" => "referrer",
        "campaign" => "campaign",
        _ => "device",
    }
}

/// The daily totals of the `days`-day range ending today, as CSV: `day,metric,item,views,
/// visitors`, the day as `YYYY-MM-DD`. A field a spreadsheet would read as a formula is
/// written as text.
pub async fn csv(store: &Store, days: i64, now: u64) -> BridgeResult<String> {
    let from = start_of(days, now);
    let rows = transaction!(store, |connection| {
        Ok::<_, Failed>(daily_in(connection, from, now).await?)
    })?;
    let mut out = String::from("day,metric,item,views,visitors\r\n");
    for (day, metric, item, views, visitors) in rows {
        out.push_str(&format!(
            "{},{},{},{views},{visitors}\r\n",
            iso_day(day),
            field(&metric),
            field(&item)
        ));
    }
    Ok(out)
}

/// A day as `YYYY-MM-DD`.
pub fn iso_day(day: i64) -> String {
    crate::web::utc(u64::try_from(day).unwrap_or(0))
        .chars()
        .take(10)
        .collect()
}

/// One CSV field: quoted when it holds a comma, a quote or a line break; a leading `=`, `+`,
/// `-`, `@`, tab or carriage return (a formula, to a spreadsheet) gets a `'` first.
fn field(text: &str) -> String {
    let text = if text.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{text}")
    } else {
        text.to_string()
    };
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_csv_field_is_never_a_formula_and_always_one_field() {
        assert_eq!(field("news.example.net"), "news.example.net");
        assert_eq!(field("=HYPERLINK(\"x\")"), "\"'=HYPERLINK(\"\"x\"\")\"");
        assert_eq!(field("+1"), "'+1");
        assert_eq!(field("@sum"), "'@sum");
        assert_eq!(field("a,b"), "\"a,b\"");
        assert_eq!(iso_day(1_793_664_000), "2026-11-03");
        assert_eq!(start_of(7, 1_793_664_000 + 5), 1_793_664_000 - 6 * DAY);
    }
}
