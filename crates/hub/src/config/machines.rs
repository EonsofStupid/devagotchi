//! `machines` in the hub's config: whether machines update themselves to the build this hub
//! hands out (docs/spec-machine-update.md, U4).
//!
//! ```jsonc
//! "machines": {
//!   "auto_update": true   // the default; false: only `machine_update` updates a machine
//! }
//! ```

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MachinesConfig {
    /// Once an hour, and when one connects, each online machine on another build with no
    /// running jobs updates itself. `machine_update` works either way.
    pub auto_update: bool,
}

impl Default for MachinesConfig {
    fn default() -> Self {
        Self { auto_update: true }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machines_update_themselves_unless_turned_off() {
        assert!(MachinesConfig::default().auto_update);
        let off: MachinesConfig = serde_json::from_str(r#"{"auto_update": false}"#).unwrap();
        assert!(!off.auto_update);
        let empty: MachinesConfig = serde_json::from_str("{}").unwrap();
        assert!(empty.auto_update);
        assert!(serde_json::from_str::<MachinesConfig>(r#"{"auto": true}"#).is_err());
    }
}
