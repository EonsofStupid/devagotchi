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
/// accepted only for a loopback hub (tests, a hub on the same machine). The URL is parsed,
/// never pattern-matched: userinfo, queries and fragments are refused, so a URL such as
/// `http://localhost@evil.example` cannot pass for a loopback hub.
pub fn endpoint(hub: &str) -> BridgeResult<String> {
    let invalid = || BridgeError::new("invalid_cli", "hub must be the hub's https:// public URL");
    let uri: http::Uri = hub.parse().map_err(|_| invalid())?;
    let authority = uri.authority().ok_or_else(invalid)?;
    if authority.as_str().contains('@') || uri.query().is_some() || hub.contains('#') {
        return Err(invalid());
    }
    let host = authority
        .host()
        .trim_start_matches('[')
        .trim_end_matches(']');
    if host.is_empty() {
        return Err(invalid());
    }
    let path = uri.path().trim_end_matches('/');
    match uri.scheme_str() {
        Some("https") => Ok(format!("wss://{authority}{path}/link")),
        Some("http") if matches!(host, "127.0.0.1" | "localhost" | "::1") => {
            Ok(format!("ws://{authority}{path}/link"))
        }
        Some("http") => Err(BridgeError::new(
            "insecure_hub",
            "a remote hub must be https; plain http is allowed only on loopback",
        )),
        _ => Err(invalid()),
    }
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
        use std::os::unix::fs::DirBuilderExt;
        if !dir.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .map_err(|e| state_error("create state dir", e))?;
        }
        // The broker's own rule: every ancestor a real directory owned by you or root and
        // not writable by others, the directory itself yours with mode 0700.
        devagotchi_ipc::validate_private_directory(dir)?;
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
        // No ancestor may be a symlink or junction someone could re-point.
        for ancestor in dir.ancestors().skip(1) {
            if std::fs::symlink_metadata(ancestor).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(BridgeError::new(
                    "state_permissions",
                    format!(
                        "{} is a link; the state dir must sit under real directories",
                        ancestor.display()
                    ),
                ));
            }
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
    let text = read_private(&path)?;
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

/// Open the key file once without following links, check that very handle is a private
/// regular file, and read from it: nothing can swap the file between check and read.
fn read_private(path: &Path) -> BridgeResult<String> {
    use std::io::Read;
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|e| state_error("device.key", e))?;
        let metadata = file.metadata().map_err(|e| state_error("device.key", e))?;
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
        file
    };
    #[cfg(windows)]
    let mut file = {
        use devagotchi_ipc::security::{self, Entry, Operator};
        use std::os::windows::fs::OpenOptionsExt;
        /// FILE_FLAG_OPEN_REPARSE_POINT: open the entry itself, never a link's target.
        const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        let operator = Operator::current().map_err(|e| state_error("current user", e))?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(OPEN_REPARSE_POINT)
            .open(path)
            .map_err(|e| state_error("device.key", e))?;
        if let Some(defect) = security::private_entry_defect(&file, Entry::File, &operator)
            .map_err(|e| state_error("device.key", e))?
        {
            return Err(BridgeError::new(
                "state_permissions",
                format!("device.key {defect}"),
            ));
        }
        file
    };
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|e| state_error("device.key", e))?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp parent under its real path: on macOS the temp root sits under /var, a
    /// symlink to /private/var, and a state directory may not have a symlinked ancestor.
    fn temp_parent() -> (tempfile::TempDir, PathBuf) {
        let parent = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let real = std::fs::canonicalize(parent.path()).unwrap();
        #[cfg(windows)]
        let real = parent.path().to_path_buf();
        (parent, real)
    }

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
        // Userinfo cannot dress a remote host up as loopback, or hide where https goes.
        assert!(endpoint("http://localhost:1@evil.example").is_err());
        assert!(endpoint("http://127.0.0.1@evil.example/").is_err());
        assert!(endpoint("https://user@hub.example").is_err());
        assert!(endpoint("https://hub.example/?x=1").is_err());
        assert!(endpoint("https://hub.example/#frag").is_err());
    }

    #[test]
    fn the_key_is_created_once_privately_and_reloads_the_same() {
        let (_parent, real) = temp_parent();
        let dir = real.join("link-state");
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
            // A symlink in place of the key is refused, not followed.
            std::fs::remove_file(dir.join("device.key")).unwrap();
            std::os::unix::fs::symlink("/etc/hostname", dir.join("device.key")).unwrap();
            assert!(device_key(&dir).is_err());
        }
    }

    #[test]
    fn config_round_trips_and_rejects_bad_names() {
        let (_parent, real) = temp_parent();
        let dir = real.join("s");
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
