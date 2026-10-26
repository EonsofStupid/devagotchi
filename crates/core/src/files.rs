//! Files on the broker's machine: bounded reads and directory listings, answered as direct
//! broker queries (no operation, nothing journaled) under the same policy as execution.
//! Under `granted` a path must resolve inside the workspace; under `account` the broker's
//! OS account's own permissions decide. Symbolic links are resolved for the target of a
//! read or listing, and listed, never followed, inside a listing.

use std::fs::Metadata;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use devagotchi_contracts::{
    BridgeError, BridgeResult, FileContent, FileEntry, FileListInput, FileListing, FileReadInput,
    MAX_FILE_READ_BYTES,
};

use crate::broker::ExecPolicy;

mod write;

const MAX_ENTRIES: u32 = 1000;
const MAX_DEPTH: u32 = 3;

/// Where file requests may reach.
pub(crate) struct Scope<'a> {
    pub(crate) workspace: &'a Path,
    pub(crate) policy: ExecPolicy,
}

fn invalid(message: impl Into<String>) -> BridgeError {
    BridgeError::new("invalid_request", message)
}

fn io_error(path: &str, error: std::io::Error) -> BridgeError {
    match error.kind() {
        std::io::ErrorKind::NotFound => {
            BridgeError::new("path_not_found", format!("{path} does not exist"))
        }
        std::io::ErrorKind::PermissionDenied => BridgeError::new(
            "permission_denied",
            format!("The broker's account may not read {path}"),
        ),
        _ => BridgeError::new("io_error", format!("{path}: {error}")),
    }
}

fn modified_ms(metadata: &Metadata) -> Option<u64> {
    let since = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(since.as_millis().min(u64::MAX as u128) as u64)
}

fn display(path: &Path) -> BridgeResult<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid("Canonical paths must be representable as UTF-8"))
}

/// Text an AI can read as is: UTF-8 with no control characters but tab, newline, carriage
/// return and form feed.
fn readable(text: &str) -> bool {
    !text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r' | '\u{c}'))
}

/// How many leading bytes of `bytes` are readable text: all of them, or all but a
/// character cut by the end of the page. `None` when they are not text.
fn text_prefix(bytes: &[u8]) -> Option<usize> {
    match std::str::from_utf8(bytes) {
        Ok(text) if readable(text) => Some(bytes.len()),
        Err(error)
            if error.error_len().is_none()
                && error.valid_up_to() > 0
                && std::str::from_utf8(&bytes[..error.valid_up_to()]).is_ok_and(readable) =>
        {
            Some(error.valid_up_to())
        }
        _ => None,
    }
}

impl Scope<'_> {
    fn resolve(&self, path: &str) -> BridgeResult<PathBuf> {
        if !Path::new(path).is_absolute() || path.as_bytes().contains(&0) {
            return Err(invalid("path must be an absolute path without NUL bytes"));
        }
        let canonical = dunce::canonicalize(path).map_err(|error| io_error(path, error))?;
        if self.policy == ExecPolicy::Granted && !canonical.starts_with(self.workspace) {
            return Err(BridgeError::new(
                "path_outside_workspace",
                "This machine's broker reaches files only inside its workspace",
            ));
        }
        Ok(canonical)
    }

    pub(crate) fn read(&self, input: FileReadInput) -> BridgeResult<FileContent> {
        if !(1..=MAX_FILE_READ_BYTES).contains(&input.max_bytes) {
            return Err(invalid(format!(
                "max_bytes must be 1..={MAX_FILE_READ_BYTES}"
            )));
        }
        let path = self.resolve(&input.path)?;
        let shown = display(&path)?;
        let mut file = std::fs::File::open(&path).map_err(|e| io_error(&shown, e))?;
        let metadata = file.metadata().map_err(|e| io_error(&shown, e))?;
        if !metadata.is_file() {
            return Err(BridgeError::new(
                "not_a_file",
                format!("{shown} is not a regular file; list a directory with list_directory"),
            ));
        }
        let size = metadata.len();
        let mut start = if input.offset < 0 {
            size.saturating_sub(input.offset.unsigned_abs())
        } else {
            (input.offset as u64).min(size)
        };
        file.seek(SeekFrom::Start(start))
            .map_err(|e| io_error(&shown, e))?;
        let mut bytes = Vec::with_capacity(input.max_bytes);
        (&mut file)
            .take(input.max_bytes as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| io_error(&shown, e))?;
        // A text page that starts inside a UTF-8 character (a tail, or an offset chosen by
        // hand) starts at the next character instead; `offset` says where it really starts.
        // Binary pages are never shifted, so reading on from `next_offset` loses no byte.
        let lead = if start > 0 {
            bytes
                .iter()
                .take(3)
                .take_while(|byte| *byte & 0xC0 == 0x80)
                .count()
        } else {
            0
        };
        let (skip, text_len) = match text_prefix(&bytes[lead..]) {
            Some(length) if lead > 0 => (lead, Some(length)),
            _ => (0, text_prefix(&bytes)),
        };
        start += skip as u64;
        let (text, bytes_base64, used) = match text_len {
            Some(length) => (
                std::str::from_utf8(&bytes[skip..skip + length])
                    .ok()
                    .map(str::to_owned),
                None,
                length,
            ),
            None => (None, Some(BASE64.encode(&bytes)), bytes.len()),
        };
        let next_offset = start + used as u64;
        Ok(FileContent {
            path: shown,
            size,
            offset: start,
            text,
            bytes_base64,
            next_offset,
            eof: next_offset >= size,
            modified_ms: modified_ms(&metadata),
        })
    }

    pub(crate) fn list(&self, input: FileListInput) -> BridgeResult<FileListing> {
        if input.depth > MAX_DEPTH {
            return Err(invalid(format!("depth must be 0..={MAX_DEPTH}")));
        }
        if !(1..=MAX_ENTRIES).contains(&input.max_entries) {
            return Err(invalid(format!("max_entries must be 1..={MAX_ENTRIES}")));
        }
        let root = self.resolve(&input.path)?;
        let shown = display(&root)?;
        let metadata = std::fs::metadata(&root).map_err(|e| io_error(&shown, e))?;
        if !metadata.is_dir() {
            return Err(BridgeError::new(
                "not_a_directory",
                format!("{shown} is not a directory; read a file with read_file"),
            ));
        }
        let mut entries = Vec::new();
        let mut truncated = false;
        walk(
            &root,
            "",
            input.depth,
            input.max_entries as usize,
            &mut entries,
            &mut truncated,
        )
        .map_err(|e| io_error(&shown, e))?;
        Ok(FileListing {
            path: shown,
            entries,
            truncated,
        })
    }
}

/// One directory's entries, directories first then by name, and below them as deep as
/// `depth` allows. Only the root's own read failure is an error; a subdirectory that
/// cannot be read is listed with why.
fn walk(
    dir: &Path,
    prefix: &str,
    depth: u32,
    limit: usize,
    entries: &mut Vec<FileEntry>,
    truncated: &mut bool,
) -> std::io::Result<()> {
    let mut children: Vec<(String, PathBuf, Option<Metadata>)> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            (
                name,
                entry.path(),
                std::fs::symlink_metadata(entry.path()).ok(),
            )
        })
        .collect();
    children.sort_by(|left, right| {
        let left_dir = left.2.as_ref().is_some_and(Metadata::is_dir);
        let right_dir = right.2.as_ref().is_some_and(Metadata::is_dir);
        right_dir.cmp(&left_dir).then_with(|| left.0.cmp(&right.0))
    });
    for (name, path, metadata) in children {
        if entries.len() >= limit {
            *truncated = true;
            return Ok(());
        }
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let kind = match &metadata {
            Some(m) if m.file_type().is_symlink() => "symlink",
            Some(m) if m.is_dir() => "dir",
            Some(m) if m.is_file() => "file",
            _ => "other",
        };
        let entry = FileEntry {
            path: relative.clone(),
            kind: kind.to_owned(),
            size: metadata.as_ref().filter(|m| m.is_file()).map(Metadata::len),
            modified_ms: metadata.as_ref().and_then(modified_ms),
            error: None,
        };
        if kind == "dir" && depth > 0 {
            let at = entries.len();
            entries.push(entry);
            if let Err(error) = walk(&path, &relative, depth - 1, limit, entries, truncated) {
                entries[at].error = Some(error.to_string());
            }
            continue;
        }
        entries.push(entry);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(workspace: &Path, policy: ExecPolicy) -> Scope<'_> {
        Scope { workspace, policy }
    }

    fn read(scope: &Scope<'_>, path: &Path, offset: i64, max_bytes: usize) -> FileContent {
        scope
            .read(FileReadInput {
                path: path.display().to_string(),
                offset,
                max_bytes,
            })
            .unwrap()
    }

    #[test]
    fn text_reads_in_pages_without_splitting_a_character() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = dunce::canonicalize(directory.path()).unwrap();
        let file = workspace.join("notes.txt");
        std::fs::write(&file, "héllo wörld\nline two\n").unwrap();
        let scope = scope(&workspace, ExecPolicy::Granted);
        // "hé" is three bytes; a two-byte page stops before the cut "é".
        let first = read(&scope, &file, 0, 2);
        assert_eq!(first.text.as_deref(), Some("h"));
        assert_eq!(first.next_offset, 1);
        let rest = read(&scope, &file, first.next_offset as i64, 1024);
        assert_eq!(rest.text.as_deref(), Some("éllo wörld\nline two\n"));
        assert!(rest.eof);
        // A tail: the last 9 bytes.
        let tail = read(&scope, &file, -9, 1024);
        assert_eq!(tail.text.as_deref(), Some("line two\n"));
        assert_eq!(tail.offset, tail.size - 9);
    }

    #[test]
    fn binary_comes_back_as_base64() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = dunce::canonicalize(directory.path()).unwrap();
        let file = workspace.join("blob.bin");
        std::fs::write(&file, [0u8, 159, 146, 150, 1, 2]).unwrap();
        let content = read(&scope(&workspace, ExecPolicy::Granted), &file, 0, 1024);
        assert!(content.text.is_none());
        assert_eq!(
            BASE64.decode(content.bytes_base64.unwrap()).unwrap(),
            [0u8, 159, 146, 150, 1, 2]
        );
    }

    #[test]
    fn the_granted_policy_stays_inside_the_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = dunce::canonicalize(directory.path()).unwrap().join("ws");
        std::fs::create_dir(&workspace).unwrap();
        let outside = workspace.parent().unwrap().join("outside.txt");
        std::fs::write(&outside, "secret").unwrap();
        let input = FileReadInput {
            path: outside.display().to_string(),
            offset: 0,
            max_bytes: 1024,
        };
        let refused = scope(&workspace, ExecPolicy::Granted)
            .read(input.clone())
            .unwrap_err();
        assert_eq!(refused.code, "path_outside_workspace");
        let read = scope(&workspace, ExecPolicy::Account).read(input).unwrap();
        assert_eq!(read.text.as_deref(), Some("secret"));
        let missing = scope(&workspace, ExecPolicy::Account)
            .read(FileReadInput {
                path: workspace.join("nope").display().to_string(),
                offset: 0,
                max_bytes: 1024,
            })
            .unwrap_err();
        assert_eq!(missing.code, "path_not_found");
    }

    #[test]
    fn listings_put_directories_first_and_respect_depth_and_limits() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = dunce::canonicalize(directory.path()).unwrap();
        std::fs::create_dir_all(workspace.join("src/deep")).unwrap();
        std::fs::write(workspace.join("README.md"), "hi").unwrap();
        std::fs::write(workspace.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(workspace.join("src/deep/x.rs"), "").unwrap();
        let scope = scope(&workspace, ExecPolicy::Granted);
        let list = |depth, max_entries| {
            scope
                .list(FileListInput {
                    path: workspace.display().to_string(),
                    depth,
                    max_entries,
                })
                .unwrap()
        };
        let top = list(0, 200);
        let names: Vec<_> = top
            .entries
            .iter()
            .map(|e| (e.path.as_str(), e.kind.as_str()))
            .collect();
        assert_eq!(names, [("src", "dir"), ("README.md", "file")]);
        assert_eq!(top.entries[1].size, Some(2));
        let all = list(2, 200);
        let paths: Vec<_> = all.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "src",
                "src/deep",
                "src/deep/x.rs",
                "src/main.rs",
                "README.md"
            ]
        );
        let cut = list(2, 2);
        assert!(cut.truncated);
        assert_eq!(cut.entries.len(), 2);
    }
}
