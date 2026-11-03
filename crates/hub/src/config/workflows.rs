//! `workflows` in the hub's config: how long the link of a workflow an AI starts for its
//! person (`/go/<token>`, `src/workflows.rs`) keeps working. The link is the credential (no
//! sign-in), so it is short-lived.
//!
//! ```jsonc
//! "workflows": {
//!   "link_ttl_secs": 1800   // the default, 30 minutes; 300 (five minutes) to 86400 (a day)
//! }
//! ```
//!
//! A link also stops working the moment its workflow completes or is cancelled. A browser
//! that did a step through it may keep the account for as long again.

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WorkflowsConfig {
    /// Seconds a workflow's link works after the AI starts it.
    pub link_ttl_secs: u64,
}

impl Default for WorkflowsConfig {
    fn default() -> Self {
        Self {
            link_ttl_secs: 30 * 60,
        }
    }
}

impl WorkflowsConfig {
    pub(crate) fn check(&self) -> Result<(), String> {
        if !(300..=24 * 3600).contains(&self.link_ttl_secs) {
            return Err("workflows.link_ttl_secs is 300 to 86400 (five minutes to a day)".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thirty_minutes_by_default_and_held_between_five_minutes_and_a_day() {
        assert_eq!(WorkflowsConfig::default().link_ttl_secs, 1800);
        assert!(WorkflowsConfig::default().check().is_ok());
        for (secs, ok) in [(299, false), (300, true), (86_400, true), (86_401, false)] {
            assert_eq!(
                WorkflowsConfig { link_ttl_secs: secs }.check().is_ok(),
                ok,
                "{secs}"
            );
        }
        let parsed: WorkflowsConfig = serde_json::from_str(r#"{"link_ttl_secs": 600}"#).unwrap();
        assert_eq!(parsed.link_ttl_secs, 600);
        assert!(serde_json::from_str::<WorkflowsConfig>(r#"{"ttl": 600}"#).is_err());
    }
}
