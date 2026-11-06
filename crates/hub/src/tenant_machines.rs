//! Machine names on a hosted hub (`tenancy.mode multi`) are each tenant's own: two people
//! can both call a machine `laptop`, and neither can learn the other's names by trying
//! them. A machine is stored under its tenant's tag and the name its person gave
//! (`t1a2b3c4d5e6f-laptop`, `accounts::tenants::tag`); the person, their AI and their pages
//! only ever see `laptop`. The link and the machine itself use the stored name, which is
//! what the join line writes into the machine's config, so nothing on the wire changes.
//! A one-owner hub stores names as given, as before.
//!
//! Adding a machine checks the account's quota here too: an account nobody secured yet adds
//! `tenancy.unsecured.machines` at most, a tenant on a plan the plan's `machines`
//! (`tenant_access.rs`), and a tenant waiting to be let in adds none.

use devagotchi_contracts::{BridgeError, BridgeResult};

use crate::{accounts::tenants, auth::Subject, devices::Device, hub::Hub, site::Cap};

/// The longest name a person can give a machine on a hosted hub: the stored name, with the
/// tenant's 13-character tag and a dash before it, is 63 characters at most.
pub const HOSTED_NAME_MAX: usize = 63 - 14;

impl Hub {
    /// The tenant tag names of `subject`'s machines carry, on a hosted hub.
    fn tenant_tag(&self, subject: &Subject) -> Option<String> {
        if !self.config.multi_tenant() {
            return None;
        }
        subject.tenant.as_deref().map(tenants::tag)
    }

    /// The name a machine of `subject`'s called `name` is stored under.
    pub(crate) fn stored_name(&self, subject: &Subject, name: &str) -> String {
        match self.tenant_tag(subject) {
            Some(tag) => format!("{tag}-{name}"),
            None => name.to_string(),
        }
    }

    /// The name `subject` knows its machine stored as `stored` by.
    pub(crate) fn shown_name(&self, subject: &Subject, stored: &str) -> String {
        match self.tenant_tag(subject) {
            Some(tag) => stored
                .strip_prefix(&format!("{tag}-"))
                .unwrap_or(stored)
                .to_string(),
            None => stored.to_string(),
        }
    }

    /// `subject`'s own machine that it calls `name`: its tenant's first, then one it added
    /// under the bare name before the hub was hosted. Anyone else's is not found, so names
    /// of other accounts' machines are never revealed.
    pub(crate) fn own_machine(&self, subject: &Subject, name: &str) -> Option<Device> {
        let mine = |device: &Device| device.owner.as_deref() == Some(subject.id.as_str());
        let stored = self.stored_name(subject, name);
        if let Ok(Some(device)) = self.links.device(&stored)
            && mine(&device)
        {
            return Some(device);
        }
        match self.links.device(name) {
            Ok(Some(device)) if stored != name && mine(&device) => Some(device),
            _ => None,
        }
    }

    /// The machines `subject` added, each named as it knows them.
    pub(crate) fn own_machines(&self, subject: &Subject) -> Vec<Device> {
        self.owned(&subject.id)
            .into_iter()
            .map(|device| Device {
                name: self.shown_name(subject, &device.name),
                ..device
            })
            .collect()
    }

    /// A one-time code for a machine of `subject`'s called `name`: the name it is stored
    /// under, and the code. An account nobody secured adds `tenancy.unsecured.machines` at
    /// most; a tenant on a plan, the plan's `machines`; a tenant waiting to be let in, none.
    pub(crate) fn add_own_machine(
        &self,
        subject: &Subject,
        name: &str,
    ) -> BridgeResult<(String, String)> {
        if let Some(refused) = self.waiting_refusal(subject) {
            return Err(refused);
        }
        if self.tenant_tag(subject).is_some() && name.len() > HOSTED_NAME_MAX {
            return Err(BridgeError::new(
                "invalid_name",
                format!("a machine name here is at most {HOSTED_NAME_MAX} characters"),
            ));
        }
        if !crate::config::valid_name(name) {
            return Err(BridgeError::new(
                "invalid_name",
                "a machine name is 1 to 63 lowercase letters, digits and dashes, starting with a letter or digit",
            ));
        }
        let stored = self.stored_name(subject, name);
        let (limit, unsecured, plan) = self.machine_limit(subject);
        let code = match self
            .links
            .issue_owned_code_within(&subject.id, &stored, limit)
        {
            // The plan's limit, when it is the one reached, says so in the site's words.
            Err(error) if error.code == "machine_limit" => match plan {
                Some((plan, Cap::AtMost(most)))
                    if unsecured.is_none_or(|quota| most as usize <= quota) =>
                {
                    let held = self.links.held(&subject.id).unwrap_or(most as usize);
                    return Err(self.plan_limit(&plan, "machines", most, held));
                }
                _ => return Err(error),
            },
            other => other?,
        };
        Ok((stored, code))
    }

    /// The most machines `subject` may hold, where something holds it: an account nobody
    /// secured, `tenancy.unsecured.machines`; a tenant on a plan, the plan's `machines`; the
    /// lower when both do. With the quota and the plan, to say which one was reached.
    fn machine_limit(
        &self,
        subject: &Subject,
    ) -> (
        Option<usize>,
        Option<usize>,
        Option<(crate::site::Plan, Cap)>,
    ) {
        let unsecured = (self.config.multi_tenant() && subject.provisional_until.is_some())
            .then_some(self.config.tenancy.unsecured.machines as usize);
        let plan = self.plan_cap(subject, "machines");
        let planned = match &plan {
            Some((_, Cap::AtMost(most))) => Some(*most as usize),
            Some((_, Cap::Unlimited)) => Some(usize::MAX),
            Some((_, Cap::Unset)) | None => None,
        };
        let limit = match (unsecured, planned) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        (limit, unsecured, plan)
    }

    /// Why `subject` cannot add one more machine now, if it cannot: waiting to be let in, or
    /// a limit reached (in the site's words when it is the plan's). What a workflow's machine
    /// step says before anyone presses its button; adding one checks again.
    pub(crate) fn machine_room(&self, subject: &Subject) -> Option<BridgeError> {
        if let Some(refused) = self.waiting_refusal(subject) {
            return Some(refused);
        }
        if self.config.is_owner(&subject.id) {
            return None;
        }
        let (limit, unsecured, plan) = self.machine_limit(subject);
        let limit = limit.unwrap_or(crate::devices::OWNED_LIMIT);
        let held = self.links.held(&subject.id).unwrap_or(0);
        if held < limit {
            return None;
        }
        match plan {
            Some((plan, Cap::AtMost(most)))
                if unsecured.is_none_or(|quota| most as usize <= quota) =>
            {
                Some(self.plan_limit(&plan, "machines", most, held))
            }
            _ => Some(BridgeError::new(
                "machine_limit",
                format!(
                    "this account already has {held} machines on this hub (the most is {limit}); revoke one first"
                ),
            )),
        }
    }

    /// Revoke `subject`'s own machine called `name`.
    pub(crate) fn revoke_own_machine(
        &self,
        subject: &Subject,
        name: &str,
    ) -> BridgeResult<(bool, usize)> {
        let stored = self
            .own_machine(subject, name)
            .map(|device| device.name)
            .unwrap_or_else(|| self.stored_name(subject, name));
        self.links.revoke_owned(&subject.id, &stored)
    }
}
