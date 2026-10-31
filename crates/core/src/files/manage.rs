//! Managing files: what a path is (`info`), making a directory (`mkdir`) and moving or
//! renaming (`moved`). Reach is the read policy: inside the workspace under `granted`,
//! wherever the account can under `account`. A symbolic link is reported and moved as
//! itself; under `granted` what it points to must be inside the workspace too.

use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use devagotchi_contracts::{
    BridgeError, BridgeResult, DirectoryInput, FileInfo, FileInfoInput, FileMoveInput,
};

use super::{Scope, display, invalid, io_error, modified_ms, text_prefix};
use crate::broker::ExecPolicy;

/// Largest text file whose lines `info` counts.
const MAX_COUNTED_BYTES: u64 = 8 * 1024 * 1024;

fn outside() -> BridgeError {
    BridgeError::new(
        "path_outside_workspace",
        "This machine's broker reaches files only inside its workspace",
    )
}

fn created_ms(metadata: &Metadata) -> Option<u64> {
    let since = metadata.created().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(since.as_millis().min(u64::MAX as u128) as u64)
}

fn kind(metadata: &Metadata) -> &'static str {
    match metadata.file_type() {
        t if t.is_symlink() => "symlink",
        t if t.is_dir() => "dir",
        t if t.is_file() => "file",
        _ => "other",
    }
}

#[cfg(unix)]
fn mode(metadata: &Metadata) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    Some(format!("{:04o}", metadata.permissions().mode() & 0o7777))
}

#[cfg(not(unix))]
fn mode(_: &Metadata) -> Option<String> {
    None
}

/// Lines in a text file small enough to count; `None` for anything else.
fn lines(path: &Path, metadata: &Metadata) -> Option<u64> {
    if !metadata.is_file() || metadata.len() > MAX_COUNTED_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    if text_prefix(&bytes)? != bytes.len() {
        return None;
    }
    let breaks = bytes.iter().filter(|b| **b == b'\n').count() as u64;
    Some(breaks + u64::from(bytes.last().is_some_and(|b| *b != b'\n')))
}

impl Scope<'_> {
    /// `path` canonical up to its own name, without following the name itself: the
    /// directory it is in, resolved, joined with its name. The directory must exist.
    fn place(&self, path: &str) -> BridgeResult<PathBuf> {
        let given = Path::new(path);
        if !given.is_absolute() || path.as_bytes().contains(&0) {
            return Err(invalid("path must be an absolute path without NUL bytes"));
        }
        let (Some(parent), Some(name)) = (given.parent(), given.file_name()) else {
            return Err(invalid("path must name an entry, not a root"));
        };
        let place = dunce::canonicalize(parent)
            .map_err(|e| io_error(path, e))?
            .join(name);
        if self.policy == ExecPolicy::Granted && !place.starts_with(self.workspace) {
            return Err(outside());
        }
        Ok(place)
    }

    /// Under `granted`, a link must point inside the workspace for the broker to touch it.
    fn link_inside(&self, path: &Path, metadata: &Metadata) -> BridgeResult<()> {
        if self.policy == ExecPolicy::Granted
            && metadata.file_type().is_symlink()
            && dunce::canonicalize(path).is_ok_and(|real| !real.starts_with(self.workspace))
        {
            return Err(outside());
        }
        Ok(())
    }

    fn describe(&self, path: &Path) -> BridgeResult<FileInfo> {
        let shown = display(path)?;
        let metadata = fs::symlink_metadata(path).map_err(|e| io_error(&shown, e))?;
        self.link_inside(path, &metadata)?;
        Ok(FileInfo {
            kind: kind(&metadata).to_owned(),
            size: metadata.len(),
            modified_ms: modified_ms(&metadata),
            created_ms: created_ms(&metadata),
            readonly: metadata.permissions().readonly(),
            mode: mode(&metadata),
            link_target: fs::read_link(path)
                .ok()
                .map(|target| target.to_string_lossy().into_owned()),
            lines: lines(path, &metadata),
            created: None,
            replaced: None,
            path: shown,
        })
    }

    pub(crate) fn info(&self, input: FileInfoInput) -> BridgeResult<FileInfo> {
        let path = self.place(&input.path)?;
        self.describe(&path)
    }

    pub(crate) fn mkdir(&self, input: DirectoryInput) -> BridgeResult<FileInfo> {
        let given = Path::new(&input.path);
        if !given.is_absolute() || input.path.as_bytes().contains(&0) {
            return Err(invalid("path must be an absolute path without NUL bytes"));
        }
        // The nearest part that exists decides where the new directories land.
        let mut ancestor = given;
        while fs::symlink_metadata(ancestor).is_err() {
            ancestor = ancestor
                .parent()
                .ok_or_else(|| invalid("path has no existing ancestor"))?;
        }
        let real = dunce::canonicalize(ancestor).map_err(|e| io_error(&input.path, e))?;
        if self.policy == ExecPolicy::Granted && !real.starts_with(self.workspace) {
            return Err(outside());
        }
        let existed = ancestor == given;
        if existed && !fs::metadata(given).is_ok_and(|m| m.is_dir()) {
            return Err(BridgeError::new(
                "not_a_directory",
                format!("{} exists and is not a directory", input.path),
            ));
        }
        fs::create_dir_all(given).map_err(|e| io_error(&input.path, e))?;
        let path = dunce::canonicalize(given).map_err(|e| io_error(&input.path, e))?;
        let mut info = self.describe(&path)?;
        info.created = Some(!existed);
        Ok(info)
    }

    pub(crate) fn moved(&self, input: FileMoveInput) -> BridgeResult<FileInfo> {
        let source = self.place(&input.source)?;
        let shown = display(&source)?;
        let from = fs::symlink_metadata(&source).map_err(|e| io_error(&shown, e))?;
        self.link_inside(&source, &from)?;
        let destination = self.place(&input.destination)?;
        if destination == source {
            return Err(invalid("source and destination are the same entry"));
        }
        if from.is_dir() && destination.starts_with(&source) {
            return Err(invalid("a directory cannot move inside itself"));
        }
        let to = display(&destination)?;
        // Checked, then renamed: an entry made at the destination in between is replaced
        // when it is a file, as overwrite would.
        let replaced = match fs::symlink_metadata(&destination) {
            Ok(existing) if existing.is_dir() => {
                return Err(BridgeError::new(
                    "already_exists",
                    format!("{to} is a directory; a move never replaces one"),
                ));
            }
            Ok(_) if !input.overwrite => {
                return Err(BridgeError::new(
                    "already_exists",
                    format!("{to} exists; pass overwrite to replace it"),
                ));
            }
            Ok(_) if from.is_dir() => {
                return Err(BridgeError::new(
                    "already_exists",
                    format!("{to} exists; a directory never replaces a file"),
                ));
            }
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(io_error(&to, error)),
        };
        fs::rename(&source, &destination).map_err(|error| {
            if error.kind() == std::io::ErrorKind::CrossesDevices {
                BridgeError::new(
                    "cross_device",
                    format!("{shown} and {to} are on different filesystems; copy it with run, then remove the original"),
                )
            } else {
                io_error(&shown, error)
            }
        })?;
        let mut info = self.describe(&destination)?;
        info.replaced = Some(replaced);
        Ok(info)
    }
}

#[cfg(test)]
mod tests;
