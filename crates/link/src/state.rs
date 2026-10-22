//! The link's private state: which hub, which device name, which local broker, and the
//! device's Ed25519 key. The key is generated here and never leaves this directory; the
//! directory and the key file are private to the operator, as the broker's state is.

use std::path::{Path, PathBuf};

use devagotchi_contracts::{BridgeError, BridgeResult};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkConfig {
    /// The hub's public URL exactly as the hub states it (its `public_url`).
    pub hub: String,
    /// The broker name this machine enrolls under on the hub.
    pub device: String,
    /// The local broker endpoint from its ready line: Unix socket or named pipe.
    pub broker: String,
}

fn state_error(context: &str, error: impl std::fmt::Display) -> BridgeError {
    BridgeError::new("link_state", format!("{context}: {error}"))
}

impl LinkConfig {
    pub fn validate(&self) -> BridgeResult<()> {
        endpoint(&self.hub)?;
        let name_ok = !self.device.is_empty()
            && self.device.len() <= 63
            && self.device.as_bytes()[0].is_ascii_alphanumeric()
            && self
                .device
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !name_ok {
            return Err(BridgeError::new(
                "invalid_cli",
                "device must be [a-z0-9][a-z0-9-]{0,62}",
            ));
        }
        if !Path::new(&self.broker).is_absolute() {
            return Err(BridgeError::new(
                "invalid_cli",
                "broker must be the absolute endpoint on the broker's ready line",
            ));
        }
        Ok(())
    }
}

/// The WebSocket endpoint for a hub public URL: `https` becomes `wss`, plain `http` is
/// accepted only for a loopback hub (tests, a hub on the same machine).
pub fn endpoint(hub: &str) -> BridgeResult<String> {
    let base = hub.trim_end_matches('/');
    if let Some(rest) = base.strip_prefix("https://") {
        if !rest.is_empty() {
            return Ok(format!("wss://{rest}/link"));
        }
    } else if let Some(rest) = base.strip_prefix("http://") {
        let host = rest.split('/').next().unwrap_or_default();
        let host = match host.strip_prefix('[') {
            Some(v6) => v6.split(']').next().unwrap_or_default(),
            None => host.split(':').next().unwrap_or_default(),
        };
        if matches!(host, "127.0.0.1" | "localhost" | "::1") {
            return Ok(format!("ws://{rest}/link"));
        }
        return Err(BridgeError::new(
            "insecure_hub",
            "a remote hub must be https; plain http is allowed only on loopback",
        ));
    }
    Err(BridgeError::new(
        "invalid_cli",
        "hub must be the hub's https:// public URL",
    ))
}

/// Create the state directory private to the operator, or check an existing one is.
pub fn prepare(dir: &Path) -> BridgeResult<()> {
    if !dir.is_absolute() {
        return Err(BridgeError::new(
            "invalid_cli",
            "--state-dir must be absolute",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        if !dir.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .map_err(|e| state_error("create state dir", e))?;
        }
        let metadata = std::fs::symlink_metadata(dir).map_err(|e| state_error("state dir", e))?;
        if !metadata.is_dir()
            || metadata.uid() != devagotchi_ipc::current_uid()
            || metadata.mode() & 0o077 != 0
        {
            return Err(BridgeError::new(
                "state_permissions",
                "state dir must be a directory owned by you with mode 0700",
            ));
        }
    }
    #[cfg(windows)]
    {
        use devagotchi_ipc::security::{self, Entry, Operator};
        let operator = Operator::current().map_err(|e| state_error("current user", e))?;
        if !dir.exists() {
            if let Some(parent) = dir.parent() {
                std::fs::create_dir_all(parent).map_err(|e| state_error("create parent", e))?;
            }
            security::create_private_directory(dir, &operator.user)
                .map_err(|e| state_error("create state dir", e))?;
        }
        let entry = security::open_entry(dir).map_err(|e| state_error("state dir", e))?;
        if let Some(defect) = security::private_entry_defect(&entry, Entry::Directory, &operator)
            .map_err(|e| state_error("state dir", e))?
        {
            return Err(BridgeError::new(
                "state_permissions",
                format!("state dir {defect}"),
            ));
        }
    }
    Ok(())
}

fn config_path(dir: &Path) -> PathBuf {
    dir.join("link.json")
}

fn key_path(dir: &Path) -> PathBuf {
    dir.join("device.key")
}

pub fn save_config(dir: &Path, config: &LinkConfig) -> BridgeResult<()> {
    let text = serde_json::to_string_pretty(config).map_err(|e| state_error("config", e))?;
    std::fs::write(config_path(dir), text).map_err(|e| state_error("write link.json", e))
}

pub fn load_config(dir: &Path) -> BridgeResult<LinkConfig> {
    let text = std::fs::read_to_string(config_path(dir)).map_err(|e| {
        BridgeError::new(
            "not_enrolled",
            format!(
                "{}: {e}; run devagotchi-link enroll first",
                config_path(dir).display()
            ),
        )
    })?;
    let config: LinkConfig =
        serde_json::from_str(&text).map_err(|e| state_error("link.json", e))?;
    config.validate()?;
    Ok(config)
}

/// The device key, created on first use. Only its public half ever leaves this machine.
pub fn device_key(dir: &Path) -> BridgeResult<SigningKey> {
    let path = key_path(dir);
    if !path.exists() {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|e| state_error("random", e))?;
        write_private(&path, hex::encode(seed).as_bytes())?;
    }
    check_private_file(&path)?;
    let text = std::fs::read_to_string(&path).map_err(|e| state_error("device.key", e))?;
    let seed: [u8; 32] = hex::decode(text.trim())
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| BridgeError::new("link_state", "device.key is not a 32-byte hex seed"))?;
    Ok(SigningKey::from_bytes(&seed))
}

fn write_private(path: &Path, bytes: &[u8]) -> BridgeResult<()> {
    #[cfg(unix)]
    {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| state_error("create device.key", e))?;
        file.write_all(bytes)
            .map_err(|e| state_error("write device.key", e))
    }
    #[cfg(windows)]
    {
        use devagotchi_ipc::security::{self, Operator};
        let operator = Operator::current().map_err(|e| state_error("current user", e))?;
        security::create_private_file(path, &operator.user)
            .map_err(|e| state_error("create device.key", e))?;
        std::fs::write(path, bytes).map_err(|e| state_error("write device.key", e))
    }
}

fn check_private_file(path: &Path) -> BridgeResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(path).map_err(|e| state_error("device.key", e))?;
        if !metadata.is_file()
            || metadata.uid() != devagotchi_ipc::current_uid()
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err(BridgeError::new(
                "state_permissions",
                "device.key must be a regular file owned by you with mode 0600",
            ));
        }
    }
    #[cfg(windows)]
    {
        use devagotchi_ipc::security::{self, Entry, Operator};
        let operator = Operator::current().map_err(|e| state_error("current user", e))?;
        let entry = security::open_entry(path).map_err(|e| state_error("device.key", e))?;
        if let Some(defect) = security::private_entry_defect(&entry, Entry::File, &operator)
            .map_err(|e| state_error("device.key", e))?
        {
            return Err(BridgeError::new(
                "state_permissions",
                format!("device.key {defect}"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_require_tls_except_on_loopback() {
        assert_eq!(
            endpoint("https://mcp.shippin.cloud").unwrap(),
            "wss://mcp.shippin.cloud/link"
        );
        assert_eq!(
            endpoint("https://example.net/devagotchi/").unwrap(),
            "wss://example.net/devagotchi/link"
        );
        assert_eq!(
            endpoint("http://127.0.0.1:8787").unwrap(),
            "ws://127.0.0.1:8787/link"
        );
        assert_eq!(endpoint("http://[::1]:9").unwrap(), "ws://[::1]:9/link");
        assert_eq!(
            endpoint("http://hub.example").unwrap_err().code,
            "insecure_hub"
        );
        assert!(endpoint("ftp://x").is_err());
        assert!(endpoint("https://").is_err());
    }

    #[test]
    fn the_key_is_created_once_privately_and_reloads_the_same() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("link-state");
        prepare(&dir).unwrap();
        let first = device_key(&dir).unwrap();
        let again = device_key(&dir).unwrap();
        assert_eq!(first.verifying_key(), again.verifying_key());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("device.key"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
            std::fs::set_permissions(
                dir.join("device.key"),
                std::fs::Permissions::from_mode(0o644),
            )
            .unwrap();
            assert_eq!(device_key(&dir).unwrap_err().code, "state_permissions");
        }
    }

    #[test]
    fn config_round_trips_and_rejects_bad_names() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("s");
        prepare(&dir).unwrap();
        #[cfg(unix)]
        let broker = "/run/broker.sock";
        #[cfg(windows)]
        let broker = r"\\.\pipe\devagotchi-broker-x";
        let config = LinkConfig {
            hub: "https://mcp.shippin.cloud".into(),
            device: "gus".into(),
            broker: broker.into(),
        };
        config.validate().unwrap();
        save_config(&dir, &config).unwrap();
        assert_eq!(load_config(&dir).unwrap(), config);
        let bad = LinkConfig {
            device: "Gus PC".into(),
            ..config
        };
        assert!(bad.validate().is_err());
    }
}
