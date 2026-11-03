//! The site's plans and early access as the hub enforces them (`accounts/access.rs`), and
//! the words it says about them (`content/messages.json` `access`).

use super::{
    Ctx, Site,
    model::{Limit, Plan, SignUp},
};

/// What a plan says about one limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    /// The plan leaves it to the hub's config.
    Unset,
    Unlimited,
    AtMost(u32),
}

impl Plan {
    /// What this plan says about limit `key` (one of `model::LIMIT_KEYS`).
    pub fn cap(&self, key: &str) -> Cap {
        match self.limits.get(key) {
            None => Cap::Unset,
            Some(Limit::Word(_)) => Cap::Unlimited,
            Some(Limit::Count(n)) => Cap::AtMost(*n),
        }
    }
}

impl Site {
    /// Whether a new tenant on a hosted hub is let in at once, or waits for an operator.
    pub fn sign_up(&self) -> SignUp {
        self.info.early_access.mode
    }

    /// The plan a new tenant starts on: the one marked `default`, else the first. `None`
    /// when the site lists no plans (nothing is then limited by a plan).
    pub fn default_plan(&self) -> Option<&Plan> {
        let plans = &self.info.plans;
        plans
            .iter()
            .find(|plan| plan.default)
            .or_else(|| plans.first())
    }

    /// The plan `id` names, if the site has it.
    pub fn plan(&self, id: &str) -> Option<&Plan> {
        self.info.plans.iter().find(|plan| plan.id == id)
    }

    /// The plan a tenant kept as `id` is on: that plan, else the default plan. The flag is
    /// true when `id` names a plan the site no longer has (the caller logs it).
    pub fn plan_for(&self, id: Option<&str>) -> (Option<&Plan>, bool) {
        match id.map(|id| self.plan(id)) {
            Some(Some(plan)) => (Some(plan), false),
            Some(None) => (self.default_plan(), true),
            None => (self.default_plan(), false),
        }
    }

    /// Where a limit message sends people for the plans: `links.plans`, else this hub's
    /// landing page at its Plans section.
    pub fn plans_url(&self, public_url: &str) -> String {
        match self.info.links.get("plans") {
            Some(url) => url.clone(),
            None => format!("{}/#plans", public_url.trim_end_matches('/')),
        }
    }

    /// `n` of limit `key`, worded as the Plans section words it.
    pub fn limit_words(&self, key: &str, n: usize) -> String {
        match self.landing.plans.limits.get(key) {
            Some(words) if n == 1 => words.one.replace("{n}", "1"),
            Some(words) => words.other.replace("{n}", &n.to_string()),
            None => format!("{n} {key}"),
        }
    }

    /// What every tool call but `whoami` answers while the caller's tenant waits.
    pub fn waiting_message(&self, since: &str, account_url: &str) -> String {
        Ctx::new(self)
            .with("since", since)
            .with("account", account_url)
            .plain(&self.messages.access.waiting)
    }

    /// A plan's limit reached: plan `plan`, limit `key` at `most`, `count` held now.
    pub fn limit_message(
        &self,
        plan: &Plan,
        key: &str,
        most: u32,
        count: usize,
        public_url: &str,
    ) -> String {
        let ctx = Ctx::new(self);
        Ctx::new(self)
            .with("plan", ctx.plain(&plan.name))
            .with("limit", ctx.plain(&self.limit_words(key, most as usize)))
            .with("count", ctx.plain(&self.limit_words(key, count)))
            .with("plans", self.plans_url(public_url))
            .plain(&self.messages.access.limit)
    }

    /// The email that says a tenant is in: subject and body.
    pub fn admitted_email(&self, account_url: &str) -> (String, String) {
        let ctx = Ctx::new(self).with("account", account_url);
        let email = &self.messages.access.admitted_email;
        (ctx.plain(&email.subject), ctx.plain(&email.body))
    }

    /// Each early-access, plan and operator message with what fills it where it is drawn,
    /// for `Site::check`.
    pub(super) fn message_checks(
        &self,
    ) -> Vec<(&'static str, &str, Vec<(&'static str, &'static str)>, bool)> {
        let access = &self.messages.access;
        let since = ("since", "1 January 2026");
        let account = ("account", "https://hub.example.org/account");
        let mut checks = vec![
            (
                "access.waiting",
                access.waiting.as_str(),
                vec![since, account],
                false,
            ),
            (
                "access.waiting_title",
                access.waiting_title.as_str(),
                vec![],
                false,
            ),
            (
                "access.account_waiting",
                access.account_waiting.as_str(),
                vec![since],
                true,
            ),
            (
                "access.account_admitted",
                access.account_admitted.as_str(),
                vec![since, ("plan", "Free")],
                true,
            ),
            (
                "access.limit",
                access.limit.as_str(),
                vec![
                    ("plan", "Free"),
                    ("limit", "1 project"),
                    ("count", "1 project"),
                    ("plans", "https://hub.example.org/#plans"),
                ],
                false,
            ),
            (
                "access.limit_title",
                access.limit_title.as_str(),
                vec![],
                false,
            ),
            (
                "access.admitted_email.subject",
                access.admitted_email.subject.as_str(),
                vec![account],
                false,
            ),
            (
                "access.admitted_email.body",
                access.admitted_email.body.as_str(),
                vec![account],
                false,
            ),
        ];
        let operator = &self.messages.operator;
        let tenant = ("tenant", "t-00000000-0000-4000-8000-000000000000");
        let columns = &operator.columns;
        for (what, text, vars, inline) in [
            ("operator.title", &operator.title, vec![], false),
            (
                "operator.account_link",
                &operator.account_link,
                vec![],
                true,
            ),
            ("operator.status_open", &operator.status_open, vec![], true),
            (
                "operator.status_early_access",
                &operator.status_early_access,
                vec![],
                true,
            ),
            (
                "operator.how_to_change",
                &operator.how_to_change,
                vec![],
                true,
            ),
            (
                "operator.done_admitted",
                &operator.done_admitted,
                vec![("n", "2")],
                true,
            ),
            ("operator.done_plan", &operator.done_plan, vec![], true),
            ("operator.none_picked", &operator.none_picked, vec![], true),
            (
                "operator.not_from_here",
                &operator.not_from_here,
                vec![],
                false,
            ),
            (
                "operator.waiting_title",
                &operator.waiting_title,
                vec![],
                false,
            ),
            (
                "operator.waiting_empty",
                &operator.waiting_empty,
                vec![],
                false,
            ),
            (
                "operator.admitted_title",
                &operator.admitted_title,
                vec![],
                false,
            ),
            (
                "operator.admitted_empty",
                &operator.admitted_empty,
                vec![],
                false,
            ),
            ("operator.columns.tenant", &columns.tenant, vec![], false),
            ("operator.columns.joined", &columns.joined, vec![], false),
            (
                "operator.columns.sign_ins",
                &columns.sign_ins,
                vec![],
                false,
            ),
            ("operator.columns.secured", &columns.secured, vec![], false),
            ("operator.columns.made_by", &columns.made_by, vec![], false),
            ("operator.columns.active", &columns.active, vec![], false),
            ("operator.columns.plan", &columns.plan, vec![], false),
            (
                "operator.columns.admitted",
                &columns.admitted,
                vec![],
                false,
            ),
            ("operator.columns.usage", &columns.usage, vec![], false),
            ("operator.secured", &operator.secured, vec![], false),
            ("operator.not_secured", &operator.not_secured, vec![], false),
            ("operator.no_sign_ins", &operator.no_sign_ins, vec![], false),
            ("operator.never", &operator.never, vec![], false),
            ("operator.pick", &operator.pick, vec![tenant], false),
            ("operator.plan_for", &operator.plan_for, vec![tenant], false),
            ("operator.admit", &operator.admit, vec![], false),
            ("operator.change_plan", &operator.change_plan, vec![], false),
            (
                "operator.admitted_line",
                &operator.admitted_line,
                vec![("when", "x"), ("by", "y")],
                false,
            ),
            (
                "operator.usage_of",
                &operator.usage_of,
                vec![("count", "1"), ("limit", "2")],
                false,
            ),
            (
                "operator.usage_unlimited",
                &operator.usage_unlimited,
                vec![("count", "1")],
                false,
            ),
        ] {
            checks.push((what, text.as_str(), vars, inline));
        }
        checks
    }
}
