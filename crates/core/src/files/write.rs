//! Changing files: write (overwrite, create, append) and edit (exact text replacement).
//! A whole-content change is written to a new file beside the target and renamed into
//! place, so a reader sees the old content or the new, never half of each; the target's
//! permissions carry over. Reach is the read policy: inside the workspace under `granted`,
//! wherever the account can write under `account`.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use devagotchi_contracts::{
    BridgeError, BridgeResult, FileEditInput, FileWriteInput, FileWritten, MAX_FILE_WRITE_BYTES,
    WriteMode,
};
use sha2::{Digest, Sha256};

use super::{Scope, display, invalid, io_error};
use crate::broker::ExecPolicy;

/// Largest file an edit reads whole.
const MAX_EDIT_BYTES: u64 = 8 * 1024 * 1024;

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Replace `target`'s content with `bytes` through a new file renamed over it.
fn replace(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = target.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    let name = target
        .file_name()
        .ok_or(std::io::ErrorKind::InvalidInput)?
        .to_string_lossy();
    let temporary = dir.join(format!(
        ".{name}.devagotchi-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let written = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        if let Ok(metadata) = fs::metadata(target) {
            fs::set_permissions(&temporary, metadata.permissions())?;
        }
        fs::rename(&temporary, target)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

impl Scope<'_> {
    /// Where a write lands: an existing file resolves through any links; a new one is its
    /// canonical parent directory joined with its name.
    fn target(&self, path: &str, create_dirs: bool) -> BridgeResult<(PathBuf, bool)> {
        if !Path::new(path).is_absolute() || path.as_bytes().contains(&0) {
            return Err(invalid("path must be an absolute path without NUL bytes"));
        }
        let given = Path::new(path);
        let (canonical, exists) = match dunce::canonicalize(given) {
            Ok(canonical) => (canonical, true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = given
                    .parent()
                    .ok_or_else(|| invalid("path must name a file"))?;
                let name = given
                    .file_name()
                    .ok_or_else(|| invalid("path must name a file"))?;
                // A link whose target is missing: writing through it makes the target.
                if let Ok(link) = fs::symlink_metadata(given) {
                    self.link_inside(given, &link)?;
                }
                let parent = if create_dirs {
                    let parent = parent
                        .to_str()
                        .ok_or_else(|| invalid("path must be UTF-8"))?;
                    self.make_dirs(parent)?.0
                } else {
                    dunce::canonicalize(parent).map_err(|e| io_error(path, e))?
                };
                (parent.join(name), false)
            }
            Err(error) => return Err(io_error(path, error)),
        };
        if self.policy == ExecPolicy::Granted && !canonical.starts_with(self.workspace) {
            return Err(outside());
        }
        if exists && !fs::metadata(&canonical).is_ok_and(|m| m.is_file()) {
            return Err(BridgeError::new(
                "not_a_file",
                format!("{path} exists and is not a regular file"),
            ));
        }
        Ok((canonical, exists))
    }

    pub(crate) fn write(&self, input: FileWriteInput) -> BridgeResult<FileWritten> {
        let bytes = match (&input.text, &input.bytes_base64) {
            (Some(text), None) => text.as_bytes().to_vec(),
            (None, Some(encoded)) => BASE64
                .decode(encoded)
                .map_err(|_| invalid("bytes_base64 is not valid base64"))?,
            _ => return Err(invalid("Give exactly one of text or bytes_base64")),
        };
        if bytes.len() > MAX_FILE_WRITE_BYTES {
            return Err(invalid(format!(
                "At most {MAX_FILE_WRITE_BYTES} bytes per call; write the rest with mode append"
            )));
        }
        let (target, exists) = self.target(&input.path, input.create_dirs)?;
        let shown = display(&target)?;
        if exists && input.mode == WriteMode::Create {
            return Err(BridgeError::new(
                "already_exists",
                format!("{shown} exists; use mode overwrite to replace it"),
            ));
        }
        if let Some(expected) = &input.expected_sha256 {
            let current = if exists {
                sha256(&fs::read(&target).map_err(|e| io_error(&shown, e))?)
            } else {
                String::new()
            };
            if !current.eq_ignore_ascii_case(expected) {
                return Err(BridgeError::new(
                    "changed_since_read",
                    format!(
                        "{shown} changed since that SHA-256 was taken; read it again before writing"
                    ),
                ));
            }
        }
        match input.mode {
            WriteMode::Overwrite | WriteMode::Create => {
                replace(&target, &bytes).map_err(|e| io_error(&shown, e))?
            }
            WriteMode::Append => {
                let mut file = OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(&target)
                    .map_err(|e| io_error(&shown, e))?;
                file.write_all(&bytes).map_err(|e| io_error(&shown, e))?;
                file.sync_all().map_err(|e| io_error(&shown, e))?;
            }
        }
        let content = fs::read(&target).map_err(|e| io_error(&shown, e))?;
        Ok(FileWritten {
            path: shown,
            size: content.len() as u64,
            sha256: sha256(&content),
            created: !exists,
            replacements: None,
        })
    }

    pub(crate) fn edit(&self, input: FileEditInput) -> BridgeResult<FileWritten> {
        if input.old_text.is_empty() {
            return Err(invalid("old_text must not be empty"));
        }
        if input.old_text.len() + input.new_text.len() > MAX_FILE_WRITE_BYTES {
            return Err(invalid(format!(
                "old_text and new_text together must be at most {MAX_FILE_WRITE_BYTES} bytes"
            )));
        }
        let (target, exists) = self.target(&input.path, false)?;
        let shown = display(&target)?;
        if !exists {
            return Err(BridgeError::new(
                "path_not_found",
                format!("{shown} does not exist"),
            ));
        }
        let size = fs::metadata(&target)
            .map_err(|e| io_error(&shown, e))?
            .len();
        if size > MAX_EDIT_BYTES {
            return Err(invalid(format!(
                "{shown} is larger than {MAX_EDIT_BYTES} bytes; edit it with run"
            )));
        }
        let current = fs::read(&target).map_err(|e| io_error(&shown, e))?;
        let text = String::from_utf8(current)
            .map_err(|_| invalid(format!("{shown} is not UTF-8 text; use write_file")))?;
        let found = text.matches(&input.old_text).count() as u32;
        if found != input.expected_replacements {
            return Err(BridgeError::new(
                "edit_mismatch",
                format!(
                    "old_text occurs {found} times in {shown}, not {}; nothing was changed. Include more surrounding text to make it unique, or set expected_replacements.",
                    input.expected_replacements
                ),
            ));
        }
        let changed = text.replace(&input.old_text, &input.new_text);
        replace(&target, changed.as_bytes()).map_err(|e| io_error(&shown, e))?;
        Ok(FileWritten {
            path: shown,
            size: changed.len() as u64,
            sha256: sha256(changed.as_bytes()),
            created: false,
            replacements: Some(found),
        })
    }
}

fn outside() -> BridgeError {
    BridgeError::new(
        "path_outside_workspace",
        "This machine's broker reaches files only inside its workspace",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(path: &Path, text: &str, mode: WriteMode) -> FileWriteInput {
        FileWriteInput {
            path: path.display().to_string(),
            text: Some(text.into()),
            bytes_base64: None,
            mode,
            expected_sha256: None,
            create_dirs: false,
        }
    }

    #[test]
    fn writes_create_overwrite_and_append_with_a_checksum() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = dunce::canonicalize(directory.path()).unwrap();
        let scope = Scope {
            workspace: &workspace,
            policy: ExecPolicy::Granted,
        };
        let file = workspace.join("a.txt");
        let made = scope
            .write(input(&file, "one\n", WriteMode::Create))
            .unwrap();
        assert!(made.created);
        let again = scope
            .write(input(&file, "x", WriteMode::Create))
            .unwrap_err();
        assert_eq!(again.code, "already_exists");
        let mut guarded = input(&file, "two\n", WriteMode::Overwrite);
        guarded.expected_sha256 = Some("0".repeat(64));
        assert_eq!(
            scope.write(guarded.clone()).unwrap_err().code,
            "changed_since_read"
        );
        guarded.expected_sha256 = Some(made.sha256);
        let replaced = scope.write(guarded).unwrap();
        assert!(!replaced.created);
        scope
            .write(input(&file, "three\n", WriteMode::Append))
            .unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "two\nthree\n");
        // Nothing is left beside it.
        assert_eq!(fs::read_dir(&workspace).unwrap().count(), 1);
    }

    #[test]
    fn edits_replace_exact_text_only_at_the_expected_count() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = dunce::canonicalize(directory.path()).unwrap();
        let scope = Scope {
            workspace: &workspace,
            policy: ExecPolicy::Granted,
        };
        let file = workspace.join("main.rs");
        fs::write(&file, "let a = 1;\nlet b = 1;\n").unwrap();
        let edit = |old: &str, new: &str, count| {
            scope.edit(FileEditInput {
                path: file.display().to_string(),
                old_text: old.into(),
                new_text: new.into(),
                expected_replacements: count,
            })
        };
        let ambiguous = edit("= 1;", "= 2;", 1).unwrap_err();
        assert_eq!(ambiguous.code, "edit_mismatch");
        assert!(ambiguous.message.contains("occurs 2 times"));
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "let a = 1;\nlet b = 1;\n"
        );
        let done = edit("let b = 1;", "let b = 2;", 1).unwrap();
        assert_eq!(done.replacements, Some(1));
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "let a = 1;\nlet b = 2;\n"
        );
        edit("= 1;", "= 3;", 1).unwrap();
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "let a = 3;\nlet b = 2;\n"
        );
    }

    #[test]
    fn the_granted_policy_writes_only_inside_the_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let workspace = root.join("ws");
        fs::create_dir(&workspace).unwrap();
        let outside = root.join("new/dir/file.txt");
        let mut write = input(&outside, "x", WriteMode::Overwrite);
        write.create_dirs = true;
        let granted = Scope {
            workspace: &workspace,
            policy: ExecPolicy::Granted,
        };
        assert_eq!(
            granted.write(write.clone()).unwrap_err().code,
            "path_outside_workspace"
        );
        assert!(
            !root.join("new").exists(),
            "nothing created outside the workspace"
        );
        let mut inside = input(&workspace.join("made/here.txt"), "y", WriteMode::Overwrite);
        inside.create_dirs = true;
        assert!(granted.write(inside).unwrap().created);
        let account = Scope {
            workspace: &workspace,
            policy: ExecPolicy::Account,
        };
        let written = account.write(write).unwrap();
        assert!(written.created);
        assert_eq!(fs::read_to_string(&outside).unwrap(), "x");
    }

    /// A path whose missing part climbs back out (`nope/../..`) makes nothing outside.
    #[test]
    fn made_parents_never_climb_out_of_the_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let workspace = root.join("ws");
        fs::create_dir(&workspace).unwrap();
        let granted = Scope {
            workspace: &workspace,
            policy: ExecPolicy::Granted,
        };
        let climbing = format!("{}/nope/../../made/file.txt", workspace.display());
        let mut write = input(Path::new(&climbing), "x", WriteMode::Overwrite);
        write.create_dirs = true;
        assert!(granted.write(write).is_err());
        assert!(!root.join("made").exists(), "nothing made outside the workspace");
    }

    /// A link inside the workspace whose target is missing and outside: appending through it
    /// would create the file outside.
    #[cfg(unix)]
    #[test]
    fn a_dangling_link_out_of_the_workspace_is_not_written_through() {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let workspace = root.join("ws");
        fs::create_dir(&workspace).unwrap();
        let link = workspace.join("door");
        std::os::unix::fs::symlink(root.join("loot.txt"), &link).unwrap();
        let granted = Scope {
            workspace: &workspace,
            policy: ExecPolicy::Granted,
        };
        let refused = granted
            .write(input(&link, "x", WriteMode::Append))
            .unwrap_err();
        assert_eq!(refused.code, "path_outside_workspace");
        assert!(!root.join("loot.txt").exists(), "nothing written outside");
    }
}
