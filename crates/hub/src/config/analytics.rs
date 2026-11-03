//! `analytics` in the hub's config: whether a hosted hub counts visits to its public pages
//! and its accounts' milestones for its operator (`src/analytics.rs`), and how long it
//! keeps each visit before only the daily totals remain.
//!
//! ```jsonc
//! "analytics": {
//!   "enabled": true,        // the default; false: nothing is counted or kept
//!   "retention_days": 90    // raw visits, 1 to 3650; the daily totals are kept
//! }
//! ```
//!
//! Both have environment overrides, so a deploy sets them from `.env` without editing the
//! file: `DEVAGOTCHI_HUB_ANALYTICS` (`on` or `off`) and
//! `DEVAGOTCHI_HUB_ANALYTICS_RETENTION_DAYS`. Only a hosted hub (`tenancy.mode multi`)
//! counts anything: its operator page is the one place the numbers are read.

use serde::Deserialize;

/// The environment variable that sets `analytics.enabled`: `on` or `off`.
pub const ANALYTICS_ENV: &str = "DEVAGOTCHI_HUB_ANALYTICS";
/// The environment variable that sets `analytics.retention_days`.
pub const RETENTION_ENV: &str = "DEVAGOTCHI_HUB_ANALYTICS_RETENTION_DAYS";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AnalyticsConfig {
    pub enabled: bool,
    /// Days each raw visit is kept; after that only the daily totals are.
    pub retention_days: u32,
}

impl Default for AnalyticsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            retention_days: 90,
        }
    }
}

impl AnalyticsConfig {
    pub(crate) fn check(&self) -> Result<(), String> {
        if !(1..=3650).contains(&self.retention_days) {
            return Err("analytics.retention_days is 1 to 3650".into());
        }
        Ok(())
    }

    /// The file's settings with the environment's over them.
    pub(crate) fn from_env(mut self, env: &dyn Fn(&str) -> Option<String>) -> Result<Self, String> {
        if let Some(value) = env(ANALYTICS_ENV).filter(|value| !value.trim().is_empty()) {
            self.enabled = match value.trim() {
                "on" => true,
                "off" => false,
                other => return Err(format!("{ANALYTICS_ENV}={other:?}: on or off")),
            };
        }
        if let Some(value) = env(RETENTION_ENV).filter(|value| !value.trim().is_empty()) {
            self.retention_days = value
                .trim()
                .parse()
                .map_err(|_| format!("{RETENTION_ENV}={value:?}: a number of days, 1 to 3650"))?;
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn on_by_default_for_ninety_days_and_the_environment_overrides_both() {
        let config = AnalyticsConfig::default();
        assert!(config.enabled);
        assert_eq!(config.retention_days, 90);
        let set = config
            .from_env(&env(&[(ANALYTICS_ENV, "off"), (RETENTION_ENV, "30")]))
            .unwrap();
        assert_eq!(
            set,
            AnalyticsConfig {
                enabled: false,
                retention_days: 30
            }
        );
        assert!(config.from_env(&env(&[(ANALYTICS_ENV, "yes")])).is_err());
        assert!(config.from_env(&env(&[(RETENTION_ENV, "ninety")])).is_err());
        let blank = config.from_env(&env(&[(ANALYTICS_ENV, " ")])).unwrap();
        assert_eq!(blank, config);
        for days in [0, 3651] {
            let wrong = AnalyticsConfig {
                retention_days: days,
                ..config
            };
            assert!(wrong.check().is_err(), "{days}");
        }
    }
}
