//! Managing files: what a path is (`info`), making directories (`mkdir`, and the parents a
//! write asks for) and moving or renaming (`moved`). Reach is the read policy: inside the
//! workspace under `granted`, wherever the account can under `account`. A symbolic link is
//! reported and moved as itself; under `granted` what it points to must be inside the
//! workspace too, judged from where the link is, or would be after a move, and from its
//! text when its target is missing.

use std::fs::{self, Metadata};
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use devagotchi_contracts::{
    BridgeError, BridgeResult, DirectoryInput, FileInfo, FileInfoInput, FileMoveInput,
};

use super::{Scope, display, invalid, io_error, modified_ms, text_prefix};
use crate::broker::ExecPolicy;

/// Most entries a directory move checks for links under `granted`.
const MAX_CHECKED_ENTRIES: usize = 10_000;

fn outside() -> BridgeError {
    BridgeError::new(
        "path_outside_workspace",
        "This machine's broker reaches files only inside its workspace",
    )
}

fn absolute(path: &str) -> BridgeResult<&Path> {
    let given = Path::new(path);
    if !given.is_absolute() || path.as_bytes().contains(&0) {
        return Err(invalid("path must be an absolute path without NUL bytes"));
    }
    Ok(given)
}

/// The nearest part of `path` that exists (as itself, links not followed), and the plain
/// names below it that do not. `None` when the missing part is not plain names (`.`, `..`).
fn split_existing(path: &Path) -> Option<(&Path, Vec<&std::ffi::OsStr>)> {
    let mut ancestor = path;
    while fs::symlink_metadata(ancestor).is_err() {
        ancestor = ancestor.parent()?;
    }
    let rest = path.strip_prefix(ancestor).ok()?;
    rest.components()
        .map(|part| match part {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .map(|names| (ancestor, names))
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

/// Windows keeps a read-only attribute on folders that it ignores for writing into them.
fn readonly(metadata: &Metadata) -> bool {
    metadata.permissions().readonly() && !(cfg!(windows) && metadata.is_dir())
}

/// Two names of one file. Unix compares the inode; elsewhere this is only known by path.
#[cfg(unix)]
fn same_file(a: &Metadata, b: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_file(_: &Metadata, _: &Metadata) -> bool {
    false
}

/// Lines in a text file small enough to count; `None` for anything else.
fn lines(path: &Path, metadata: &Metadata) -> Option<u64> {
    if !metadata.is_file() || metadata.len() > super::lines::COUNTED_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    if text_prefix(&bytes)? != bytes.len() {
        return None;
    }
    Some(super::lines::count(&bytes))
}

impl Scope<'_> {
    /// `path` canonical up to its own name, without following the name itself: the
    /// directory it is in, resolved, joined with its name (a root is itself).
    fn place(&self, path: &str) -> BridgeResult<PathBuf> {
        let given = absolute(path)?;
        let place = match (given.parent(), given.file_name()) {
            (Some(parent), Some(name)) => dunce::canonicalize(parent)
                .map_err(|e| io_error(path, e))?
                .join(name),
            _ => dunce::canonicalize(given).map_err(|e| io_error(path, e))?,
        };
        if self.policy == ExecPolicy::Granted && !place.starts_with(self.workspace) {
            return Err(outside());
        }
        Ok(place)
    }

    /// Whether `target`, followed from the directory `from`, lands inside the workspace:
    /// the part that exists resolved through any links, the rest plain names. A target
    /// that cannot be shown to land inside does not.
    fn lands_inside(&self, from: &Path, target: &Path) -> bool {
        let joined = if target.is_absolute() {
            target.to_path_buf()
        } else {
            from.join(target)
        };
        let Some((existing, missing)) = split_existing(&joined) else {
            return false;
        };
        dunce::canonicalize(existing).is_ok_and(|real| {
            missing
                .iter()
                .fold(real, |path, name| path.join(name))
                .starts_with(self.workspace)
        })
    }

    /// Under `granted`, a link must point inside the workspace for the broker to touch it.
    pub(super) fn link_inside(&self, path: &Path, metadata: &Metadata) -> BridgeResult<()> {
        if self.policy != ExecPolicy::Granted || !metadata.file_type().is_symlink() {
            return Ok(());
        }
        let target = fs::read_link(path).map_err(|e| io_error(&path.to_string_lossy(), e))?;
        match path.parent() {
            Some(from) if self.lands_inside(from, &target) => Ok(()),
            _ => Err(outside()),
        }
    }

    /// Make the directory `path` and any missing parents, following nothing that was not
    /// already there: the part that exists is resolved (and under `granted` must be in the
    /// workspace), the missing part must be plain names and is made one name at a time.
    /// Returns the directory, canonical, and whether anything was made.
    pub(super) fn make_dirs(&self, path: &str) -> BridgeResult<(PathBuf, bool)> {
        let given = absolute(path)?;
        let Some((existing, missing)) = split_existing(given) else {
            return Err(invalid(
                "the directories to make must be plain names, without . or ..",
            ));
        };
        let mut current = dunce::canonicalize(existing).map_err(|e| io_error(path, e))?;
        if self.policy == ExecPolicy::Granted && !current.starts_with(self.workspace) {
            return Err(outside());
        }
        if !fs::metadata(&current).is_ok_and(|m| m.is_dir()) {
            return Err(BridgeError::new(
                "not_a_directory",
                format!("{} exists and is not a directory", display(&current)?),
            ));
        }
        for name in &missing {
            current.push(name);
            match fs::create_dir(&current) {
                Ok(()) => {}
                Err(error)
                    if error.kind() == std::io::ErrorKind::AlreadyExists
                        && fs::symlink_metadata(&current).is_ok_and(|m| m.is_dir()) => {}
                Err(error) => return Err(io_error(path, error)),
            }
        }
        if self.policy == ExecPolicy::Granted
            && !dunce::canonicalize(&current).is_ok_and(|real| real.starts_with(self.workspace))
        {
            return Err(outside());
        }
        Ok((current, !missing.is_empty()))
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
            readonly: readonly(&metadata),
            mode: mode(&metadata),
            link_target: fs::read_link(path)
                .ok()
                .map(|target| dunce::simplified(&target).to_string_lossy().into_owned()),
            lines: lines(path, &metadata),
            made: None,
            replaced: None,
            path: shown,
        })
    }

    pub(crate) fn info(&self, input: FileInfoInput) -> BridgeResult<FileInfo> {
        let path = self.place(&input.path)?;
        self.describe(&path)
    }

    pub(crate) fn mkdir(&self, input: DirectoryInput) -> BridgeResult<FileInfo> {
        let (path, made) = self.make_dirs(&input.path)?;
        let mut info = self.describe(&path)?;
        info.made = Some(made);
        Ok(info)
    }

    /// Under `granted`, a directory moves only when no link inside it can point elsewhere
    /// once moved: none climbs with `..`, and every absolute one lands inside.
    fn links_stay_inside(&self, dir: &Path) -> BridgeResult<()> {
        let mut pending = vec![dir.to_path_buf()];
        let mut seen = 0;
        while let Some(next) = pending.pop() {
            let shown = next.to_string_lossy().into_owned();
            for entry in fs::read_dir(&next).map_err(|e| io_error(&shown, e))? {
                let path = entry.map_err(|e| io_error(&shown, e))?.path();
                seen += 1;
                if seen > MAX_CHECKED_ENTRIES {
                    return Err(BridgeError::new(
                        "too_large",
                        format!("A directory with more than {MAX_CHECKED_ENTRIES} entries is not checked for links here; move it with run"),
                    ));
                }
                let metadata = fs::symlink_metadata(&path).map_err(|e| io_error(&shown, e))?;
                if metadata.is_dir() {
                    pending.push(path);
                } else if metadata.file_type().is_symlink() {
                    let target = fs::read_link(&path).map_err(|e| io_error(&shown, e))?;
                    let climbs = target
                        .components()
                        .any(|part| matches!(part, Component::ParentDir));
                    if climbs || (target.is_absolute() && !self.lands_inside(dir, &target)) {
                        return Err(outside());
                    }
                }
            }
        }
        Ok(())
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
        if self.policy == ExecPolicy::Granted {
            if from.file_type().is_symlink() {
                // A relative link is followed from where it lands.
                let target = fs::read_link(&source).map_err(|e| io_error(&shown, e))?;
                if !destination
                    .parent()
                    .is_some_and(|dir| self.lands_inside(dir, &target))
                {
                    return Err(outside());
                }
            } else if from.is_dir() {
                self.links_stay_inside(&source)?;
            }
        }
        // Checked, then renamed: an entry made at the destination in between is replaced
        // when it is a file, as overwrite would.
        let replaced = match fs::symlink_metadata(&destination) {
            Ok(existing) => {
                let one_file = !from.file_type().is_symlink()
                    && (same_file(&from, &existing)
                        || (cfg!(windows)
                            && from.is_file()
                            && existing.is_file()
                            && dunce::canonicalize(&source).ok()
                                == dunce::canonicalize(&destination).ok()));
                if one_file && shown.eq_ignore_ascii_case(&to) {
                    // The same name in another case, on a filesystem that ignores case.
                    false
                } else if one_file {
                    // Two names of one file: a move leaves only the destination's name.
                    if !input.overwrite {
                        return Err(BridgeError::new(
                            "already_exists",
                            format!("{to} is the same file as {shown} under another name; pass overwrite to drop {shown}"),
                        ));
                    }
                    fs::remove_file(&source).map_err(|e| io_error(&shown, e))?;
                    let mut info = self.describe(&destination)?;
                    info.replaced = Some(false);
                    return Ok(info);
                } else if existing.is_dir() {
                    return Err(BridgeError::new(
                        "already_exists",
                        format!("{to} is a directory; a move never replaces one"),
                    ));
                } else if !input.overwrite {
                    return Err(BridgeError::new(
                        "already_exists",
                        format!("{to} exists; pass overwrite to replace it"),
                    ));
                } else if from.is_dir() {
                    return Err(BridgeError::new(
                        "already_exists",
                        format!("{to} exists; a directory never replaces a file"),
                    ));
                } else {
                    true
                }
            }
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
        match self.describe(&destination) {
            Ok(mut info) => {
                info.replaced = Some(replaced);
                Ok(info)
            }
            Err(error) => {
                // Out of reach after all (it changed while moving): put it back.
                let _ = fs::rename(&destination, &source);
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests;
