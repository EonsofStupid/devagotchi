//! The link updates its machine to the hub's build (docs/spec-machine-update.md, U3): wait
//! until the broker runs no jobs (unless forced), download the installer and this machine's
//! archive from the hub, check both against the hub's `SHA256SUMS`, then hand off to the
//! platform's updater (`handoff.rs`), which outlives this link and the broker it replaces.
//! The new link reports how it went from the record left here (`outcome`).

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use devagotchi_contracts::LinkFrame;
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use crate::{
    handoff,
    idle::{busy_operations, wait_for_idle},
    state::LinkConfig,
};

/// How often a waiting update looks at the broker's jobs again, and when it stops waiting.
const RECHECK: Duration = Duration::from_secs(30);
const GIVE_UP: Duration = Duration::from_secs(6 * 60 * 60);
/// A handed-off update that has not restarted this link by then did not happen.
const HANDOFF_LIMIT: Duration = Duration::from_secs(10 * 60);

/// Where an update stands, as `LinkFrame::UpdateStatus` says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub state: &'static str,
    pub detail: Option<String>,
}

impl Status {
    pub fn frame(&self) -> LinkFrame {
        LinkFrame::UpdateStatus {
            state: self.state.to_string(),
            detail: self.detail.clone(),
        }
    }
}

/// This machine's one updater: at most one update at a time, its status watched by
/// whichever session is connected.
pub struct Updater {
    hub: String,
    broker: PathBuf,
    state_dir: PathBuf,
    build: String,
    status: watch::Sender<Option<Status>>,
    running: Mutex<bool>,
}

impl Updater {
    pub fn new(config: &LinkConfig, state_dir: &Path, build: &str) -> Arc<Self> {
        Arc::new(Self {
            hub: config.hub.clone(),
            broker: PathBuf::from(&config.broker),
            state_dir: state_dir.to_path_buf(),
            build: build.to_string(),
            status: watch::Sender::new(None),
            running: Mutex::new(false),
        })
    }

    pub fn build(&self) -> &str {
        &self.build
    }

    pub fn subscribe(&self) -> watch::Receiver<Option<Status>> {
        self.status.subscribe()
    }

    fn set(&self, state: &'static str, detail: Option<String>) {
        let next = Some(Status { state, detail });
        self.status.send_if_modified(|now| {
            let changed = *now != next;
            *now = next.clone();
            changed
        });
    }

    /// Start one update to `version`. While one runs, a second request starts nothing and
    /// is answered with the current status instead.
    pub fn request(self: &Arc<Self>, version: String, force: bool) -> Option<Status> {
        {
            let mut running = self.running.lock().expect("updater");
            if *running {
                return self.status.borrow().clone();
            }
            *running = true;
        }
        let this = self.clone();
        tokio::spawn(async move {
            match this.run(&version, force).await {
                Ok(true) => {
                    // Handed off: the updater stops this link. If it never does, say so.
                    tokio::time::sleep(HANDOFF_LIMIT).await;
                    let _ = handoff::pending(&this.state_dir, None);
                    this.set(
                        "failed",
                        Some("the updater did not restart this link".into()),
                    );
                }
                Ok(false) => {}
                Err(detail) => this.set("failed", Some(detail)),
            }
            *this.running.lock().expect("updater") = false;
        });
        None
    }

    /// Ok(true) once handed off, Ok(false) when there was nothing to do.
    async fn run(&self, version: &str, force: bool) -> Result<bool, String> {
        if !valid_version(version) {
            return Err(format!("{version:?} is not a plain version"));
        }
        if version == self.build {
            self.set("done", None);
            return Ok(false);
        }
        if !force {
            let broker = self.broker.clone();
            wait_for_idle(
                || busy_operations(broker.clone()),
                RECHECK,
                GIVE_UP,
                || self.set("waiting_for_jobs", None),
            )
            .await?;
        }
        self.set("downloading", None);
        let dir = self.download(version).await?;
        self.set("installing", None);
        handoff::pending(&self.state_dir, Some(version))?;
        if let Err(why) = handoff::start(&self.state_dir, &dir, version) {
            let _ = handoff::pending(&self.state_dir, None);
            return Err(why);
        }
        Ok(true)
    }

    /// The installer and this machine's archive in `<state>/updates/<version>`, each checked
    /// against the hub's list, with the archive's `.sha256` the installers check in turn. On
    /// any mismatch nothing is left there.
    async fn download(&self, version: &str) -> Result<PathBuf, String> {
        let (archive, installer) =
            handoff::files().ok_or("the hub has no build for this machine")?;
        let base = format!("{}/dist", self.hub.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(15 * 60))
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        let sums = String::from_utf8_lossy(fetch(&client, &base, "SHA256SUMS").await?.as_ref())
            .into_owned();
        let dir = self.state_dir.join("updates").join(version);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        let fetched = async {
            for name in [installer, archive.as_str()] {
                let expected = listed_sum(&sums, name)
                    .ok_or_else(|| format!("{name} is not in the hub's SHA256SUMS"))?;
                let actual = fetch(&client, &base, name).await.and_then(|bytes| {
                    let actual = hex::encode(Sha256::digest(bytes.as_ref()));
                    std::fs::write(dir.join(name), bytes.as_ref())
                        .map_err(|e| format!("write {name}: {e}"))?;
                    Ok(actual)
                })?;
                if actual != expected {
                    return Err(format!(
                        "{name} has sha256 {actual}, the hub's SHA256SUMS says {expected}"
                    ));
                }
                if name == archive {
                    std::fs::write(
                        dir.join(format!("{name}.sha256")),
                        format!("{actual}  {name}\n"),
                    )
                    .map_err(|e| format!("write {name}.sha256: {e}"))?;
                }
            }
            Ok(())
        };
        match fetched.await {
            Ok(()) => Ok(dir),
            Err(why) => {
                let _ = std::fs::remove_dir_all(&dir);
                Err(why)
            }
        }
    }
}

async fn fetch(
    client: &reqwest::Client,
    base: &str,
    name: &str,
) -> Result<impl AsRef<[u8]> + use<>, String> {
    let failed = |e: reqwest::Error| format!("download {name}: {e}");
    let response = client
        .get(format!("{base}/{name}"))
        .send()
        .await
        .map_err(failed)?;
    let bytes = response
        .error_for_status()
        .map_err(failed)?
        .bytes()
        .await
        .map_err(failed)?;
    Ok(bytes)
}

fn listed_sum(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (sum, file) = line.split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name).then(|| sum.to_ascii_lowercase())
    })
}

/// The hub's own rule for a version it hands out (`downloads.rs`): it names directories
/// and goes on an installer's command line.
fn valid_version(version: &str) -> bool {
    version.len() <= 64
        && version
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}
