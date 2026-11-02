//! `tenancy` and `operators` in the hub's config: whether this hub is one person's (or one
//! team's) hub, or a hosted service where everyone who connects gets an account and a space
//! of their own, and who runs it.
//!
//! ```jsonc
//! "tenancy": {
//!   "mode": "multi",                 // "single" (the default): today's hub, claimed by its owner
//!   "domain": "devagotchi.app",      // optional: tenant hostnames <slug>.<domain>
//!   "trusted_proxies": ["10.0.0.0/8"],  // whose X-Forwarded-For names the client
//!   "starts_per_hour": 8,            // new accounts per client address per hour
//!   "max_unsecured": 10000,          // accounts not yet secured, live at once
//!   "unsecured": { "machines": 2, "clients": 3, "idle_days": 14 }
//! },
//! "operators": ["email:jesse@example.com", "github:12345"]
//! ```
//!
//! Both have environment overrides, so a deploy sets them from `.env` or the secret store
//! without editing the file: `DEVAGOTCHI_HUB_TENANCY` (`single` or `multi`) and
//! `DEVAGOTCHI_HUB_OPERATORS` (comma separated). Nothing about either is in the code.

use std::net::IpAddr;

use serde::Deserialize;

/// The environment variable that sets `tenancy.mode`.
pub const TENANCY_ENV: &str = "DEVAGOTCHI_HUB_TENANCY";
/// The environment variable that sets `operators`, comma separated.
pub const OPERATORS_ENV: &str = "DEVAGOTCHI_HUB_OPERATORS";

/// The identity providers an operator entry may name, as `<provider>:<subject>`; `email:`
/// names a verified address instead.
pub const OPERATOR_PROVIDERS: [&str; 5] = ["email", "google", "github", "nomen", "oidc"];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TenancyMode {
    /// One tenant: the hub is its owner's, claimed as before (16c) when no operator is set.
    #[default]
    Single,
    /// A hosted service: everyone who connects gets an account and a tenant of their own.
    Multi,
}

/// What an account nobody has secured yet may hold (`tenancy.unsecured`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct UnsecuredLimits {
    /// Machines it may add.
    pub machines: u32,
    /// AI clients (and terminals) it may connect at once.
    pub clients: u32,
    /// Days unused after which it ends, with everything it connected.
    pub idle_days: u32,
}

impl Default for UnsecuredLimits {
    fn default() -> Self {
        Self {
            machines: 2,
            clients: 3,
            idle_days: 14,
        }
    }
}

impl UnsecuredLimits {
    pub fn idle_secs(&self) -> u64 {
        u64::from(self.idle_days) * 24 * 3600
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TenancyConfig {
    pub mode: TenancyMode,
    /// Tenant hostnames are `<slug>.<domain>` when set.
    pub domain: Option<String>,
    /// Proxies in front of the hub (addresses or CIDR ranges) whose `X-Forwarded-For` is
    /// believed: the client's address is the last one in it no such proxy added. Empty: the
    /// address that connected is the client's.
    pub trusted_proxies: Vec<String>,
    /// New accounts one client address may make in an hour.
    pub starts_per_hour: u32,
    /// Accounts not yet secured that may be live at once on the hub.
    pub max_unsecured: u32,
    pub unsecured: UnsecuredLimits,
}

impl Default for TenancyConfig {
    fn default() -> Self {
        Self {
            mode: TenancyMode::Single,
            domain: None,
            trusted_proxies: Vec::new(),
            starts_per_hour: 8,
            max_unsecured: 10_000,
            unsecured: UnsecuredLimits::default(),
        }
    }
}

impl TenancyConfig {
    pub fn multi(&self) -> bool {
        self.mode == TenancyMode::Multi
    }

    pub(crate) fn check(&self) -> Result<(), String> {
        if let Some(domain) = &self.domain {
            let ok = domain.len() <= 200
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && !domain.contains("..")
                && domain.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.'
                });
            if !ok {
                return Err(format!(
                    "tenancy.domain {domain:?} is a lowercase domain name like devagotchi.app"
                ));
            }
        }
        for proxy in &self.trusted_proxies {
            Cidr::parse(proxy).ok_or_else(|| {
                format!("tenancy.trusted_proxies: {proxy:?} is an IP address or a CIDR range")
            })?;
        }
        if !(1..=10_000).contains(&self.starts_per_hour) {
            return Err("tenancy.starts_per_hour is 1 to 10000".into());
        }
        if !(1..=10_000_000).contains(&self.max_unsecured) {
            return Err("tenancy.max_unsecured is 1 to 10000000".into());
        }
        let limits = &self.unsecured;
        if !(0..=100).contains(&limits.machines) {
            return Err("tenancy.unsecured.machines is 0 to 100".into());
        }
        if !(1..=100).contains(&limits.clients) {
            return Err("tenancy.unsecured.clients is 1 to 100".into());
        }
        if !(1..=90).contains(&limits.idle_days) {
            return Err("tenancy.unsecured.idle_days is 1 to 90".into());
        }
        Ok(())
    }

    /// Whether `ip`, the address that connected, is a proxy whose forwarding is believed.
    pub fn trusts(&self, ip: IpAddr) -> bool {
        self.trusted_proxies
            .iter()
            .filter_map(|proxy| Cidr::parse(proxy))
            .any(|range| range.contains(ip))
    }

    /// The client's address: `peer`, unless it is a trusted proxy, then the last address in
    /// `X-Forwarded-For` that no trusted proxy added (a client can put anything it likes in
    /// front of that, and none of it is believed).
    pub fn client_address(&self, peer: IpAddr, forwarded: Option<&str>) -> IpAddr {
        if !self.trusts(peer) {
            return peer;
        }
        let mut client = peer;
        for hop in forwarded.unwrap_or("").rsplit(',') {
            let Ok(ip) = hop.trim().parse::<IpAddr>() else {
                break;
            };
            client = ip;
            if !self.trusts(ip) {
                break;
            }
        }
        client
    }
}

/// An address range: `10.0.0.0/8`, `fd00::/8`, or a bare address.
#[derive(Debug, Clone, Copy)]
struct Cidr {
    base: IpAddr,
    bits: u8,
}

impl Cidr {
    fn parse(text: &str) -> Option<Self> {
        let (address, bits) = match text.trim().split_once('/') {
            Some((address, bits)) => (address, Some(bits.parse::<u8>().ok()?)),
            None => (text.trim(), None),
        };
        let base: IpAddr = address.parse().ok()?;
        let max = if base.is_ipv4() { 32 } else { 128 };
        let bits = bits.unwrap_or(max);
        (bits <= max).then_some(Self { base, bits })
    }

    fn contains(&self, ip: IpAddr) -> bool {
        let ip = match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
            v4 => v4,
        };
        match (self.base, ip) {
            (IpAddr::V4(base), IpAddr::V4(ip)) => {
                let mask = u32::MAX.checked_shl(32 - u32::from(self.bits)).unwrap_or(0);
                u32::from(base) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(base), IpAddr::V6(ip)) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.bits))
                    .unwrap_or(0);
                u128::from(base) & mask == u128::from(ip) & mask
            }
            _ => false,
        }
    }
}

/// An `operators` entry: `email:<verified address>` or `<provider>:<subject>`, provider one
/// of [`OPERATOR_PROVIDERS`]. Emails compare in lowercase.
pub(crate) fn check_operator(entry: &str) -> Result<(), String> {
    let Some((provider, subject)) = entry.split_once(':') else {
        return Err(format!(
            "operators: {entry:?} is provider:subject (email:you@example.com, github:12345, google:…, nomen:…)"
        ));
    };
    if !OPERATOR_PROVIDERS.contains(&provider) {
        return Err(format!(
            "operators: {provider:?} is not one of {}",
            OPERATOR_PROVIDERS.join(", ")
        ));
    }
    let subject = subject.trim();
    if subject.is_empty()
        || subject.len() > 256
        || subject.chars().any(|c| c.is_whitespace() || c.is_control())
        || (provider == "email" && !subject.contains('@'))
    {
        return Err(format!("operators: {entry:?} has no usable subject"));
    }
    Ok(())
}

/// Whether an identity (`provider`, `subject`, and its verified email, if any) is one the
/// `operators` list names.
pub fn is_operator(
    operators: &[String],
    provider: &str,
    subject: &str,
    verified_email: Option<&str>,
) -> bool {
    operators.iter().any(|entry| match entry.split_once(':') {
        Some(("email", address)) => {
            verified_email.is_some_and(|email| email.trim().eq_ignore_ascii_case(address.trim()))
        }
        Some((named, id)) => named == provider && id.trim() == subject,
        None => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_client_is_the_last_address_no_trusted_proxy_added() {
        let config = TenancyConfig {
            trusted_proxies: vec!["10.0.0.0/8".into(), "::1".into()],
            ..TenancyConfig::default()
        };
        let ip = |text: &str| text.parse::<IpAddr>().unwrap();
        // Straight from the internet: what connected, whatever the header says.
        assert_eq!(
            config.client_address(ip("203.0.113.9"), Some("198.51.100.1")),
            ip("203.0.113.9")
        );
        // Through the proxy: the address it saw, never one the client wrote before it.
        assert_eq!(
            config.client_address(ip("10.1.2.3"), Some("6.6.6.6, 198.51.100.7")),
            ip("198.51.100.7")
        );
        // Two trusted hops.
        assert_eq!(
            config.client_address(ip("10.1.2.3"), Some("198.51.100.7, 10.9.9.9")),
            ip("198.51.100.7")
        );
        // A trusted proxy that forwarded nothing readable is the client.
        assert_eq!(config.client_address(ip("::1"), Some("garbage")), ip("::1"));
        assert_eq!(config.client_address(ip("::1"), None), ip("::1"));
        // Nothing trusted by default.
        assert_eq!(
            TenancyConfig::default().client_address(ip("10.1.2.3"), Some("198.51.100.7")),
            ip("10.1.2.3")
        );
    }

    #[test]
    fn operators_name_a_verified_email_or_a_providers_subject() {
        let operators = vec![
            "email:Jesse@Example.com".to_string(),
            "github:42".to_string(),
        ];
        assert!(is_operator(
            &operators,
            "google",
            "x",
            Some("jesse@example.com")
        ));
        assert!(!is_operator(&operators, "google", "x", None));
        assert!(is_operator(&operators, "github", "42", None));
        assert!(!is_operator(&operators, "github", "43", None));
        assert!(!is_operator(&operators, "google", "42", None));
        for good in [
            "email:a@b.c",
            "github:1",
            "google:118",
            "nomen:395290028818825218",
            "oidc:x",
        ] {
            assert_eq!(check_operator(good), Ok(()), "{good}");
        }
        for bad in [
            "jesse",
            "email:jesse",
            "twitter:1",
            "github:",
            "github: 1 2",
        ] {
            assert!(check_operator(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn limits_stay_in_their_ranges() {
        assert_eq!(TenancyConfig::default().check(), Ok(()));
        let bad = [
            TenancyConfig {
                starts_per_hour: 0,
                ..TenancyConfig::default()
            },
            TenancyConfig {
                unsecured: UnsecuredLimits {
                    idle_days: 0,
                    ..UnsecuredLimits::default()
                },
                ..TenancyConfig::default()
            },
            TenancyConfig {
                trusted_proxies: vec!["10.0.0.0/33".into()],
                ..TenancyConfig::default()
            },
            TenancyConfig {
                domain: Some("Devagotchi.app".into()),
                ..TenancyConfig::default()
            },
        ];
        for config in bad {
            assert!(config.check().is_err(), "{config:?}");
        }
    }
}
