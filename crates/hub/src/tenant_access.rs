//! Early access and plans where a caller acts (`accounts/access.rs` keeps them): a tenant
//! waiting to be let in is answered, not served, and a tenant's plan caps what it holds.
//!
//! - Waiting: every MCP tool but `whoami` answers with the site's one message (when it
//!   joined the list, that an email comes when it is in, its account page) instead of
//!   acting, and adding a machine, enrolling one or deciding on a board is refused the same.
//! - Plans hold tenants on a hosted hub (`tenancy.mode multi`), everyone but its owner. A
//!   limit a plan sets replaces the config's (`account_boards.free_limit`, the machine
//!   quota); a limit it leaves out stays the config's; an account nobody secured is also
//!   held to `tenancy.unsecured`. A hosted tenant is one account (made on first connect or
//!   first sign-in; sign-ins link to it), so its count is that account's.
//! - Reaching a limit is refused with `plan_limit`: the plan, the limit, the count and where
//!   the plans are, in the site's words.

use devagotchi_contracts::BridgeError;
use rmcp::model::{CallToolResult, ContentBlock};

use crate::{
    accounts::access,
    auth::Subject,
    hub::Hub,
    site::{Cap, Plan},
};

impl Hub {
    /// The account page this hub's messages link to.
    fn account_url(&self) -> String {
        format!("{}/account", self.config.public_url.trim_end_matches('/'))
    }

    /// What a waiting caller is told: the site's message, with when it joined the list.
    pub(crate) fn waiting_text(&self, subject: &Subject) -> String {
        let since = subject
            .access
            .as_ref()
            .and_then(|access| access.waiting_since)
            .map(crate::web::utc)
            .unwrap_or_default();
        self.config
            .site()
            .waiting_message(&since, &self.account_url())
    }

    /// The refusal for a caller whose tenant waits to be let in, if it does.
    pub(crate) fn waiting_refusal(&self, subject: &Subject) -> Option<BridgeError> {
        subject
            .waiting()
            .then(|| BridgeError::new("early_access", self.waiting_text(subject)))
    }

    /// What a tool call answers while the caller's tenant waits: the message alone, as
    /// text. Marked an error result because nothing ran: every hub tool declares an output
    /// schema, and a result that is not an error must carry content that matches it (MCP
    /// clients check it). No structured content, so no client misreads it against that
    /// schema; the text is a plain sentence for the person.
    pub(crate) fn waiting_result(&self, subject: &Subject) -> Option<CallToolResult> {
        subject
            .waiting()
            .then(|| CallToolResult::error(vec![ContentBlock::text(self.waiting_text(subject))]))
    }

    /// The plan `subject`'s tenant is held to: on a hosted hub, for anyone but its owner,
    /// with a tenant the hub keeps.
    pub(crate) fn held_plan(&self, subject: &Subject) -> Option<Plan> {
        if !self.config.multi_tenant() || self.config.is_owner(&subject.id) {
            return None;
        }
        let tenant = subject.tenant.as_deref()?;
        let kept = subject.access.as_ref()?;
        access::plan_for(&self.config.site(), tenant, kept)
    }

    /// What `subject`'s plan says about limit `key`, with the plan; `None` when no plan
    /// holds it.
    pub(crate) fn plan_cap(&self, subject: &Subject, key: &str) -> Option<(Plan, Cap)> {
        self.held_plan(subject).map(|plan| {
            let cap = plan.cap(key);
            (plan, cap)
        })
    }

    /// The refusal for reaching limit `key` of `plan` (at most `most`, `count` held).
    pub(crate) fn plan_limit(
        &self,
        plan: &Plan,
        key: &str,
        most: u32,
        count: usize,
    ) -> BridgeError {
        BridgeError::new(
            "plan_limit",
            self.config
                .site()
                .limit_message(plan, key, most, count, &self.config.public_url),
        )
    }
}
