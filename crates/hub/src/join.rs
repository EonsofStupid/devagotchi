//! `/join/<code>`: the one line that adds a machine (increment 29b).
//!
//! "Add a machine" hands a person `curl -fsSL <hub>/join/<code> | sudo sh` (Linux) or
//! `irm <hub>/join/<code>.ps1 | iex` (Windows). The hub answers with a script made for that
//! code: the hub's URL, the machine's name, the build version, the SHA-256 of every file it
//! will download from this hub's `/dist`, and the code itself, which the script hands to the
//! installer on stdin (Linux) or in the environment (Windows), never on a command line.
//! Fetching the script does not spend the code; enrolling does, so a link preview cannot
//! burn it. A used, expired or unknown code gets a script that only says so. Every value
//! written into a script was checked against a strict pattern first, and goes in single
//! quotes, so nothing in it can become shell syntax.

use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};

use crate::{AppState, config::valid_name, downloads::Downloads};

const SH: &str = include_str!("join/join.sh");
const PS1: &str = include_str!("join/join.ps1");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Sh,
    PowerShell,
}

/// `dvg-` and 32 lowercase hex digits, as the hub issues them.
pub fn valid_code(code: &str) -> bool {
    code.len() == 36
        && code.starts_with("dvg-")
        && code[4..]
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A hub URL safe to put in single quotes: https (or loopback http, for tests), host,
/// optional port, nothing else.
fn safe_url(url: &str) -> bool {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"));
    rest.is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
    })
}

/// The script for one live code. `None` when a value could not be written safely.
pub fn script(
    shell: Shell,
    hub: &str,
    device: &str,
    code: &str,
    downloads: &Downloads,
) -> Option<String> {
    let hub = hub.trim_end_matches('/');
    if !safe_url(hub) || !valid_name(device) || !valid_code(code) {
        return None;
    }
    let sum = |name: &str| downloads.sha256(name).unwrap_or("").to_string();
    let replacements = [
        ("@HUB@", hub.to_string()),
        (
            "@PROTO@",
            if hub.starts_with("https://") {
                "=https"
            } else {
                "=http"
            }
            .to_string(),
        ),
        ("@DEVICE@", device.to_string()),
        ("@VERSION@", downloads.version.clone()),
        ("@CODE@", code.to_string()),
        ("@SHA_INSTALLER@", sum("install-devagotchi.sh")),
        ("@SHA_INSTALLER_PS1@", sum("install-devagotchi.ps1")),
        (
            "@SHA_LINUX_X86_64@",
            sum("devagotchi-x86_64-unknown-linux-gnu.tar.xz"),
        ),
        (
            "@SHA_LINUX_AARCH64@",
            sum("devagotchi-aarch64-unknown-linux-gnu.tar.xz"),
        ),
        (
            "@SHA_WINDOWS_X86_64@",
            sum("devagotchi-x86_64-pc-windows-msvc.zip"),
        ),
        (
            "@SHA_WINDOWS_AARCH64@",
            sum("devagotchi-aarch64-pc-windows-msvc.zip"),
        ),
    ];
    let installer = match shell {
        Shell::Sh => "install-devagotchi.sh",
        Shell::PowerShell => "install-devagotchi.ps1",
    };
    downloads.sha256(installer)?;
    let mut text = match shell {
        Shell::Sh => SH,
        Shell::PowerShell => PS1,
    }
    .to_string();
    for (placeholder, value) in replacements {
        text = text.replace(placeholder, &value);
    }
    Some(text)
}

/// What a dead or unknown code gets: a script that only says so.
fn refusal(shell: Shell, hub: &str) -> String {
    let message = format!(
        "devagotchi join: this join code was already used, has expired or is unknown. Get a new one under Add a machine at {}/account",
        hub.trim_end_matches('/')
    );
    match shell {
        Shell::Sh => format!("#!/bin/sh\necho '{message}' >&2\nexit 1\n"),
        Shell::PowerShell => format!("throw '{message}'\n"),
    }
}

fn text(status: StatusCode, body: String) -> Response {
    let mut response = (status, body).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

/// `/join/<code>` and `/dist/<file>`: open to anyone, since a machine joining has no
/// account yet; the code is what lets it in, and only enrolling spends it.
pub fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/join/{code}", axum::routing::get(join))
        .route("/dist/{name}", axum::routing::get(download))
}

pub async fn join(State(state): State<AppState>, Path(asked): Path<String>) -> Response {
    let (code, shell) = if let Some(code) = asked.strip_suffix(".compose.yml") {
        return compose(&state, code);
    } else if let Some(code) = asked.strip_suffix(".host.sh") {
        (code, Shell::Sh)
    } else if let Some(code) = asked.strip_suffix(".ps1") {
        (code, Shell::PowerShell)
    } else {
        (asked.strip_suffix(".sh").unwrap_or(&asked), Shell::Sh)
    };
    let hub = &state.hub.config.public_url;
    let Some(downloads) = state.hub.downloads.clone() else {
        return text(
            StatusCode::NOT_FOUND,
            "this hub hands out no builds\n".into(),
        );
    };
    if !valid_code(code) {
        return text(StatusCode::OK, refusal(shell, hub));
    }
    match state.hub.links.join_script(code, shell) {
        Ok(Some(device)) => match script(shell, hub, &device, code, &downloads) {
            Some(body) => text(StatusCode::OK, body),
            None => text(
                StatusCode::INTERNAL_SERVER_ERROR,
                "this hub could not write a join script for that machine\n".into(),
            ),
        },
        Ok(None) => text(StatusCode::OK, refusal(shell, hub)),
        Err(_) => text(
            StatusCode::SERVICE_UNAVAILABLE,
            "the hub could not check that code right now; run the same line again in a minute\n"
                .into(),
        ),
    }
}

/// Docker Compose that enrolls with the same single-use code. It does not invent an image:
/// the container runs the hub's Linux join script, which needs a Linux host with systemd
/// when the person wants a service. The file is the other shape of that one code.
fn compose(state: &AppState, code: &str) -> Response {
    let hub = state
        .hub
        .config
        .public_url
        .trim_end_matches('/')
        .to_string();
    if !valid_code(code) || !safe_url(&hub) {
        return text(StatusCode::OK, refusal(Shell::Sh, &hub));
    }
    text(
        StatusCode::OK,
        format!(
            "# Same single-use join code as the Linux one-liner.\n\
             # A host that should run Devagotchi under systemd should use that one-liner.\n\
             services:\n  enroll:\n    image: docker.io/library/debian:stable-slim\n    network_mode: host\n    volumes:\n      - devagotchi-state:/var/lib/devagotchi\n    command:\n      - sh\n      - -c\n      - apt-get update && apt-get install -y curl ca-certificates && curl -fsSL {hub}/join/{code} | sh\nvolumes:\n  devagotchi-state: {{}}\n"
        ),
    )
}

/// One file of the build this hub hands out, streamed; only names `SHA256SUMS` lists.
pub async fn download(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    if name == "SHA256SUMS"
        && let Some(downloads) = &state.hub.downloads
    {
        return text(StatusCode::OK, downloads.sha256sums_text());
    }
    let found = state
        .hub
        .downloads
        .as_ref()
        .and_then(|downloads| downloads.path(&name));
    let Some(path) = found else {
        return text(StatusCode::NOT_FOUND, "no such file\n".into());
    };
    let Ok(file) = tokio::fs::File::open(&path).await else {
        return text(StatusCode::NOT_FOUND, "no such file\n".into());
    };
    let length = file.metadata().await.map(|m| m.len()).ok();
    let chunks = futures_util::stream::unfold(file, |mut file| async move {
        use tokio::io::AsyncReadExt;
        let mut buffer = vec![0u8; 64 * 1024];
        match file.read(&mut buffer).await {
            Ok(0) => None,
            Ok(read) => {
                buffer.truncate(read);
                Some((
                    Ok::<_, std::io::Error>(axum::body::Bytes::from(buffer)),
                    file,
                ))
            }
            Err(error) => Some((Err(error), file)),
        }
    });
    let mut response = Response::new(Body::from_stream(chunks));
    let headers = response.headers_mut();
    let kind = if name.ends_with(".sh") || name.ends_with(".ps1") {
        "text/plain; charset=utf-8"
    } else {
        "application/octet-stream"
    };
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if let Some(length) = length {
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    }
    response
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::*;

    const CODE: &str = "dvg-0123456789abcdef0123456789abcdef";

    /// Whether a placeholder is left (`"$@"` in the shell script is not one).
    fn unfilled(text: &str) -> bool {
        [
            "@HUB@",
            "@PROTO@",
            "@DEVICE@",
            "@VERSION@",
            "@CODE@",
            "@SHA_",
        ]
        .iter()
        .any(|placeholder| text.contains(placeholder))
    }

    fn downloads(files: &[&str]) -> (tempfile::TempDir, Downloads) {
        let dir = tempfile::tempdir().unwrap();
        let mut sums = String::new();
        for name in files {
            std::fs::write(dir.path().join(name), name.as_bytes()).unwrap();
            sums.push_str(&format!(
                "{}  {name}\n",
                hex::encode(Sha256::digest(name.as_bytes()))
            ));
        }
        std::fs::write(dir.path().join("SHA256SUMS"), sums).unwrap();
        std::fs::write(dir.path().join("VERSION"), "main-9842d39\n").unwrap();
        let downloads = Downloads::open(dir.path()).unwrap();
        (dir, downloads)
    }

    #[test]
    fn a_script_carries_the_name_the_version_every_checksum_and_the_code() {
        let (_dir, downloads) = downloads(&[
            "install-devagotchi.sh",
            "install-devagotchi.ps1",
            "devagotchi-x86_64-unknown-linux-gnu.tar.xz",
            "devagotchi-x86_64-pc-windows-msvc.zip",
        ]);
        let sh = script(
            Shell::Sh,
            "https://app.devagotchi.com/",
            "platinum",
            CODE,
            &downloads,
        )
        .unwrap();
        assert!(!unfilled(&sh), "every placeholder is filled:\n{sh}");
        for wanted in [
            "hub='https://app.devagotchi.com'",
            "device='platinum'",
            "version='main-9842d39'",
            "--proto '=https'",
            &format!("printf '%s\\n' '{CODE}' | bash"),
            downloads
                .sha256("devagotchi-x86_64-unknown-linux-gnu.tar.xz")
                .unwrap(),
            downloads.sha256("install-devagotchi.sh").unwrap(),
        ] {
            assert!(sh.contains(wanted), "{wanted} missing:\n{sh}");
        }
        // No aarch64 build here: the script says so on such a machine instead of guessing.
        assert!(sh.contains("target=aarch64-unknown-linux-gnu; sum=''"));
        // The code reaches the installer on stdin only, never as an argument.
        assert_eq!(sh.matches(CODE).count(), 1);
        let ps = script(
            Shell::PowerShell,
            "https://app.devagotchi.com",
            "platinum",
            CODE,
            &downloads,
        )
        .unwrap();
        assert!(!unfilled(&ps), "every placeholder is filled:\n{ps}");
        assert!(ps.contains(&format!("$env:DEVAGOTCHI_ENROLL_CODE = '{CODE}'")));
        assert!(ps.contains(downloads.sha256("install-devagotchi.ps1").unwrap()));
        assert!(
            !ps.contains("-EnrollCode"),
            "the code never goes on a command line"
        );
    }

    #[test]
    fn nothing_unsafe_is_ever_written_into_a_script() {
        let (_dir, downloads) = downloads(&["install-devagotchi.sh"]);
        let hub = "https://app.devagotchi.com";
        for (hub, device, code) in [
            ("https://evil.example/'; rm -rf / #", "platinum", CODE),
            ("ftp://hub.example", "platinum", CODE),
            (hub, "Plat'inum", CODE),
            (hub, "platinum", "dvg-0123456789ABCDEF0123456789abcdef"),
            (hub, "platinum", "dvg-'$(id)'"),
        ] {
            assert!(
                script(Shell::Sh, hub, device, code, &downloads).is_none(),
                "{hub} {device} {code}"
            );
        }
        // A hub without the PowerShell installer writes no PowerShell script.
        assert!(script(Shell::PowerShell, hub, "platinum", CODE, &downloads).is_none());
        assert!(valid_code(CODE) && !valid_code("dvg-") && !valid_code(&format!("{CODE}0")));
    }

    #[cfg(unix)]
    #[test]
    fn the_shell_script_parses_and_refuses_a_machine_it_cannot_serve() {
        let (_dir, downloads) = downloads(&["install-devagotchi.sh"]);
        let sh = script(
            Shell::Sh,
            "http://127.0.0.1:9",
            "platinum",
            CODE,
            &downloads,
        )
        .unwrap();
        assert!(sh.contains("--proto '=http'"));
        let checked = std::process::Command::new("sh")
            .args(["-n", "-c", &sh])
            .status()
            .unwrap();
        assert!(checked.success(), "sh -n rejects the script");
        // As a non-root user it stops before downloading anything, and says how to run it.
        if unsafe_is_root() {
            return;
        }
        let out = std::process::Command::new("sh")
            .args(["-c", &sh])
            .output()
            .unwrap();
        assert!(!out.status.success());
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(
            said.contains("devagotchi join:") && !said.contains(CODE),
            "{said}"
        );
        let refused = refusal(Shell::Sh, "https://app.devagotchi.com");
        let out = std::process::Command::new("sh")
            .args(["-c", &refused])
            .output()
            .unwrap();
        assert!(
            !out.status.success() && String::from_utf8_lossy(&out.stderr).contains("Add a machine")
        );
    }

    #[cfg(unix)]
    fn unsafe_is_root() -> bool {
        std::process::Command::new("id")
            .arg("-u")
            .output()
            .is_ok_and(|out| out.stdout.starts_with(b"0"))
    }
}
