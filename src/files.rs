//! Workspace file queries: bounded, read-only list and read requests that the
//! broker answers directly, without creating an operation. Every path
//! resolves inside the configured workspace. A read is checked against the
//! opened handle's own path and closes that handle before it returns, so a
//! completed read never blocks a later replace.

use crate::contracts::*;
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD as CURSOR},
};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fs::{DirEntry, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

mod platform;

/// Workspace file access the operator grants to broker clients.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileAccess {
    /// No file queries. This is the default.
    #[default]
    None,
    /// `file_list` and `file_read` inside the workspace.
    Read,
}

/// Directory entries one list request may examine.
const MAX_DIRECTORY_ENTRIES: usize = 200_000;
const MIN_FILE_PAGE_BYTES: usize = 1024;
/// A native name is at most 255 bytes on Unix and 255 UTF-16 units on Windows.
const MAX_CURSOR_KEY_BYTES: usize = 1024;

fn invalid(message: impl Into<String>) -> BridgeError {
    BridgeError::new("invalid_request", message)
}

fn io_error(error: std::io::Error) -> BridgeError {
    match error.kind() {
        std::io::ErrorKind::NotFound => BridgeError::new("file_not_found", error.to_string()),
        std::io::ErrorKind::PermissionDenied => {
            BridgeError::new("file_permission_denied", error.to_string())
        }
        _ => BridgeError::new("file_io", error.to_string()),
    }
}

fn path_string(path: &Path) -> BridgeResult<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid("Canonical paths must be representable as UTF-8"))
}

fn modified_ms(metadata: &Metadata) -> Option<u64> {
    let since_epoch = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    u64::try_from(since_epoch.as_millis()).ok()
}

/// Canonicalizes `value` and requires the result to lie inside `workspace`.
fn resolve(workspace: &Path, value: &str) -> BridgeResult<PathBuf> {
    let path = Path::new(value);
    if !path.is_absolute() || value.contains('\0') {
        return Err(invalid("path must be an absolute path without NUL bytes"));
    }
    let canonical = dunce::canonicalize(path).map_err(io_error)?;
    if !canonical.starts_with(workspace) {
        return Err(BridgeError::new(
            "path_outside_workspace",
            "The canonical path is outside the configured workspace",
        ));
    }
    Ok(canonical)
}

/// One directory entry, ordered by its native name.
struct Candidate {
    key: Vec<u8>,
    entry: DirEntry,
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}
impl Eq for Candidate {}
impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key.cmp(&other.key)
    }
}

fn decode_cursor(cursor: &str) -> BridgeResult<Vec<u8>> {
    CURSOR
        .decode(cursor)
        .ok()
        .filter(|key| !key.is_empty() && key.len() <= MAX_CURSOR_KEY_BYTES)
        .ok_or_else(|| invalid("cursor must be a next_cursor returned by file_list"))
}

fn describe(entry: &DirEntry) -> FileEntry {
    let name = entry.file_name();
    let kind = match entry.file_type() {
        Ok(kind) if kind.is_symlink() => FileKind::Symlink,
        Ok(kind) if kind.is_dir() => FileKind::Directory,
        Ok(kind) if kind.is_file() => FileKind::File,
        _ => FileKind::Other,
    };
    // DirEntry metadata describes the entry itself and never follows a link.
    let metadata = entry.metadata().ok();
    FileEntry {
        name: name.to_string_lossy().into_owned(),
        name_exact: name.to_str().is_some(),
        size_bytes: metadata
            .as_ref()
            .filter(|_| kind == FileKind::File)
            .map(Metadata::len),
        modified_ms: metadata.as_ref().and_then(modified_ms),
        kind,
    }
}

/// Lists one directory in ascending native-name order. Only the page's
/// `limit + 1` smallest names after the cursor are retained while scanning.
pub(crate) fn list(workspace: &Path, input: FileListInput) -> BridgeResult<FileListPage> {
    if !(1..=MAX_FILE_LIST_ENTRIES).contains(&input.limit) {
        return Err(invalid(format!(
            "limit must be 1..={MAX_FILE_LIST_ENTRIES}"
        )));
    }
    let after = input.cursor.as_deref().map(decode_cursor).transpose()?;
    let directory = resolve(workspace, &input.path)?;
    if !std::fs::metadata(&directory).map_err(io_error)?.is_dir() {
        return Err(BridgeError::new(
            "not_a_directory",
            "path does not name a directory",
        ));
    }
    let keep = input.limit as usize + 1;
    let mut selected = BinaryHeap::with_capacity(keep + 1);
    for (examined, entry) in std::fs::read_dir(&directory).map_err(io_error)?.enumerate() {
        if examined >= MAX_DIRECTORY_ENTRIES {
            return Err(BridgeError::new(
                "directory_too_large",
                format!("The directory has more than {MAX_DIRECTORY_ENTRIES} entries"),
            ));
        }
        let entry = entry.map_err(io_error)?;
        let key = platform::sort_key(&entry.file_name());
        if after.as_ref().is_some_and(|after| key <= *after) {
            continue;
        }
        selected.push(Candidate { key, entry });
        if selected.len() > keep {
            selected.pop();
        }
    }
    let mut selected = selected.into_sorted_vec();
    let has_more = selected.len() > input.limit as usize;
    selected.truncate(input.limit as usize);
    let next_cursor = has_more
        .then(|| selected.last().map(|last| CURSOR.encode(&last.key)))
        .flatten();
    Ok(FileListPage {
        path: path_string(&directory)?,
        entries: selected
            .iter()
            .map(|candidate| describe(&candidate.entry))
            .collect(),
        next_cursor,
    })
}

/// Reads one bounded byte range of a regular file through a handle whose own
/// path is verified to lie inside the workspace.
pub(crate) fn read(workspace: &Path, input: FileReadInput) -> BridgeResult<FileReadPage> {
    if !(MIN_FILE_PAGE_BYTES..=MAX_FILE_PAGE_BYTES).contains(&input.max_bytes) {
        return Err(invalid(format!(
            "max_bytes must be {MIN_FILE_PAGE_BYTES}..={MAX_FILE_PAGE_BYTES}"
        )));
    }
    let path = resolve(workspace, &input.path)?;
    let not_a_file = || BridgeError::new("not_a_file", "path does not name a regular file");
    // Checked before opening, so a directory, FIFO, or device is never opened.
    if !std::fs::metadata(&path).map_err(io_error)?.is_file() {
        return Err(not_a_file());
    }
    let mut file = platform::open_for_read(&path).map_err(io_error)?;
    let metadata = file.metadata().map_err(io_error)?;
    if !metadata.is_file() {
        return Err(not_a_file());
    }
    // Canonicalization is a snapshot; the opened object is what gets read.
    let opened = platform::opened_path(&file).map_err(io_error)?;
    if !opened.starts_with(workspace) {
        return Err(BridgeError::new(
            "path_outside_workspace",
            "The opened file is outside the configured workspace",
        ));
    }
    let revision = FileRevision {
        identity: platform::identity(&file, &metadata).map_err(io_error)?,
        size_bytes: metadata.len(),
        modified_ms: modified_ms(&metadata),
    };
    file.seek(SeekFrom::Start(input.offset)).map_err(io_error)?;
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len().saturating_sub(input.offset))
            .unwrap_or(usize::MAX)
            .min(input.max_bytes),
    );
    (&mut file)
        .take(input.max_bytes as u64)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    drop(file);
    let reached_end = bytes.len() < input.max_bytes;
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => Some(text.to_owned()),
        // Only a character cut off by the page limit is deferred to the next page.
        Err(error) if error.error_len().is_none() && !reached_end && error.valid_up_to() > 0 => {
            bytes.truncate(error.valid_up_to());
            Some(String::from_utf8(bytes.clone()).expect("validated UTF-8 prefix"))
        }
        Err(_) => None,
    };
    Ok(FileReadPage {
        path: path_string(&path)?,
        revision,
        offset: input.offset,
        next_offset: input.offset + bytes.len() as u64,
        bytes_base64: BASE64.encode(&bytes),
        text,
        eof: reached_end,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Workspace {
        _directory: tempfile::TempDir,
        root: PathBuf,
    }

    fn workspace() -> Workspace {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        Workspace {
            _directory: directory,
            root,
        }
    }

    fn read_page(root: &Path, path: &Path, offset: u64, max_bytes: usize) -> FileReadPage {
        read(
            root,
            FileReadInput {
                path: path.display().to_string(),
                offset,
                max_bytes,
            },
        )
        .unwrap()
    }

    fn list_page(root: &Path, path: &Path, cursor: Option<String>, limit: u32) -> FileListPage {
        list(
            root,
            FileListInput {
                path: path.display().to_string(),
                cursor,
                limit,
            },
        )
        .unwrap()
    }

    #[test]
    fn reads_are_bounded_and_defer_a_split_character_to_the_next_page() {
        let workspace = workspace();
        let file = workspace.root.join("notes.txt");
        let content = format!("{}€tail", "a".repeat(1023));
        std::fs::write(&file, &content).unwrap();
        let first = read_page(&workspace.root, &file, 0, 1024);
        assert_eq!(first.text.as_deref(), Some("a".repeat(1023).as_str()));
        assert_eq!(first.next_offset, 1023);
        assert!(!first.eof);
        let second = read_page(&workspace.root, &file, first.next_offset, 1024);
        assert_eq!(second.text.as_deref(), Some("€tail"));
        assert!(second.eof);
        assert_eq!(second.revision, first.revision);
        assert_eq!(second.revision.size_bytes, content.len() as u64);
    }

    #[test]
    fn binary_ranges_are_exact_and_have_no_text_view() {
        let workspace = workspace();
        let file = workspace.root.join("blob.bin");
        std::fs::write(&file, [0xff, 0x00, 0x80, b'x']).unwrap();
        let page = read_page(&workspace.root, &file, 0, 1024);
        assert_eq!(
            BASE64.decode(page.bytes_base64).unwrap(),
            [0xff, 0x00, 0x80, b'x']
        );
        assert!(page.text.is_none());
        assert!(page.eof);
        let past_end = read_page(&workspace.root, &file, 100, 1024);
        assert!(past_end.bytes_base64.is_empty() && past_end.eof);
        assert_eq!(past_end.next_offset, 100);
    }

    #[test]
    fn listing_is_ordered_paginated_and_reports_entries_themselves() {
        let workspace = workspace();
        for name in ["c.txt", "a.txt", "b.txt"] {
            std::fs::write(workspace.root.join(name), name).unwrap();
        }
        std::fs::create_dir(workspace.root.join("d")).unwrap();
        let first = list_page(&workspace.root, &workspace.root, None, 2);
        let names: Vec<_> = first
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, ["a.txt", "b.txt"]);
        assert_eq!(first.entries[0].kind, FileKind::File);
        assert_eq!(first.entries[0].size_bytes, Some(5));
        let second = list_page(&workspace.root, &workspace.root, first.next_cursor, 2);
        let names: Vec<_> = second
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, ["c.txt", "d"]);
        assert_eq!(second.entries[1].kind, FileKind::Directory);
        assert_eq!(second.entries[1].size_bytes, None);
        assert!(second.next_cursor.is_none());
        assert_eq!(second.path, path_string(&workspace.root).unwrap());
    }

    #[test]
    fn paths_outside_the_workspace_are_refused() {
        let workspace = workspace();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "secret").unwrap();
        let error = read(
            &workspace.root,
            FileReadInput {
                path: secret.display().to_string(),
                offset: 0,
                max_bytes: 1024,
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "path_outside_workspace");
        let traversal = workspace
            .root
            .join("..")
            .join(outside.path().file_name().unwrap());
        let error = list(
            &workspace.root,
            FileListInput {
                path: traversal.display().to_string(),
                cursor: None,
                limit: 8,
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "path_outside_workspace");
        let error = read(
            &workspace.root,
            FileReadInput {
                path: "relative.txt".into(),
                offset: 0,
                max_bytes: 1024,
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "invalid_request");
    }

    #[test]
    fn a_completed_read_leaves_the_file_replaceable_and_identity_tracks_the_object() {
        let workspace = workspace();
        let file = workspace.root.join("config.json");
        std::fs::write(&file, "{\"version\":1}").unwrap();
        let original = read_page(&workspace.root, &file, 0, 1024).revision;
        let renamed = workspace.root.join("renamed.json");
        std::fs::rename(&file, &renamed).unwrap();
        assert_eq!(
            read_page(&workspace.root, &renamed, 0, 1024)
                .revision
                .identity,
            original.identity
        );
        // An atomic replace over the file that was just read succeeds.
        let replacement = workspace.root.join("replacement.json");
        std::fs::write(&replacement, "{\"version\":2}").unwrap();
        std::fs::rename(&replacement, &renamed).unwrap();
        let replaced = read_page(&workspace.root, &renamed, 0, 1024);
        assert_ne!(replaced.revision.identity, original.identity);
        assert_eq!(replaced.text.as_deref(), Some("{\"version\":2}"));
    }

    #[test]
    fn directories_and_files_are_not_interchangeable() {
        let workspace = workspace();
        let file = workspace.root.join("file.txt");
        std::fs::write(&file, "x").unwrap();
        let error = list(
            &workspace.root,
            FileListInput {
                path: file.display().to_string(),
                cursor: None,
                limit: 8,
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "not_a_directory");
        let error = read(
            &workspace.root,
            FileReadInput {
                path: workspace.root.display().to_string(),
                offset: 0,
                max_bytes: 1024,
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "not_a_file");
        let error = list(
            &workspace.root,
            FileListInput {
                path: workspace.root.display().to_string(),
                cursor: Some("not a cursor!".into()),
                limit: 8,
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "invalid_request");
    }

    #[test]
    fn names_that_are_not_unicode_are_listed_inexactly_and_paginate() {
        let workspace = workspace();
        #[cfg(unix)]
        let name = {
            use std::os::unix::ffi::OsStringExt;
            std::ffi::OsString::from_vec(b"bad\xff".to_vec())
        };
        #[cfg(windows)]
        let name = {
            use std::os::windows::ffi::OsStringExt;
            std::ffi::OsString::from_wide(&[
                u16::from(b'b'),
                u16::from(b'a'),
                u16::from(b'd'),
                0xD800,
            ])
        };
        std::fs::write(workspace.root.join(&name), "x").unwrap();
        std::fs::write(workspace.root.join("zz.txt"), "z").unwrap();
        let first = list_page(&workspace.root, &workspace.root, None, 1);
        assert_eq!(first.entries[0].name, "bad\u{fffd}");
        assert!(!first.entries[0].name_exact);
        let second = list_page(&workspace.root, &workspace.root, first.next_cursor, 1);
        assert_eq!(second.entries[0].name, "zz.txt");
        assert!(second.entries[0].name_exact);
    }

    #[cfg(unix)]
    #[test]
    fn links_are_listed_unfollowed_and_reads_through_them_stay_inside() {
        let workspace = workspace();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        std::os::unix::fs::symlink(outside.path(), workspace.root.join("escape")).unwrap();
        std::fs::write(workspace.root.join("inside.txt"), "inside").unwrap();
        std::os::unix::fs::symlink(
            workspace.root.join("inside.txt"),
            workspace.root.join("alias.txt"),
        )
        .unwrap();
        let page = list_page(&workspace.root, &workspace.root, None, 8);
        let escape = page
            .entries
            .iter()
            .find(|entry| entry.name == "escape")
            .unwrap();
        assert_eq!(escape.kind, FileKind::Symlink);
        let error = read(
            &workspace.root,
            FileReadInput {
                path: workspace
                    .root
                    .join("escape/secret.txt")
                    .display()
                    .to_string(),
                offset: 0,
                max_bytes: 1024,
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "path_outside_workspace");
        let alias = read_page(&workspace.root, &workspace.root.join("alias.txt"), 0, 1024);
        assert_eq!(alias.text.as_deref(), Some("inside"));
        assert_eq!(
            alias.path,
            path_string(&workspace.root.join("inside.txt")).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_without_blocking() {
        let workspace = workspace();
        let fifo = workspace.root.join("pipe");
        let path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let error = read(
            &workspace.root,
            FileReadInput {
                path: fifo.display().to_string(),
                offset: 0,
                max_bytes: 1024,
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "not_a_file");
    }
}
