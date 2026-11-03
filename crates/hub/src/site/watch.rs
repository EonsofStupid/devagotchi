//! The running hub follows its site directory: a change to it is read and checked, and
//! served from the next request on, with no restart and no rebuild. A directory that no
//! longer checks out is refused the same way a start would refuse it, and the hub keeps
//! serving the site it had, saying why in its log.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::{Duration, UNIX_EPOCH},
};

use super::Site;

/// How often the directory is looked at, and how long a change must hold still (a copy in
/// progress is not a site yet).
pub const EVERY: Duration = Duration::from_secs(2);
const SETTLE: Duration = Duration::from_secs(1);

/// The site the hub serves now; every holder of the hub's config sees the same one.
pub struct SiteCell(RwLock<Arc<Site>>);

impl std::fmt::Debug for SiteCell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.get())
    }
}

impl SiteCell {
    pub fn new(site: Arc<Site>) -> Self {
        Self(RwLock::new(site))
    }

    pub fn get(&self) -> Arc<Site> {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set(&self, site: Arc<Site>) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = site;
    }
}

/// Every file under `dir`, with its size and modification time: what changed when this does.
fn fingerprint(dir: &Path) -> Vec<(PathBuf, u64, u128)> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0)];
    while let Some((at, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&at) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                if depth < 3 {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            out.push((path, meta.len(), modified));
        }
    }
    out.sort();
    out
}

/// Follow `dir` for as long as the hub runs, looking every `every`.
pub fn watch(cell: Arc<SiteCell>, dir: PathBuf, every: Duration) {
    // What the site in `cell` was read from, taken now: a change made while the thread
    // starts is still a change.
    let mut seen = fingerprint(&dir);
    let spawned = std::thread::Builder::new()
        .name("site-watch".into())
        .spawn(move || {
            loop {
                std::thread::sleep(every);
                let now = fingerprint(&dir);
                if now == seen {
                    continue;
                }
                std::thread::sleep(SETTLE.min(every));
                if fingerprint(&dir) != now {
                    continue;
                }
                seen = now;
                match Site::load(&dir) {
                    Ok(site) => {
                        eprintln!(
                            "devagotchi hub: site {} ({}) read again from {}",
                            site.name(),
                            site.info.mcp.name,
                            dir.display()
                        );
                        cell.set(Arc::new(site));
                    }
                    Err(why) => eprintln!(
                        "devagotchi hub: site.dir {}: {why}; still serving the site it had",
                        dir.display()
                    ),
                }
            }
        });
    if let Err(error) = spawned {
        eprintln!("devagotchi hub: cannot follow the site directory: {error}");
    }
}
