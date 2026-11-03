//! `/operator/analytics`: what a hosted hub counted for its operator (`crate::analytics`),
//! and `/operator/analytics.csv`, the daily totals behind it (increment 67).
//!
//! - The operator page's rule: only the hub's owner, signed in, on a hosted hub; anyone
//!   else, and any other hub, gets the empty 404 an unknown path gets.
//! - A range of 7, 30 or 90 days, as plain links. Traffic: visitors and page views per
//!   day as an inline SVG chart (each day's numbers in its tooltip) with the same numbers
//!   in a table, the top pages, referrers and campaigns, and the device split. The funnel:
//!   visitors, then the accounts made in the range and how far they got, with the rate
//!   from each step to the next. Accounts: by AI app and by sign-in, who waits now, and
//!   who ran a command in the last 7 and 30 days.
//! - Server-rendered, no script, nothing from anywhere else; colours and fonts are the
//!   site's tokens through `hub.css`, in light and dark. Every word is the site's
//!   (`content/messages.json` `analytics`).

use axum::{
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use super::{
    operator::{hidden, operator},
    pages::{self, escape},
};
use crate::{
    AppState,
    analytics::{
        funnel::FAMILIES,
        report::{self, Counted, Funnel, RANGES, Report, iso_day},
        visit::DEVICES,
    },
    devices::now_secs,
    site::{Ctx, Site},
};

/// The range shown when none (or another) is asked for.
const DEFAULT_DAYS: i64 = 30;

#[derive(Deserialize)]
pub struct Range {
    #[serde(default)]
    days: Option<String>,
}

/// The range asked for: one of [`RANGES`], else [`DEFAULT_DAYS`].
fn days(range: &Range) -> i64 {
    range
        .days
        .as_deref()
        .and_then(|days| days.parse().ok())
        .filter(|days| RANGES.contains(days))
        .unwrap_or(DEFAULT_DAYS)
}

pub async fn page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(range): Query<Range>,
) -> Response {
    if operator(&state, &headers).await.is_none() {
        return hidden();
    }
    let Some(hub) = &state.passkeys else {
        return hidden();
    };
    let site = pages::site();
    let copy = &site.messages.analytics;
    let ctx = || Ctx::new(&site);
    let Some(analytics) = &state.hub.analytics else {
        return pages::html(
            StatusCode::OK,
            &copy.title,
            &format!(
                "<h1>{}</h1><p class=\"note\" id=\"off\">{}</p>",
                escape(&copy.title),
                ctx().inline(&copy.off)
            ),
        );
    };
    let head = format!(
        "<h1>{}</h1><p class=\"meta\">{}</p>",
        escape(&copy.title),
        ctx().inline(&copy.intro)
    );
    let days = days(&range);
    let report = match report::report(hub.accounts().store(), days, now_secs()).await {
        Ok(report) => report,
        Err(error) => {
            eprintln!("devagotchi-hub: analytics: {}", error.message);
            return super::routes::unavailable();
        }
    };
    let mut kept = ctx()
        .with("days", analytics.retention_days().to_string())
        .inline(&copy.kept);
    if analytics.dropped() > 0 {
        kept.push(' ');
        kept.push_str(
            &ctx()
                .with("n", analytics.dropped().to_string())
                .inline(&copy.dropped),
        );
    }
    let body = format!(
        "{head}{}<p class=\"meta\">{kept}</p><div class=\"report\">{}{}{}</div>",
        range_nav(&site, days),
        traffic(&site, &report, days),
        funnel(&site, &report.funnel, days),
        accounts(&site, &report),
    );
    pages::html(StatusCode::OK, &copy.title, &body)
}

/// `GET /operator/analytics.csv?days=N`: the daily totals of the range, as a file.
pub async fn csv(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(range): Query<Range>,
) -> Response {
    if operator(&state, &headers).await.is_none() {
        return hidden();
    }
    let Some(hub) = &state.passkeys else {
        return hidden();
    };
    let days = days(&range);
    let now = now_secs();
    let text = match report::csv(hub.accounts().store(), days, now).await {
        Ok(text) => text,
        Err(error) => {
            eprintln!("devagotchi-hub: analytics: {}", error.message);
            return super::routes::unavailable();
        }
    };
    let name = format!(
        "attachment; filename=\"analytics-{}-to-{}.csv\"",
        iso_day(report::start_of(days, now)),
        iso_day(report::start_of(1, now))
    );
    let mut response = (StatusCode::OK, text).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    if let Ok(value) = HeaderValue::from_str(&name) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

/// The range links and the export.
fn range_nav(site: &Site, days: i64) -> String {
    let copy = &site.messages.analytics;
    let links: String = RANGES
        .iter()
        .map(|n| {
            let label = escape(
                &Ctx::new(site)
                    .with("n", n.to_string())
                    .plain(&copy.range_days),
            );
            if *n == days {
                format!(
                    "<a href=\"/operator/analytics?days={n}\" aria-current=\"page\">{label}</a>"
                )
            } else {
                format!("<a href=\"/operator/analytics?days={n}\">{label}</a>")
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "<nav class=\"range\" aria-label=\"{range}\"><span>{range}</span> {links}</nav>\
<p><a class=\"button\" id=\"export\" href=\"/operator/analytics.csv?days={days}\" download>{}</a></p>",
        escape(&copy.export),
        range = escape(&copy.range),
    )
}

/// A step of `max` that a few round lines divide, and the top of the scale.
fn scale(max: i64) -> (i64, i64) {
    if max <= 0 {
        return (1, 4);
    }
    let raw = max as f64 / 4.0;
    let magnitude = 10f64.powf(raw.log10().floor());
    let nice = match raw / magnitude {
        n if n <= 1.0 => 1.0,
        n if n <= 2.0 => 2.0,
        n if n <= 5.0 => 5.0,
        _ => 10.0,
    };
    let step = ((nice * magnitude).round() as i64).max(1);
    (step, (max + step - 1) / step * step)
}

/// A bar from `base` up to `top`, its upper corners rounded.
fn bar(x: f64, width: f64, top: f64, base: f64) -> String {
    let r = 2f64.min(width / 2.0).min(base - top);
    format!(
        "M{x:.1} {base:.1}V{:.1}Q{x:.1} {top:.1} {:.1} {top:.1}H{:.1}Q{:.1} {top:.1} {:.1} {:.1}V{base:.1}Z",
        top + r,
        x + r,
        x + width - r,
        x + width,
        x + width,
        top + r,
    )
}

/// Visitors and page views per day: page views as pale bars, the day's visitors as the
/// solid part of each (a visitor views at least one page), one scale, a tooltip per day.
fn chart(site: &Site, report: &Report, days: i64) -> String {
    const W: f64 = 720.0;
    const H: f64 = 220.0;
    const LEFT: f64 = 44.0;
    const RIGHT: f64 = 8.0;
    const TOP: f64 = 10.0;
    const BOTTOM: f64 = 26.0;
    let copy = &site.messages.analytics;
    let max = report
        .days
        .iter()
        .map(|(_, views, _)| *views)
        .max()
        .unwrap_or(0);
    let (step, top) = scale(max);
    let plot_w = W - LEFT - RIGHT;
    let plot_h = H - TOP - BOTTOM;
    let base = TOP + plot_h;
    let y = |value: i64| base - value as f64 / top as f64 * plot_h;
    let slot = plot_w / report.days.len().max(1) as f64;
    let gap = if slot > 6.0 { 2.0 } else { 1.0 };
    let width = (slot - gap).max(1.0);
    let mut grid = String::new();
    let mut value = 0;
    while value <= top {
        grid.push_str(&format!(
            "<line class=\"grid\" x1=\"{LEFT}\" x2=\"{:.1}\" y1=\"{y:.1}\" y2=\"{y:.1}\"/>\
<text class=\"tick\" x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"end\">{value}</text>",
            W - RIGHT,
            LEFT - 6.0,
            y(value) + 4.0,
            y = y(value),
        ));
        value += step;
    }
    let mut marks = String::new();
    for (i, (day, views, visitors)) in report.days.iter().enumerate() {
        let x = LEFT + i as f64 * slot + gap / 2.0;
        let tip = Ctx::new(site)
            .with("day", iso_day(*day))
            .with("views", views.to_string())
            .with("visitors", visitors.to_string())
            .plain(&copy.chart_day);
        let mut shapes = String::new();
        if *views > 0 {
            shapes.push_str(&format!(
                "<path class=\"views\" d=\"{}\"/>",
                bar(x, width, y(*views), base)
            ));
        }
        if *visitors > 0 {
            shapes.push_str(&format!(
                "<path class=\"visitors\" d=\"{}\"/>",
                bar(x, width, y(*visitors), base)
            ));
        }
        marks.push_str(&format!(
            "<g class=\"day\"><title>{}</title>{shapes}<rect class=\"hit\" x=\"{:.1}\" y=\"{TOP}\" width=\"{slot:.1}\" height=\"{plot_h:.1}\"/></g>",
            escape(&tip),
            x - gap / 2.0,
        ));
    }
    let (first, last) = (
        report
            .days
            .first()
            .map(|d| iso_day(d.0))
            .unwrap_or_default(),
        report.days.last().map(|d| iso_day(d.0)).unwrap_or_default(),
    );
    let (views, visitors): (i64, i64) = report
        .days
        .iter()
        .fold((0, 0), |(a, b), (_, v, u)| (a + v, b + u));
    let summary = Ctx::new(site)
        .with("views", views.to_string())
        .with("visitors", visitors.to_string())
        .with("n", days.to_string())
        .plain(&copy.chart_summary);
    format!(
        "<figure class=\"chart\"><svg viewBox=\"0 0 {W} {H}\" role=\"img\" aria-labelledby=\"chart-title chart-desc\">\
<title id=\"chart-title\">{}</title><desc id=\"chart-desc\">{}</desc>{grid}\
<line class=\"axis\" x1=\"{LEFT}\" x2=\"{:.1}\" y1=\"{base:.1}\" y2=\"{base:.1}\"/>{marks}\
<text class=\"tick\" x=\"{LEFT}\" y=\"{:.1}\">{first}</text>\
<text class=\"tick\" x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"end\">{last}</text></svg>\
<figcaption class=\"legend\">{} {}</figcaption></figure>",
        escape(&copy.chart_title),
        escape(&summary),
        W - RIGHT,
        H - 8.0,
        W - RIGHT,
        H - 8.0,
        key("views", &copy.columns.views),
        key("visitors", &copy.columns.visitors),
    )
}

/// A legend entry: the series' swatch and its name.
fn key(class: &str, name: &str) -> String {
    format!(
        "<span class=\"key\"><svg class=\"swatch\" viewBox=\"0 0 12 12\" aria-hidden=\"true\">\
<rect class=\"{class}\" width=\"12\" height=\"12\" rx=\"2\"/></svg>{}</span>",
        escape(name)
    )
}

/// A table with `head` (column names) and `rows` (cells, escaped already).
fn table(head: &[&str], rows: &[Vec<String>]) -> String {
    let head: String = head
        .iter()
        .map(|name| format!("<th scope=\"col\">{}</th>", escape(name)))
        .collect();
    let rows: String = rows
        .iter()
        .map(|cells| {
            let cells: String = cells
                .iter()
                .enumerate()
                .map(|(i, cell)| {
                    if i == 0 {
                        format!("<th scope=\"row\">{cell}</th>")
                    } else {
                        format!("<td class=\"n\">{cell}</td>")
                    }
                })
                .collect();
            format!("<tr>{cells}</tr>")
        })
        .collect();
    format!("<table><thead><tr>{head}</tr></thead><tbody>{rows}</tbody></table>")
}

/// A titled part: its heading, then `content`, or the empty note when there is none.
fn part(id: &str, title: &str, content: Option<String>, none: &str) -> String {
    format!(
        "<section id=\"{id}\" aria-labelledby=\"{id}-title\"><h3 id=\"{id}-title\">{}</h3>{}</section>",
        escape(title),
        content.unwrap_or_else(|| format!("<p class=\"empty\">{}</p>", escape(none)))
    )
}

fn counted(list: &[Counted], label: impl Fn(&str) -> String) -> Vec<Vec<String>> {
    list.iter()
        .map(|row| {
            vec![
                label(&row.item),
                row.views.to_string(),
                row.visitors.to_string(),
            ]
        })
        .collect()
}

fn share(part: i64, whole: i64) -> String {
    if whole <= 0 {
        return "0%".into();
    }
    format!("{:.1}%", part as f64 * 100.0 / whole as f64)
}

fn traffic(site: &Site, report: &Report, days: i64) -> String {
    let copy = &site.messages.analytics;
    let columns = &copy.columns;
    let none = &copy.none;
    let daily: Vec<Vec<String>> = report
        .days
        .iter()
        .rev()
        .map(|(day, views, visitors)| vec![iso_day(*day), views.to_string(), visitors.to_string()])
        .collect();
    let numbers = format!(
        "<details><summary>{}</summary>{}</details>",
        escape(&copy.table_summary),
        table(&[&columns.day, &columns.views, &columns.visitors], &daily)
    );
    let head = [&columns.page[..], &columns.views, &columns.visitors];
    let pages = (!report.pages.is_empty()).then(|| {
        table(
            &head,
            &counted(&report.pages, |path| {
                format!("<code>{}</code>", escape(path))
            }),
        )
    });
    let head = [&columns.referrer[..], &columns.views, &columns.visitors];
    let referrers =
        (!report.referrers.is_empty()).then(|| table(&head, &counted(&report.referrers, escape)));
    let campaigns = (!report.campaigns.is_empty()).then(|| {
        let rows: Vec<Vec<String>> = report
            .campaigns
            .iter()
            .map(|row| {
                let mut parts = row.item.splitn(3, '|').map(escape);
                vec![
                    parts.next().unwrap_or_default(),
                    parts.next().unwrap_or_default(),
                    parts.next().unwrap_or_default(),
                    row.views.to_string(),
                    row.visitors.to_string(),
                ]
            })
            .collect();
        format!(
            "<div class=\"wide\">{}</div>",
            table(
                &[
                    &columns.source,
                    &columns.medium,
                    &columns.campaign,
                    &columns.views,
                    &columns.visitors
                ],
                &rows
            )
        )
    });
    let total: i64 = report.devices.iter().map(|row| row.views).sum();
    let devices = (!report.devices.is_empty()).then(|| {
        let names = &copy.devices;
        let rows: Vec<Vec<String>> = DEVICES
            .iter()
            .map(|device| {
                let views = report
                    .devices
                    .iter()
                    .find(|row| row.item == *device)
                    .map(|row| row.views)
                    .unwrap_or(0);
                let name = match *device {
                    "mobile" => &names.mobile,
                    "tablet" => &names.tablet,
                    _ => &names.desktop,
                };
                vec![escape(name), views.to_string(), share(views, total)]
            })
            .collect();
        table(&[&columns.device, &columns.views, &columns.share], &rows)
    });
    format!(
        "<section id=\"traffic\" aria-labelledby=\"traffic-title\"><h2 id=\"traffic-title\">{}</h2>{}{numbers}{}{}{}{}</section>",
        escape(&copy.traffic_title),
        chart(site, report, days),
        part("pages", &copy.pages_title, pages, none),
        part("referrers", &copy.referrers_title, referrers, none),
        part("campaigns", &copy.campaigns_title, campaigns, none),
        part("devices", &copy.devices_title, devices, none),
    )
}

fn funnel(site: &Site, funnel: &Funnel, days: i64) -> String {
    let copy = &site.messages.analytics;
    let steps = &copy.steps;
    let columns = &copy.columns;
    let never = &site.messages.operator.never;
    let counts = [
        (&steps.visitors, funnel.visitors),
        (&steps.made, funnel.made),
        (&steps.secured, funnel.secured),
        (&steps.admitted, funnel.admitted),
        (&steps.machine, funnel.machine),
        (&steps.operation, funnel.operation),
    ];
    let most = counts.iter().map(|(_, n)| *n).max().unwrap_or(0).max(1);
    let mut before: Option<i64> = None;
    let rows: String = counts
        .iter()
        .map(|(name, count)| {
            let rate = match before {
                Some(before) if before > 0 => share(*count, before),
                _ => never.clone(),
            };
            before = Some(*count);
            format!(
                "<tr><th scope=\"row\">{}</th><td class=\"n\">{count}</td><td class=\"n\">{}</td>\
<td><svg class=\"meter\" viewBox=\"0 0 100 10\" preserveAspectRatio=\"none\" aria-hidden=\"true\">\
<rect class=\"track\" width=\"100\" height=\"10\"/><rect class=\"fill\" width=\"{:.1}\" height=\"10\"/></svg></td></tr>",
                escape(name),
                escape(&rate),
                *count as f64 * 100.0 / most as f64,
            )
        })
        .collect();
    format!(
        "<section id=\"funnel\" aria-labelledby=\"funnel-title\"><h2 id=\"funnel-title\">{}</h2>\
<p class=\"meta\">{}</p><table class=\"funnel\"><thead><tr><th scope=\"col\">{}</th><th scope=\"col\">{}</th>\
<th scope=\"col\">{}</th><th scope=\"col\"><span class=\"visually-hidden\">{}</span></th></tr></thead>\
<tbody>{rows}</tbody></table></section>",
        escape(&copy.funnel_title),
        Ctx::new(site)
            .with("n", days.to_string())
            .inline(&copy.funnel_intro),
        escape(&columns.step),
        escape(&columns.count),
        escape(&columns.conversion),
        escape(&columns.count),
    )
}

fn accounts(site: &Site, report: &Report) -> String {
    let copy = &site.messages.analytics;
    let columns = &copy.columns;
    let names = &copy.clients;
    let made = |family: &str| -> i64 {
        report
            .clients
            .iter()
            .filter(|(detail, _)| {
                detail == family || (family == "other" && !FAMILIES.contains(&detail.as_str()))
            })
            .map(|(_, n)| n)
            .sum()
    };
    let clients: Vec<Vec<String>> = FAMILIES
        .iter()
        .map(|family| {
            let name = match *family {
                "claude" => &names.claude,
                "chatgpt" => &names.chatgpt,
                "grok" => &names.grok,
                "cursor" => &names.cursor,
                _ => &names.other,
            };
            vec![escape(name), made(family).to_string()]
        })
        .collect();
    let sign_ins = (!report.sign_ins.is_empty()).then(|| {
        let rows: Vec<Vec<String>> = report
            .sign_ins
            .iter()
            .map(|(how, n)| {
                let name = if how == "passkey" {
                    copy.passkey.as_str()
                } else {
                    site.label(how, how)
                };
                vec![escape(name), n.to_string()]
            })
            .collect();
        table(&[&columns.sign_in, &columns.accounts], &rows)
    });
    format!(
        "<section id=\"accounts\" aria-labelledby=\"accounts-title\"><h2 id=\"accounts-title\">{}</h2>\
<p id=\"waiting-now\">{}</p><p id=\"active\">{}</p>{}{}</section>",
        escape(&copy.accounts_title),
        Ctx::new(site)
            .with("n", report.waiting.to_string())
            .inline(&copy.waiting_now),
        Ctx::new(site)
            .with("week", report.active_week.to_string())
            .with("month", report.active_month.to_string())
            .inline(&copy.active),
        part(
            "clients",
            &copy.clients_title,
            Some(table(&[&columns.client, &columns.accounts], &clients)),
            &copy.none
        ),
        part("sign-ins", &copy.sign_ins_title, sign_ins, &copy.none),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scale_is_round_and_a_bar_stays_in_its_box() {
        assert_eq!(scale(0), (1, 4));
        assert_eq!(scale(3), (1, 3));
        assert_eq!(scale(7), (2, 8));
        assert_eq!(scale(37), (10, 40));
        assert_eq!(scale(1234), (500, 1500));
        assert_eq!(
            bar(10.0, 6.0, 50.0, 100.0),
            "M10.0 100.0V52.0Q10.0 50.0 12.0 50.0H14.0Q16.0 50.0 16.0 52.0V100.0Z"
        );
        assert_eq!(
            days(&Range {
                days: Some("90".into())
            }),
            90
        );
        for asked in [None, Some("14".into()), Some("x".into())] {
            assert_eq!(days(&Range { days: asked }), DEFAULT_DAYS);
        }
    }
}
