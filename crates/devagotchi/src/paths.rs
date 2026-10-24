//! Where this user's Devagotchi lives when nothing says otherwise: the XDG directories
//! on Unix (`~/.local/state/devagotchi`, `~/.config/devagotchi`), the per-user app
//! directories on Windows (`%LOCALAPPDATA%\devagotchi\state`, `%APPDATA%\devagotchi`).
//! `DEVAGOTCHI_STATE_DIR` and `DEVAGOTCHI_CONFIG_DIR` override both, for tests and for
//! people who keep their state elsewhere; the flags on each command override everything.

use std::{env, ffi::OsString, path::PathBuf};

pub const STATE_ENV: &str = "DEVAGOTCHI_STATE_DIR";
pub const CONFIG_ENV: &str = "DEVAGOTCHI_CONFIG_DIR";

/// One environment variable, as the process sees it.
pub type Lookup<'a> = &'a dyn Fn(&str) -> Option<OsString>;

fn absolute(lookup: Lookup<'_>, name: &str) -> Option<PathBuf> {
    lookup(name).map(PathBuf::from).filter(|p| p.is_absolute())
}

/// The user's home directory, or none when the environment does not say.
pub fn home_in(lookup: Lookup<'_>) -> Option<PathBuf> {
    absolute(lookup, "HOME").or_else(|| absolute(lookup, "USERPROFILE"))
}

/// `<state>/`: the broker's journal, socket and `endpoint` file.
pub fn state_dir_in(lookup: Lookup<'_>) -> Option<PathBuf> {
    if let Some(dir) = absolute(lookup, STATE_ENV) {
        return Some(dir);
    }
    if cfg!(windows) {
        return absolute(lookup, "LOCALAPPDATA").map(|d| d.join("devagotchi").join("state"));
    }
    absolute(lookup, "XDG_STATE_HOME")
        .or_else(|| home_in(lookup).map(|h| h.join(".local").join("state")))
        .map(|d| d.join("devagotchi"))
}

/// `<config>/`: `broker.json`, and later the sign-in.
pub fn config_dir_in(lookup: Lookup<'_>) -> Option<PathBuf> {
    if let Some(dir) = absolute(lookup, CONFIG_ENV) {
        return Some(dir);
    }
    if cfg!(windows) {
        return absolute(lookup, "APPDATA").map(|d| d.join("devagotchi"));
    }
    absolute(lookup, "XDG_CONFIG_HOME")
        .or_else(|| home_in(lookup).map(|h| h.join(".config")))
        .map(|d| d.join("devagotchi"))
}

pub fn home() -> Option<PathBuf> {
    home_in(&|k| env::var_os(k))
}

pub fn state_dir() -> Option<PathBuf> {
    state_dir_in(&|k| env::var_os(k))
}

pub fn config_dir() -> Option<PathBuf> {
    config_dir_in(&|k| env::var_os(k))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, OsString> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn xdg_then_home_then_the_override() {
        let home = env(&[("HOME", "/home/pat")]);
        let look = |k: &str| home.get(k).cloned();
        assert_eq!(
            state_dir_in(&look),
            Some(PathBuf::from("/home/pat/.local/state/devagotchi"))
        );
        assert_eq!(
            config_dir_in(&look),
            Some(PathBuf::from("/home/pat/.config/devagotchi"))
        );
        let xdg = env(&[("HOME", "/home/pat"), ("XDG_STATE_HOME", "/var/state")]);
        let look = |k: &str| xdg.get(k).cloned();
        assert_eq!(
            state_dir_in(&look),
            Some(PathBuf::from("/var/state/devagotchi"))
        );
        let over = env(&[
            ("HOME", "/home/pat"),
            (STATE_ENV, "/srv/dvg"),
            (CONFIG_ENV, "relative/never"),
        ]);
        let look = |k: &str| over.get(k).cloned();
        assert_eq!(state_dir_in(&look), Some(PathBuf::from("/srv/dvg")));
        assert_eq!(
            config_dir_in(&look),
            Some(PathBuf::from("/home/pat/.config/devagotchi")),
            "a relative override is ignored"
        );
        let nothing = env(&[]);
        let look = |k: &str| nothing.get(k).cloned();
        assert_eq!(state_dir_in(&look), None);
    }

    #[cfg(windows)]
    #[test]
    fn the_app_directories_on_windows() {
        let vars = env(&[
            ("USERPROFILE", r"C:\Users\pat"),
            ("LOCALAPPDATA", r"C:\Users\pat\AppData\Local"),
            ("APPDATA", r"C:\Users\pat\AppData\Roaming"),
        ]);
        let look = |k: &str| vars.get(k).cloned();
        assert_eq!(
            state_dir_in(&look),
            Some(PathBuf::from(
                r"C:\Users\pat\AppData\Local\devagotchi\state"
            ))
        );
        assert_eq!(
            config_dir_in(&look),
            Some(PathBuf::from(r"C:\Users\pat\AppData\Roaming\devagotchi"))
        );
    }
}
