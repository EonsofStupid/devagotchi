//! Starting a process detached from this one: what `devagotchi mcp` uses to start this
//! user's broker in the background when none answers. The native mechanics live in
//! `platform`; this module holds what is the same everywhere.
//!
//! The started process
//! - is in a session of its own (Linux) or has no console and a process group of its own,
//!   outside the caller's job where the job allows it (Windows), so closing the caller, its
//!   terminal or its process tree leaves it running;
//! - inherits nothing the caller had open but its own stdin (the null device) and its log,
//!   so an AI client's pipes are never held open by it;
//! - runs in the directory and with the environment it is given (see [`environment`]), not
//!   whichever the caller happened to have.

use std::{
    ffi::{OsStr, OsString},
    io,
    path::Path,
};

use super::platform;

/// What to start.
pub struct DetachedSpec<'a> {
    pub program: &'a Path,
    pub args: &'a [OsString],
    pub env: &'a [(OsString, OsString)],
    pub cwd: &'a Path,
    /// Its stdout and stderr, appended to; created owner-only.
    pub log: &'a Path,
}

/// A process started detached. Dropping it neither stops nor waits for the process.
pub struct Detached(platform::DetachedChild);

impl Detached {
    /// The exit code once the process has ended; `None` while it runs.
    pub fn try_wait(&mut self) -> io::Result<Option<i32>> {
        self.0.try_wait()
    }
}

/// Start `spec` detached from this process.
pub fn spawn(spec: &DetachedSpec<'_>) -> io::Result<Detached> {
    platform::spawn_detached(spec).map(Detached)
}

/// The variables a long-running background process keeps from the environment it was
/// started in: what programs need to find their home, their temporary directory, their
/// tools and the system, and what jobs need to reach the network, git hosts and their
/// toolchains as they would from a broker started in a terminal (the SSH agent, proxies,
/// extra CA certificates, toolchain homes), and nothing an AI client happened to put in its
/// own (tokens in an MCP server's `env` block, say). A broker started in a terminal keeps
/// that terminal's whole environment; one started in the background keeps only these, and
/// so do its jobs. Names compare case-insensitively on Windows.
pub fn environment(
    from: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    from.into_iter().filter(|(name, _)| kept(name)).collect()
}

const KEPT: &[&str] = &[
    // Everywhere.
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LANGUAGE",
    "TZ",
    "TMPDIR",
    "SHELL",
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_STATE_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    // Where this user's Devagotchi lives. Not every DEVAGOTCHI_ variable: who a client acts
    // as (DEVAGOTCHI_ACTOR) or a one-time enrollment code is that client's alone, and the
    // broker serves every client.
    "DEVAGOTCHI_STATE_DIR",
    "DEVAGOTCHI_CONFIG_DIR",
    "DEVAGOTCHI_BROKER_CONFIG",
    // The SSH agent, proxies and extra CA certificates (a proxy URL may carry a password;
    // a terminal start passes it on just the same).
    "SSH_AUTH_SOCK",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    "GIT_SSL_CAINFO",
    // Toolchain homes.
    "CARGO_HOME",
    "RUSTUP_HOME",
    "GOPATH",
    "GOROOT",
    "GOBIN",
    "GOMODCACHE",
    "JAVA_HOME",
    "NVM_DIR",
    "NVM_HOME",
    "NVM_SYMLINK",
    "PNPM_HOME",
    "VOLTA_HOME",
    "BUN_INSTALL",
    "PYENV_ROOT",
    // Windows.
    "SystemRoot",
    "SystemDrive",
    "windir",
    "ComSpec",
    "PATHEXT",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "USERNAME",
    "USERDOMAIN",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "CommonProgramFiles",
    "CommonProgramFiles(x86)",
    "CommonProgramW6432",
    "PUBLIC",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "OS",
];

fn kept(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let same = |known: &str| {
        if cfg!(windows) {
            known.eq_ignore_ascii_case(name)
        } else {
            known == name
        }
    };
    KEPT.iter().any(|known| same(known)) || name.starts_with("LC_")
}

#[cfg_attr(not(windows), allow(dead_code))]
/// One argument appended to a Windows command line so that `CommandLineToArgvW` and the
/// Microsoft C runtime read it back exactly: quoted when it has to be, with the
/// backslashes before a quote, or before the closing quote, doubled.
pub(crate) fn append_windows_argument(out: &mut Vec<u16>, argument: &[u16]) {
    const QUOTE: u16 = b'"' as u16;
    const BACKSLASH: u16 = b'\\' as u16;
    let quoted = argument.is_empty()
        || argument.iter().any(|&c| {
            c == b' ' as u16 || c == b'\t' as u16 || c == b'\n' as u16 || c == 0x0b || c == QUOTE
        });
    if !quoted {
        out.extend_from_slice(argument);
        return;
    }
    out.push(QUOTE);
    let mut backslashes = 0;
    for &c in argument {
        if c == BACKSLASH {
            backslashes += 1;
        } else {
            if c == QUOTE {
                out.extend(std::iter::repeat_n(BACKSLASH, backslashes + 1));
            }
            backslashes = 0;
        }
        out.push(c);
    }
    out.extend(std::iter::repeat_n(BACKSLASH, backslashes));
    out.push(QUOTE);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(arguments: &[&str]) -> String {
        let mut out = Vec::new();
        for (i, argument) in arguments.iter().enumerate() {
            if i > 0 {
                out.push(b' ' as u16);
            }
            append_windows_argument(&mut out, &argument.encode_utf16().collect::<Vec<_>>());
        }
        String::from_utf16(&out).unwrap()
    }

    #[test]
    fn windows_arguments_survive_the_c_runtime_parser() {
        // The cases from "Everyone quotes command line arguments the wrong way".
        assert_eq!(line(&["start", "--state-dir"]), "start --state-dir");
        assert_eq!(line(&[""]), "\"\"");
        assert_eq!(
            line(&[r"C:\Users\Pat Smith\AppData\Local\devagotchi\state"]),
            r#""C:\Users\Pat Smith\AppData\Local\devagotchi\state""#
        );
        assert_eq!(line(&[r"C:\no spaces\"]), r#""C:\no spaces\\""#);
        assert_eq!(line(&[r"C:\plain\"]), r"C:\plain\");
        assert_eq!(line(&[r#"say "hi""#]), r#""say \"hi\"""#);
        assert_eq!(line(&[r#"a\"b"#]), r#""a\\\"b""#);
        assert_eq!(line(&["tab\there"]), "\"tab\there\"");
    }

    #[test]
    fn only_what_a_background_process_needs_is_kept() {
        let kept = environment([
            ("PATH".into(), "/usr/bin".into()),
            ("HOME".into(), "/home/pat".into()),
            ("LC_ALL".into(), "C".into()),
            ("DEVAGOTCHI_STATE_DIR".into(), "/s".into()),
            ("GITHUB_TOKEN".into(), "secret".into()),
            ("ANTHROPIC_API_KEY".into(), "secret".into()),
            ("CLAUDECODE".into(), "1".into()),
            // What jobs need to reach the network, git hosts and their toolchains, as
            // they would from a broker started in a terminal.
            ("SSH_AUTH_SOCK".into(), "/run/user/1000/ssh".into()),
            ("HTTPS_PROXY".into(), "http://proxy:3128".into()),
            ("no_proxy".into(), "localhost".into()),
            ("SSL_CERT_FILE".into(), "/etc/ca.pem".into()),
            ("CARGO_HOME".into(), "/home/pat/.cargo".into()),
            ("GOPATH".into(), "/home/pat/go".into()),
            ("AWS_SECRET_ACCESS_KEY".into(), "secret".into()),
            // Who a client acts as, and a one-time code, are that client's alone: a broker
            // serves every client, so its jobs must not act as the one that started it.
            ("DEVAGOTCHI_ACTOR".into(), "claude".into()),
            ("DEVAGOTCHI_ENROLL_CODE".into(), "one-time".into()),
            ("DEVAGOTCHI_BROKER_CONFIG".into(), "/c/broker.json".into()),
        ]);
        let names: Vec<_> = kept.iter().map(|(n, _)| n.to_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "PATH",
                "HOME",
                "LC_ALL",
                "DEVAGOTCHI_STATE_DIR",
                "SSH_AUTH_SOCK",
                "HTTPS_PROXY",
                "no_proxy",
                "SSL_CERT_FILE",
                "CARGO_HOME",
                "GOPATH",
                "DEVAGOTCHI_BROKER_CONFIG",
            ]
        );
    }
}
