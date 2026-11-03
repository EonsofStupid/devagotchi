//! The analytics page's copy (`content/messages.json` `analytics`), each message with what
//! fills it where it is drawn, for `Site::check`.

use super::Site;

/// What one message is checked with: where it is, its text, its placeholders and whether it
/// is drawn with inline markup.
type Check<'a> = (
    &'static str,
    &'a str,
    Vec<(&'static str, &'static str)>,
    bool,
);

impl Site {
    pub(super) fn analytics_checks(&self) -> Vec<Check<'_>> {
        let copy = &self.messages.analytics;
        let n = ("n", "30");
        let mut checks: Vec<Check<'_>> = vec![
            ("analytics.title", &copy.title, vec![], false),
            ("analytics.operator_link", &copy.operator_link, vec![], true),
            ("analytics.intro", &copy.intro, vec![], true),
            ("analytics.off", &copy.off, vec![], true),
            ("analytics.range", &copy.range, vec![], false),
            ("analytics.range_days", &copy.range_days, vec![n], false),
            ("analytics.export", &copy.export, vec![], false),
            ("analytics.kept", &copy.kept, vec![("days", "90")], true),
            ("analytics.dropped", &copy.dropped, vec![n], true),
            (
                "analytics.traffic_title",
                &copy.traffic_title,
                vec![],
                false,
            ),
            ("analytics.chart_title", &copy.chart_title, vec![], false),
            (
                "analytics.chart_summary",
                &copy.chart_summary,
                vec![("views", "120"), ("visitors", "40"), n],
                false,
            ),
            (
                "analytics.chart_day",
                &copy.chart_day,
                vec![("day", "2026-01-01"), ("views", "12"), ("visitors", "4")],
                false,
            ),
            (
                "analytics.table_summary",
                &copy.table_summary,
                vec![],
                false,
            ),
            ("analytics.pages_title", &copy.pages_title, vec![], false),
            (
                "analytics.referrers_title",
                &copy.referrers_title,
                vec![],
                false,
            ),
            (
                "analytics.campaigns_title",
                &copy.campaigns_title,
                vec![],
                false,
            ),
            (
                "analytics.devices_title",
                &copy.devices_title,
                vec![],
                false,
            ),
            ("analytics.none", &copy.none, vec![], false),
            ("analytics.funnel_title", &copy.funnel_title, vec![], false),
            ("analytics.funnel_intro", &copy.funnel_intro, vec![n], true),
            (
                "analytics.accounts_title",
                &copy.accounts_title,
                vec![],
                false,
            ),
            (
                "analytics.clients_title",
                &copy.clients_title,
                vec![],
                false,
            ),
            (
                "analytics.sign_ins_title",
                &copy.sign_ins_title,
                vec![],
                false,
            ),
            ("analytics.waiting_now", &copy.waiting_now, vec![n], true),
            (
                "analytics.active",
                &copy.active,
                vec![("week", "3"), ("month", "7")],
                true,
            ),
            ("analytics.passkey", &copy.passkey, vec![], false),
        ];
        let columns = &copy.columns;
        let steps = &copy.steps;
        let clients = &copy.clients;
        let devices = &copy.devices;
        for (what, text) in [
            ("analytics.columns.day", &columns.day),
            ("analytics.columns.views", &columns.views),
            ("analytics.columns.visitors", &columns.visitors),
            ("analytics.columns.page", &columns.page),
            ("analytics.columns.referrer", &columns.referrer),
            ("analytics.columns.source", &columns.source),
            ("analytics.columns.medium", &columns.medium),
            ("analytics.columns.campaign", &columns.campaign),
            ("analytics.columns.device", &columns.device),
            ("analytics.columns.share", &columns.share),
            ("analytics.columns.step", &columns.step),
            ("analytics.columns.count", &columns.count),
            ("analytics.columns.conversion", &columns.conversion),
            ("analytics.columns.client", &columns.client),
            ("analytics.columns.sign_in", &columns.sign_in),
            ("analytics.columns.accounts", &columns.accounts),
            ("analytics.steps.visitors", &steps.visitors),
            ("analytics.steps.made", &steps.made),
            ("analytics.steps.secured", &steps.secured),
            ("analytics.steps.admitted", &steps.admitted),
            ("analytics.steps.machine", &steps.machine),
            ("analytics.steps.operation", &steps.operation),
            ("analytics.clients.claude", &clients.claude),
            ("analytics.clients.chatgpt", &clients.chatgpt),
            ("analytics.clients.grok", &clients.grok),
            ("analytics.clients.cursor", &clients.cursor),
            ("analytics.clients.other", &clients.other),
            ("analytics.devices.mobile", &devices.mobile),
            ("analytics.devices.tablet", &devices.tablet),
            ("analytics.devices.desktop", &devices.desktop),
        ] {
            checks.push((what, text.as_str(), vec![], false));
        }
        checks
    }
}
