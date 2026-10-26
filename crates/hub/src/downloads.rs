//! The builds this hub hands to machines that join it (increment 29b): a directory laid
//! down at deploy from the GitHub build the hub itself runs, holding `VERSION` (one line),
//! `SHA256SUMS` (`<hex>  <file>` per line) and the files it lists. At start every listed
//! file is hashed and must match, so the hub never serves a byte its own list does not
//! vouch for; only listed names are served (`/dist/<file>`). `/join/<code>` writes those
//! sums into the script it serves, and the script checks each download against them.

use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};

use devagotchi_contracts::{BridgeError, BridgeResult};
use sha2::{Digest, Sha256};

pub struct Downloads {
    dir: PathBuf,
    pub version: String,
    sums: BTreeMap<String, String>,
}

fn invalid(dir: &Path, why: impl std::fmt::Display) -> BridgeError {
    BridgeError::new("invalid_downloads", format!("{}: {why}", dir.display()))
}

/// A file name the hub will serve: no paths, no hidden files, nothing odd.
pub fn servable(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// A version the join script may pass to the installers as is (their own rule).
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

fn file_sha256(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

impl Downloads {
    pub fn open(dir: &Path) -> BridgeResult<Self> {
        let read = |name: &str| {
            std::fs::read_to_string(dir.join(name))
                .map_err(|error| invalid(dir, format!("{name}: {error}")))
        };
        let version = read("VERSION")?.trim().to_string();
        if !valid_version(&version) {
            return Err(invalid(
                dir,
                format!("VERSION {version:?} is not a plain version"),
            ));
        }
        let mut sums = BTreeMap::new();
        for line in read("SHA256SUMS")?.lines().filter(|l| !l.trim().is_empty()) {
            let (sum, name) = line
                .split_once(char::is_whitespace)
                .map(|(sum, name)| {
                    (
                        sum.to_ascii_lowercase(),
                        name.trim().trim_start_matches('*'),
                    )
                })
                .ok_or_else(|| invalid(dir, format!("SHA256SUMS line {line:?}")))?;
            if sum.len() != 64 || !sum.bytes().all(|b| b.is_ascii_hexdigit()) || !servable(name) {
                return Err(invalid(dir, format!("SHA256SUMS line {line:?}")));
            }
            let actual = file_sha256(&dir.join(name))
                .map_err(|error| invalid(dir, format!("{name}: {error}")))?;
            if actual != sum {
                return Err(invalid(
                    dir,
                    format!("{name} has sha256 {actual}, SHA256SUMS says {sum}"),
                ));
            }
            sums.insert(name.to_string(), sum);
        }
        if sums.is_empty() {
            return Err(invalid(dir, "SHA256SUMS lists nothing"));
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            version,
            sums,
        })
    }

    /// The checksum of a file this hub serves, or `None` when it serves no such file.
    pub fn sha256(&self, name: &str) -> Option<&str> {
        self.sums.get(name).map(String::as_str)
    }

    /// Where a served file is, when it is one.
    pub fn path(&self, name: &str) -> Option<PathBuf> {
        self.sums.contains_key(name).then(|| self.dir.join(name))
    }

    pub fn files(&self) -> impl Iterator<Item = &str> {
        self.sums.keys().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lay_down(files: &[(&str, &[u8])], version: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let mut sums = String::new();
        for (name, bytes) in files {
            std::fs::write(dir.path().join(name), bytes).unwrap();
            sums.push_str(&format!("{}  {name}\n", hex::encode(Sha256::digest(bytes))));
        }
        std::fs::write(dir.path().join("SHA256SUMS"), sums).unwrap();
        std::fs::write(dir.path().join("VERSION"), format!("{version}\n")).unwrap();
        dir
    }

    #[test]
    fn listed_files_that_match_are_served_and_nothing_else() {
        let dir = lay_down(
            &[
                ("install-devagotchi.sh", b"#!/bin/sh\n"),
                ("devagotchi-x.tar.xz", b"xz"),
            ],
            "main-abc1234",
        );
        std::fs::write(dir.path().join("secret.txt"), b"not listed").unwrap();
        let downloads = Downloads::open(dir.path()).unwrap();
        assert_eq!(downloads.version, "main-abc1234");
        assert_eq!(
            downloads.sha256("devagotchi-x.tar.xz"),
            Some(hex::encode(Sha256::digest(b"xz")).as_str())
        );
        assert!(downloads.path("install-devagotchi.sh").is_some());
        for name in ["secret.txt", "SHA256SUMS", "VERSION", "../etc/passwd"] {
            assert!(downloads.path(name).is_none(), "{name}");
        }
    }

    #[test]
    fn a_file_that_does_not_match_its_sum_stops_the_hub_at_start() {
        let dir = lay_down(&[("devagotchi-x.tar.xz", b"xz")], "main-abc1234");
        std::fs::write(dir.path().join("devagotchi-x.tar.xz"), b"tampered").unwrap();
        assert_eq!(
            Downloads::open(dir.path()).err().unwrap().code,
            "invalid_downloads"
        );
        let odd = lay_down(&[("a", b"a")], "main abc; rm -rf /");
        assert!(
            Downloads::open(odd.path()).is_err(),
            "a version goes into a script as is"
        );
        let path = lay_down(&[("a", b"a")], "v1");
        std::fs::write(
            path.path().join("SHA256SUMS"),
            format!("{}  ../a\n", hex::encode(Sha256::digest(b"a"))),
        )
        .unwrap();
        assert!(
            Downloads::open(path.path()).is_err(),
            "no paths in SHA256SUMS"
        );
    }
}
