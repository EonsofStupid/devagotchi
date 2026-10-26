//! Hub configuration: who may reach which broker, and how callers are authenticated.
//!
//! Unknown fields and unsafe values fail closed, like the broker's own config.

use std::{collections::BTreeSet, path::Path};

use devagotchi_contracts::{BridgeError, BridgeResult};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HubConfig {
    /// Socket address the hub listens on. Put it behind the edge (Caddy); the hub
    /// speaks plain HTTP and trusts nothing from the network but the bearer token.
    pub listen: String,
    /// The hub's own HTTPS URL as clients see it. It is the protected resource
    /// identifier (RFC 9728) and the `resource` hosted clients send.
    pub public_url: String,
    pub auth: AuthConfig,
    /// The account ID (`sub`) of the person who owns this hub. The owner reaches every
    /// broker without a grant, and nothing at runtime can take that away: only editing
    /// this file changes it. Required with Nomen sign-in, so a hub can never be set up
    /// without someone who can always get in.
    #[serde(default)]
    pub owner: Option<String>,
    /// Brokers this hub can route to: local ones by their IPC endpoint, and brokers on
    /// other machines reached through the link their `devagotchi link` holds open.
    pub brokers: Vec<BrokerEntry>,
    /// Which authenticated subject may use which brokers and boards. No grant, no route.
    pub grants: Vec<Grant>,
    /// Project boards this hub serves over `/mcp` (`board_*` tools): each is a project
    /// directory on this host that `devagotchi board init` prepared, so the same
    /// store answers the command line here and agents anywhere.
    #[serde(default)]
    pub boards: Vec<BoardEntry>,
    /// Boards this hub hosts for accounts (D23, packet 15): one per account and project,
    /// made by `board_bind`, kept under `dir`. Absent: only the `boards` above exist here.
    #[serde(default)]
    pub account_boards: Option<AccountBoardsConfig>,
    /// Browser sign-in for people: `/account` shows who you are signed in as and your
    /// machines. Nomen sign-in only. Absent: no pages, only `/mcp`, `/link` and the APIs.
    #[serde(default)]
    pub web: Option<WebConfig>,
    /// A directory of Devagotchi builds this hub hands to machines that join it
    /// (`/dist/<file>`, `/join/<code>`): `SHA256SUMS`, `VERSION` and the files they list,
    /// laid down at deploy from the GitHub build the hub itself runs. Absent: no `/join`,
    /// and "Add a machine" shows the installer lines instead.
    #[serde(default)]
    pub downloads: Option<String>,
}

/// The hosted tier's boards: an account binds a project and gets a board of its own on this
/// hub, reached by name like a configured one; its seats (`X-Devagotchi-Seat`) are the
/// account's own agent names on it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountBoardsConfig {
    /// Absolute directory for the stores; default `<state dir>/account-boards`.
    #[serde(default)]
    pub dir: Option<String>,
    /// How many projects a free account may bind here (D21: realistically one). The owner
    /// and the `unlimited` accounts have no limit.
    #[serde(default)]
    pub free_limit: Option<u32>,
    /// Account IDs with no limit (paid, or friends).
    #[serde(default)]
    pub unlimited: Vec<String>,
}

/// What a free account gets when the config does not say.
pub const DEFAULT_FREE_LIMIT: u32 = 1;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardEntry {
    /// The project name callers pass as `project`; `[a-z0-9][a-z0-9-]{0,62}`. It must be
    /// the name the board was initialised with (`.devagotchi/config.json`), because ticket
    /// ids embed its prefix.
    pub project: String,
    /// Absolute path of the project directory holding `.devagotchi/board.sqlite3`.
    pub dir: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    /// The Nomen application people sign in through on the hub's pages: a User Agent app
    /// with PKCE and no secret, redirect URI `<public_url>/auth/callback`. It must also be
    /// in `auth.clients`: the hub checks the tokens it gets like any other token.
    pub client_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, tag = "mode", rename_all = "snake_case")]
pub enum AuthConfig {
    /// Bearer tokens are JWTs issued by an OpenID Connect provider (Nomen).
    /// Discovery runs at start; keys come from the provider's JWKS.
    Oidc {
        /// Issuer URL exactly as the provider states it (`iss` claim).
        issuer: String,
        /// Accepted `aud` values: the hub's own client/project ids at the provider,
        /// and the dynamic-registration project when hosted clients register there.
        audiences: Vec<String>,
    },
    /// Shippin sign-in. Nomen (Shippin's identity provider) issues opaque access
    /// tokens, so the hub asks Nomen about each one (token introspection, RFC 7662),
    /// signing in to Nomen as its own API application with a private-key JWT.
    Nomen {
        /// Nomen's issuer URL exactly as Nomen states it, e.g. `https://id.shippin.cloud`.
        /// Tokens are checked at `<issuer>/oauth/v2/introspect`.
        issuer: String,
        /// Nomen project ids whose tokens this hub accepts. Nomen reports a token active
        /// only when the hub's application is in its audience; the hub also requires one
        /// of these ids in `aud`.
        audiences: Vec<String>,
        /// Nomen client ids (applications) whose sign-ins the hub accepts, such as
        /// mcp.shippin.cloud's and shippin-bridge's. Any application in Nomen can ask for the
        /// hub's audience, and a person already signed in to Nomen can be handed a token by
        /// any of them, so the hub also requires the token's `client_id` to be one of these.
        clients: Vec<String>,
        /// Absolute path to the hub's API application key, the JSON file Nomen downloads
        /// (`type`, `keyId`, `key`, `clientId`). Owner-only (chmod 0600).
        key_file: String,
    },
    /// Static tokens from a file the hub owner controls (mode 0600). For tests only
    /// (D21): accepted when `public_url` is loopback http, refused for a hub people reach.
    Dev {
        /// Absolute path to a JSON object `{ "<token>": "<subject>" }`.
        tokens_file: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerEntry {
    /// Stable name callers use in `broker` fields; `[a-z0-9][a-z0-9-]{0,62}`. For a
    /// linked broker it is also the device name the machine enrolls under.
    pub name: String,
    /// A broker on this host: the absolute endpoint on its ready line (Unix socket path
    /// or named pipe). Exactly one of `socket` and `link`.
    #[serde(default)]
    pub socket: Option<String>,
    /// A broker on another machine, reached over the outbound link it dials to `/link`.
    #[serde(default)]
    pub link: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    /// The subject as the identity provider names it (`sub`), or the dev token's subject.
    pub subject: String,
    /// Broker names this subject may route to.
    #[serde(default)]
    pub brokers: Vec<String>,
    /// Project boards this subject may work on.
    #[serde(default)]
    pub boards: Vec<String>,
    /// The subject is a person, not an agent: on a board they may force-release a ticket
    /// and move one by hand. The owner is always a person; agents never are.
    #[serde(default)]
    pub human: bool,
}

impl HubConfig {
    pub fn parse(text: &str) -> BridgeResult<Self> {
        let config: HubConfig = serde_json::from_str(text)
            .map_err(|error| BridgeError::new("invalid_config", error.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> BridgeResult<()> {
        let invalid = |message: String| BridgeError::new("invalid_config", message);
        if self.listen.parse::<std::net::SocketAddr>().is_err() {
            return Err(invalid(format!(
                "listen must be host:port, got {:?}",
                self.listen
            )));
        }
        if !self.public_url.starts_with("https://") && !self.public_url.starts_with("http://") {
            return Err(invalid("public_url must be an absolute http(s) URL".into()));
        }
        match &self.auth {
            AuthConfig::Oidc { issuer, audiences } => {
                if !issuer.starts_with("https://") {
                    return Err(invalid("auth.issuer must be an https URL".into()));
                }
                if audiences.is_empty() || audiences.iter().any(|a| a.trim().is_empty()) {
                    return Err(invalid(
                        "auth.audiences must name at least one audience".into(),
                    ));
                }
            }
            AuthConfig::Nomen {
                issuer,
                audiences,
                clients,
                key_file,
            } => {
                if !issuer.starts_with("https://") && !loopback_http(issuer) {
                    return Err(invalid(
                        "auth.issuer must be an https URL (plain http only on this machine's loopback, for tests)"
                            .into(),
                    ));
                }
                if audiences.is_empty() || audiences.iter().any(|a| a.trim().is_empty()) {
                    return Err(invalid(
                        "auth.audiences must name at least one Nomen project id".into(),
                    ));
                }
                if clients.is_empty() || clients.iter().any(|c| c.trim().is_empty()) {
                    return Err(invalid(
                        "auth.clients must name the Nomen applications whose sign-ins the hub accepts"
                            .into(),
                    ));
                }
                if !Path::new(key_file).is_absolute() {
                    return Err(invalid("auth.key_file must be an absolute path".into()));
                }
                if self.owner.is_none() {
                    return Err(invalid(
                        "Nomen sign-in needs an owner: the account ID (sub) that can always reach every broker"
                            .into(),
                    ));
                }
            }
            AuthConfig::Dev { tokens_file } => {
                if !Path::new(tokens_file).is_absolute() {
                    return Err(invalid("auth.tokens_file must be an absolute path".into()));
                }
                // D21: dev tokens are for tests. A hub with an address people reach signs
                // them in with Nomen or an OpenID Connect issuer; a static token would be
                // a password in a file, on the network, with no one to revoke it.
                if !loopback_http(&self.public_url) {
                    return Err(invalid(format!(
                        "auth.mode dev is for tests (public_url on loopback http); a hub at {} signs people in with auth.mode nomen or oidc",
                        self.public_url
                    )));
                }
            }
        }
        if let Some(owner) = &self.owner {
            valid_subject(owner).map_err(|why| invalid(format!("owner {why}")))?;
        }
        if let Some(dir) = &self.downloads
            && !std::path::Path::new(dir).is_absolute()
        {
            return Err(invalid(format!("downloads {dir:?} must be an absolute path")));
        }
        if let Some(web) = &self.web {
            let AuthConfig::Nomen { clients, .. } = &self.auth else {
                return Err(invalid("web sign-in needs auth mode nomen".into()));
            };
            if web.client_id.trim().is_empty() || !clients.contains(&web.client_id) {
                return Err(invalid(
                    "web.client_id must be one of auth.clients, so the tokens it issues are accepted"
                        .into(),
                ));
            }
            // Session cookies are Secure: over plain http they would never come back.
            if !self.public_url.starts_with("https://") && !loopback_http(&self.public_url) {
                return Err(invalid(
                    "web sign-in needs an https public_url (plain http only on loopback, for tests)"
                        .into(),
                ));
            }
        }
        // Both lists may be empty: a hub that serves nothing yet still answers whoami and
        // /healthz, so an installer can bring it up before its first board or machine.
        if self.brokers.len() > 256 || self.boards.len() > 256 {
            return Err(invalid("at most 256 brokers and 256 boards".into()));
        }
        let mut names = BTreeSet::new();
        for broker in &self.brokers {
            if !valid_name(&broker.name) {
                return Err(invalid(format!(
                    "broker name {:?} is not [a-z0-9][a-z0-9-]{{0,62}}",
                    broker.name
                )));
            }
            if !names.insert(broker.name.as_str()) {
                return Err(invalid(format!(
                    "broker name {:?} is listed twice",
                    broker.name
                )));
            }
            match (&broker.socket, broker.link) {
                (Some(socket), false) if Path::new(socket).is_absolute() => {}
                (Some(_), false) => {
                    return Err(invalid(format!(
                        "broker {:?}: socket must be an absolute path",
                        broker.name
                    )));
                }
                _ => {
                    if broker.socket.is_some() || !broker.link {
                        return Err(invalid(format!(
                            "broker {:?}: give exactly one of socket or \"link\": true",
                            broker.name
                        )));
                    }
                }
            }
        }
        let mut projects = BTreeSet::new();
        for board in &self.boards {
            if !valid_name(&board.project) {
                return Err(invalid(format!(
                    "board project {:?} is not [a-z0-9][a-z0-9-]{{0,62}}",
                    board.project
                )));
            }
            if !projects.insert(board.project.as_str()) {
                return Err(invalid(format!(
                    "board project {:?} is listed twice",
                    board.project
                )));
            }
            if !Path::new(&board.dir).is_absolute() {
                return Err(invalid(format!(
                    "board {:?}: dir must be an absolute path",
                    board.project
                )));
            }
        }
        if let Some(accounts) = &self.account_boards {
            if let Some(dir) = &accounts.dir
                && !Path::new(dir).is_absolute()
            {
                return Err(invalid(
                    "account_boards.dir must be an absolute path".into(),
                ));
            }
            for subject in &accounts.unlimited {
                valid_subject(subject)
                    .map_err(|why| invalid(format!("account_boards.unlimited {why}")))?;
            }
        }
        if self.grants.len() > 4096 {
            return Err(invalid("at most 4096 grants".into()));
        }
        for grant in &self.grants {
            valid_subject(&grant.subject).map_err(|why| invalid(format!("grant subject {why}")))?;
            for name in &grant.brokers {
                if !names.contains(name.as_str()) {
                    return Err(invalid(format!(
                        "grant for {:?} names unknown broker {:?}",
                        grant.subject, name
                    )));
                }
            }
            for project in &grant.boards {
                if !projects.contains(project.as_str()) {
                    return Err(invalid(format!(
                        "grant for {:?} names unknown board {:?}",
                        grant.subject, project
                    )));
                }
            }
        }
        Ok(())
    }

    /// Whether this subject is the hub's configured owner.
    pub fn is_owner(&self, subject: &str) -> bool {
        self.owner.as_deref() == Some(subject)
    }

    /// The people who decide approvals on `project`: the owner and every grant that is
    /// marked human and names the board.
    pub fn people_for_board(&self, project: &str) -> Vec<String> {
        let mut people: Vec<String> = self.owner.iter().cloned().collect();
        for grant in &self.grants {
            if grant.human
                && grant.boards.iter().any(|b| b == project)
                && !people.contains(&grant.subject)
            {
                people.push(grant.subject.clone());
            }
        }
        people
    }

    /// Whether this subject acts as a person on boards: the owner, or a grant that says so.
    pub fn is_human(&self, subject: &str) -> bool {
        self.is_owner(subject) || self.grants.iter().any(|g| g.subject == subject && g.human)
    }

    /// Boards the subject may work on, in config order. The owner reaches every board.
    pub fn boards_for(&self, subject: &str) -> Vec<&BoardEntry> {
        if self.is_owner(subject) {
            return self.boards.iter().collect();
        }
        let mut allowed = BTreeSet::new();
        for grant in self.grants.iter().filter(|g| g.subject == subject) {
            allowed.extend(grant.boards.iter().map(String::as_str));
        }
        self.boards
            .iter()
            .filter(|b| allowed.contains(b.project.as_str()))
            .collect()
    }

    pub fn board(&self, project: &str) -> Option<&BoardEntry> {
        self.boards.iter().find(|b| b.project == project)
    }

    /// Broker names the subject may route to, in config order, without duplicates.
    /// The owner reaches every broker.
    pub fn brokers_for(&self, subject: &str) -> Vec<&BrokerEntry> {
        if self.is_owner(subject) {
            return self.brokers.iter().collect();
        }
        let mut allowed = BTreeSet::new();
        for grant in self.grants.iter().filter(|g| g.subject == subject) {
            allowed.extend(grant.brokers.iter().map(String::as_str));
        }
        self.brokers
            .iter()
            .filter(|b| allowed.contains(b.name.as_str()))
            .collect()
    }

    pub fn broker(&self, name: &str) -> Option<&BrokerEntry> {
        self.brokers.iter().find(|b| b.name == name)
    }
}

/// A subject is the account ID the identity provider issues, which never changes. An
/// email or a display name can be shared by two accounts or change hands (Shippin once
/// had two active accounts with the same name), so neither is accepted where an ID belongs.
pub(crate) fn valid_subject(subject: &str) -> Result<(), String> {
    if subject.is_empty() || subject.len() > 256 {
        return Err(format!("{subject:?} must be 1..=256 characters"));
    }
    if subject.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!(
            "{subject:?} has spaces; use the account ID (sub), not a name"
        ));
    }
    if subject.contains('@') {
        return Err(format!(
            "{subject:?} is an email; use the account ID (sub), which never changes. `whoami` shows it"
        ));
    }
    Ok(())
}

/// `http://` to this machine's loopback only, where no network carries the traffic.
fn loopback_http(url: &str) -> bool {
    let Ok(uri) = url.parse::<http::Uri>() else {
        return false;
    };
    uri.scheme_str() == Some("http")
        && uri.authority().is_some_and(|a| !a.as_str().contains('@'))
        && matches!(
            uri.host(),
            Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
        )
}

pub(crate) fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 63
        && bytes[0].is_ascii_lowercase() | bytes[0].is_ascii_digit()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Absolute paths in the platform's own form: `/…` is not absolute on Windows.
    #[cfg(unix)]
    const TOKENS: &str = "/tmp/tokens.json";
    #[cfg(windows)]
    const TOKENS: &str = r"C:\tmp\tokens.json";
    #[cfg(unix)]
    const SOCKET: &str = "/run/broker.sock";
    #[cfg(windows)]
    const SOCKET: &str = r"\\.\pipe\devagotchi-broker-test";

    fn good() -> serde_json::Value {
        serde_json::json!({
            "listen": "127.0.0.1:8787",
            "public_url": "http://127.0.0.1:8787",
            "auth": { "mode": "dev", "tokens_file": TOKENS },
            "brokers": [ { "name": "gold-ws", "socket": SOCKET } ],
            "grants": [ { "subject": "jesse", "brokers": ["gold-ws"] } ]
        })
    }

    fn parse(value: &serde_json::Value) -> BridgeResult<HubConfig> {
        HubConfig::parse(&value.to_string())
    }

    #[test]
    fn accepts_a_minimal_dev_config_and_resolves_grants() {
        let config = parse(&good()).unwrap();
        assert_eq!(config.brokers_for("jesse").len(), 1);
        assert!(config.brokers_for("someone-else").is_empty());
        assert!(config.broker("gold-ws").is_some());
    }

    #[test]
    fn account_boards_need_an_absolute_dir_and_real_account_ids() {
        let mut hosted = good();
        hosted["account_boards"] = serde_json::json!({});
        let config = parse(&hosted).unwrap();
        let accounts = config.account_boards.as_ref().unwrap();
        assert!(
            accounts.dir.is_none()
                && accounts.free_limit.is_none()
                && accounts.unlimited.is_empty()
        );

        let mut relative = good();
        relative["account_boards"] = serde_json::json!({ "dir": "boards" });
        assert_eq!(parse(&relative).unwrap_err().code, "invalid_config");

        let mut email = good();
        email["account_boards"] = serde_json::json!({ "unlimited": ["someone@example.test"] });
        assert_eq!(parse(&email).unwrap_err().code, "invalid_config");

        let mut unknown = good();
        unknown["account_boards"] = serde_json::json!({ "limit": 3 });
        assert_eq!(
            parse(&unknown).unwrap_err().code,
            "invalid_config",
            "unknown field fails closed"
        );
    }

    #[test]
    fn dev_tokens_are_for_tests_a_reachable_hub_refuses_them() {
        for public_url in [
            "https://hub.example",
            "http://hub.example:8787",
            "https://127.0.0.1",
        ] {
            let mut reachable = good();
            reachable["public_url"] = serde_json::json!(public_url);
            let error = parse(&reachable).unwrap_err();
            assert_eq!(error.code, "invalid_config", "{public_url}");
            assert!(
                error.message.contains("nomen or oidc"),
                "{public_url}: {}",
                error.message
            );
        }
        for public_url in ["http://localhost:8787", "http://[::1]:8787"] {
            let mut local = good();
            local["public_url"] = serde_json::json!(public_url);
            parse(&local).unwrap_or_else(|e| panic!("{public_url}: {}", e.message));
        }
    }

    #[test]
    fn unknown_fields_unknown_brokers_and_bad_names_fail_closed() {
        let mut unknown = good();
        let grants = unknown.as_object_mut().unwrap().remove("grants").unwrap();
        unknown["grant"] = grants;
        assert_eq!(parse(&unknown).unwrap_err().code, "invalid_config");

        let mut dangling = good();
        dangling["grants"][0]["brokers"] = serde_json::json!(["other"]);
        assert_eq!(parse(&dangling).unwrap_err().code, "invalid_config");

        let mut bad_name = good();
        bad_name["brokers"][0]["name"] = serde_json::json!("Gold WS");
        assert_eq!(parse(&bad_name).unwrap_err().code, "invalid_config");

        let mut relative = good();
        relative["brokers"][0]["socket"] = serde_json::json!("broker.sock");
        assert_eq!(parse(&relative).unwrap_err().code, "invalid_config");

        let mut both = good();
        both["brokers"][0]["link"] = serde_json::json!(true);
        assert_eq!(parse(&both).unwrap_err().code, "invalid_config");

        let mut neither = good();
        neither["brokers"][0]
            .as_object_mut()
            .unwrap()
            .remove("socket");
        assert_eq!(parse(&neither).unwrap_err().code, "invalid_config");

        let mut relative_tokens = good();
        relative_tokens["auth"]["tokens_file"] = serde_json::json!("tokens.json");
        assert_eq!(parse(&relative_tokens).unwrap_err().code, "invalid_config");
    }

    #[test]
    fn a_linked_broker_needs_no_socket() {
        let mut linked = good();
        linked["brokers"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({ "name": "gus", "link": true }));
        linked["grants"][0]["brokers"] = serde_json::json!(["gold-ws", "gus"]);
        let config = parse(&linked).unwrap();
        assert!(config.broker("gus").unwrap().link);
        assert_eq!(config.brokers_for("jesse").len(), 2);
    }

    #[test]
    fn oidc_requires_https_issuer_and_an_audience() {
        let mut oidc = good();
        oidc["auth"] = serde_json::json!({ "mode": "oidc", "issuer": "https://id.example", "audiences": ["hub"] });
        assert!(parse(&oidc).is_ok());

        let mut no_aud = oidc.clone();
        no_aud["auth"]["audiences"] = serde_json::json!([]);
        assert_eq!(parse(&no_aud).unwrap_err().code, "invalid_config");

        let mut plain = oidc.clone();
        plain["auth"]["issuer"] = serde_json::json!("http://id.example");
        assert_eq!(parse(&plain).unwrap_err().code, "invalid_config");
    }

    fn nomen() -> serde_json::Value {
        let mut config = good();
        config["auth"] = serde_json::json!({
            "mode": "nomen",
            "issuer": "https://id.shippin.cloud",
            "audiences": ["300000000000000500"],
            "clients": ["300000000000000600"],
            "key_file": TOKENS,
        });
        config["owner"] = serde_json::json!("300000000000000001");
        config["brokers"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({ "name": "gold-host", "link": true }));
        config
    }

    #[test]
    fn nomen_sign_in_needs_an_owner_who_reaches_every_broker() {
        let config = parse(&nomen()).unwrap();
        assert!(config.is_owner("300000000000000001"));
        let owned: Vec<_> = config
            .brokers_for("300000000000000001")
            .iter()
            .map(|b| b.name.as_str())
            .collect();
        assert_eq!(owned, ["gold-ws", "gold-host"]);
        assert_eq!(config.brokers_for("jesse").len(), 1);
        assert!(!config.is_owner("jesse"));

        let mut ownerless = nomen();
        ownerless.as_object_mut().unwrap().remove("owner");
        assert_eq!(parse(&ownerless).unwrap_err().code, "invalid_config");

        let mut no_key = nomen();
        no_key["auth"]["key_file"] = serde_json::json!("hub-key.json");
        assert_eq!(parse(&no_key).unwrap_err().code, "invalid_config");

        let mut no_aud = nomen();
        no_aud["auth"]["audiences"] = serde_json::json!([" "]);
        assert_eq!(parse(&no_aud).unwrap_err().code, "invalid_config");

        let mut any_client = nomen();
        any_client["auth"]["clients"] = serde_json::json!([]);
        assert_eq!(parse(&any_client).unwrap_err().code, "invalid_config");
    }

    #[test]
    fn web_sign_in_needs_nomen_an_accepted_client_and_https() {
        let mut web = nomen();
        web["auth"]["clients"] = serde_json::json!(["shippin-mcp", "devagotchi-web"]);
        web["web"] = serde_json::json!({ "client_id": "devagotchi-web" });
        assert_eq!(
            parse(&web).unwrap().web.unwrap().client_id,
            "devagotchi-web"
        );

        // The web app's tokens are checked like any other: it must be an accepted client.
        let mut unlisted = web.clone();
        unlisted["web"]["client_id"] = serde_json::json!("some-other-app");
        assert_eq!(parse(&unlisted).unwrap_err().code, "invalid_config");

        let mut plain = web.clone();
        plain["public_url"] = serde_json::json!("http://hub.example");
        assert_eq!(parse(&plain).unwrap_err().code, "invalid_config");
        plain["public_url"] = serde_json::json!("http://127.0.0.1:8787");
        assert!(parse(&plain).is_ok(), "plain http on loopback, for tests");

        let mut dev = good();
        dev["web"] = serde_json::json!({ "client_id": "devagotchi-web" });
        assert_eq!(parse(&dev).unwrap_err().code, "invalid_config");

        let mut extra = web;
        extra["web"]["secret"] = serde_json::json!("x");
        assert_eq!(parse(&extra).unwrap_err().code, "invalid_config");
    }

    #[test]
    fn an_email_or_a_name_is_never_an_identity() {
        let mut email_owner = nomen();
        email_owner["owner"] = serde_json::json!("jessay@gmail.com");
        let error = parse(&email_owner).unwrap_err();
        assert_eq!(error.code, "invalid_config");
        assert!(error.message.contains("account ID"), "{}", error.message);

        let mut email_grant = good();
        email_grant["grants"][0]["subject"] = serde_json::json!("jessay@gmail.com");
        assert_eq!(parse(&email_grant).unwrap_err().code, "invalid_config");

        let mut name_grant = good();
        name_grant["grants"][0]["subject"] = serde_json::json!("Pat Smith");
        assert_eq!(parse(&name_grant).unwrap_err().code, "invalid_config");
    }

    #[test]
    fn plain_http_issuers_only_on_loopback() {
        for issuer in [
            "http://127.0.0.1:9999",
            "http://localhost:1",
            "http://[::1]:2",
        ] {
            let mut local = nomen();
            local["auth"]["issuer"] = serde_json::json!(issuer);
            assert!(parse(&local).is_ok(), "{issuer}");
        }
        for issuer in [
            "http://id.shippin.cloud",
            "http://127.0.0.1.evil.example",
            "http://user@127.0.0.1:1",
            "ftp://127.0.0.1",
        ] {
            let mut remote = nomen();
            remote["auth"]["issuer"] = serde_json::json!(issuer);
            assert_eq!(
                parse(&remote).unwrap_err().code,
                "invalid_config",
                "{issuer}"
            );
        }
    }

    #[cfg(unix)]
    const PROJECT_DIR: &str = "/srv/projects/devagotchi";
    #[cfg(windows)]
    const PROJECT_DIR: &str = r"D:\projects\devagotchi";

    #[test]
    fn boards_are_granted_like_brokers_and_people_are_declared() {
        let mut with_boards = good();
        with_boards["boards"] =
            serde_json::json!([{ "project": "devagotchi", "dir": PROJECT_DIR }]);
        with_boards["grants"] = serde_json::json!([
            { "subject": "jesse", "brokers": ["gold-ws"], "boards": ["devagotchi"], "human": true },
            { "subject": "claude-on-gus", "boards": ["devagotchi"] },
            { "subject": "reader", "brokers": ["gold-ws"] }
        ]);
        let config = parse(&with_boards).unwrap();
        assert_eq!(config.boards_for("jesse").len(), 1);
        assert_eq!(config.boards_for("claude-on-gus").len(), 1);
        assert!(config.boards_for("reader").is_empty());
        assert!(config.boards_for("nobody").is_empty());
        assert!(config.is_human("jesse"));
        assert!(
            !config.is_human("claude-on-gus"),
            "an agent's grant never makes it a person"
        );
        assert!(
            config.brokers_for("claude-on-gus").is_empty(),
            "a board grant is not a broker grant"
        );
        assert_eq!(config.board("devagotchi").unwrap().dir, PROJECT_DIR);

        // A hub may serve boards alone, with no broker at all, or nothing yet.
        let mut boards_only = with_boards.clone();
        boards_only["brokers"] = serde_json::json!([]);
        boards_only["grants"] =
            serde_json::json!([{ "subject": "jesse", "boards": ["devagotchi"] }]);
        assert!(parse(&boards_only).is_ok());
        let mut nothing = boards_only.clone();
        nothing["boards"] = serde_json::json!([]);
        nothing["grants"] = serde_json::json!([]);
        let empty = parse(&nothing).unwrap();
        assert!(empty.brokers_for("jesse").is_empty() && empty.boards_for("jesse").is_empty());

        let mut dangling = with_boards.clone();
        dangling["grants"][1]["boards"] = serde_json::json!(["other"]);
        assert_eq!(parse(&dangling).unwrap_err().code, "invalid_config");

        let mut relative = with_boards.clone();
        relative["boards"][0]["dir"] = serde_json::json!("projects/devagotchi");
        assert_eq!(parse(&relative).unwrap_err().code, "invalid_config");

        let mut bad_name = with_boards.clone();
        bad_name["boards"][0]["project"] = serde_json::json!("Devagotchi Main");
        assert_eq!(parse(&bad_name).unwrap_err().code, "invalid_config");

        let mut twice = with_boards;
        twice["boards"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({ "project": "devagotchi", "dir": PROJECT_DIR }));
        assert_eq!(parse(&twice).unwrap_err().code, "invalid_config");
    }
}
