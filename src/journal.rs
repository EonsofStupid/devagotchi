//! Durable broker records. Every mutation commits with SQLite synchronous=FULL.
//! OS process identifiers are deliberately never persisted or recovered.

use crate::contracts::{BridgeError, BridgeResult, OperationView};
use rusqlite::{Connection, params};
use std::collections::VecDeque;
#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone)]
pub(crate) struct JournalFrame {
    pub sequence: u64,
    pub stream: String,
    pub bytes: Vec<u8>,
    pub text: String,
}

#[derive(Debug, Clone)]
pub(crate) struct JournalOperation {
    pub key: String,
    pub request_digest: String,
    pub config_identity: String,
    pub view: OperationView,
    pub latest_sequence: u64,
    pub frames: VecDeque<JournalFrame>,
}

pub(crate) struct Journal {
    connection: Connection,
}

fn persistence(error: impl std::fmt::Display) -> BridgeError {
    BridgeError::new("persistence_error", format!("SQLite journal: {error}"))
}

fn collision(path: &Path) -> BridgeError {
    persistence(format!(
        "Refusing journal collision at {}: expected an operator-owned private regular file with one link",
        path.display()
    ))
}

#[cfg(unix)]
fn validate_private_file(path: &Path) -> BridgeResult<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(persistence(error)),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(collision(path));
    }
    Ok(())
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> std::io::Result<()> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map(drop)
}

/// The entry is inspected through its own handle; a reparse point is opened as
/// itself, never followed.
#[cfg(windows)]
fn validate_private_file(path: &Path) -> BridgeResult<()> {
    use crate::win32::{self, Entry, Operator};
    let entry = match win32::open_entry(path) {
        Ok(entry) => entry,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(persistence(error)),
    };
    let operator = Operator::current().map_err(persistence)?;
    if win32::private_entry_defect(&entry, Entry::File, &operator)
        .map_err(persistence)?
        .is_some()
    {
        return Err(collision(path));
    }
    Ok(())
}

#[cfg(windows)]
fn create_private_file(path: &Path) -> std::io::Result<()> {
    let operator = crate::win32::Operator::current()?;
    crate::win32::create_private_file(path, &operator.user)
}

impl Journal {
    pub(crate) fn open(state_dir: &Path) -> BridgeResult<Self> {
        let path = state_dir.join("operations.sqlite3");
        for name in [
            "operations.sqlite3",
            "operations.sqlite3-wal",
            "operations.sqlite3-shm",
            "operations.sqlite3-journal",
        ] {
            validate_private_file(&state_dir.join(name))?;
        }
        match create_private_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(persistence(error)),
        }
        validate_private_file(&path)?;
        let connection = Connection::open(path).map_err(persistence)?;
        connection
            .busy_timeout(Duration::from_millis(1000))
            .map_err(persistence)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS operations (
                 operation_id TEXT PRIMARY KEY NOT NULL,
                 idempotency_key TEXT UNIQUE NOT NULL,
                 request_digest TEXT NOT NULL,
                 config_identity TEXT NOT NULL,
                 view_json TEXT NOT NULL,
                 latest_sequence INTEGER NOT NULL DEFAULT 0 CHECK(latest_sequence >= 0)
             );
             CREATE TABLE IF NOT EXISTS output_frames (
                 operation_id TEXT NOT NULL REFERENCES operations(operation_id),
                 sequence INTEGER NOT NULL CHECK(sequence > 0),
                 stream TEXT NOT NULL CHECK(stream IN ('stdout', 'stderr')),
                 raw_bytes BLOB NOT NULL,
                 decoded_text TEXT NOT NULL,
                 PRIMARY KEY(operation_id, sequence)
             );",
            )
            .map_err(persistence)?;
        let synchronous: i64 = connection
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .map_err(persistence)?;
        if synchronous != 2 {
            return Err(persistence("synchronous=FULL could not be established"));
        }
        Ok(Self { connection })
    }

    pub(crate) fn load(&self) -> BridgeResult<Vec<JournalOperation>> {
        let count: i64 = self
            .connection
            .query_row("SELECT COUNT(*) FROM operations", [], |row| row.get(0))
            .map_err(persistence)?;
        if count > 4096 {
            return Err(persistence(
                "journal contains more than the supported 4096 operations",
            ));
        }
        let mut statement = self.connection.prepare(
            "SELECT operation_id, idempotency_key, request_digest, config_identity, view_json, latest_sequence FROM operations ORDER BY rowid"
        ).map_err(persistence)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, u64>(5)?,
                ))
            })
            .map_err(persistence)?;
        let mut operations = Vec::new();
        for row in rows {
            let (id, key, request_digest, config_identity, view_json, latest_sequence) =
                row.map_err(persistence)?;
            let view: OperationView = serde_json::from_str(&view_json).map_err(persistence)?;
            if view.operation_id != id || uuid::Uuid::parse_str(&id).is_err() {
                return Err(persistence("operation identity in the journal is invalid"));
            }
            let frame_count: i64 = self
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM output_frames WHERE operation_id=?1",
                    [&id],
                    |row| row.get(0),
                )
                .map_err(persistence)?;
            if frame_count > 1024 {
                return Err(persistence(
                    "operation exceeds the journal frame-count bound",
                ));
            }
            let mut frame_statement = self.connection.prepare(
                "SELECT sequence, stream, raw_bytes, decoded_text FROM output_frames WHERE operation_id=?1 ORDER BY sequence"
            ).map_err(persistence)?;
            let frame_rows = frame_statement
                .query_map([&id], |row| {
                    Ok(JournalFrame {
                        sequence: row.get(0)?,
                        stream: row.get(1)?,
                        bytes: row.get(2)?,
                        text: row.get(3)?,
                    })
                })
                .map_err(persistence)?;
            let mut frames = VecDeque::new();
            for frame in frame_rows {
                let frame = frame.map_err(persistence)?;
                if frame.bytes.len() > 1024
                    || frame.sequence == 0
                    || frame.sequence > latest_sequence
                {
                    return Err(persistence("invalid retained output frame"));
                }
                frames.push_back(frame);
            }
            operations.push(JournalOperation {
                key,
                request_digest,
                config_identity,
                view,
                latest_sequence,
                frames,
            });
        }
        Ok(operations)
    }

    pub(crate) fn insert(&mut self, operation: &JournalOperation) -> BridgeResult<()> {
        let view = serde_json::to_string(&operation.view).map_err(persistence)?;
        let transaction = self.connection.transaction().map_err(persistence)?;
        transaction.execute(
            "INSERT INTO operations(operation_id,idempotency_key,request_digest,config_identity,view_json,latest_sequence) VALUES(?1,?2,?3,?4,?5,0)",
            params![operation.view.operation_id, operation.key, operation.request_digest, operation.config_identity, view]
        ).map_err(persistence)?;
        transaction.commit().map_err(persistence)
    }

    pub(crate) fn update(&mut self, view: &OperationView) -> BridgeResult<()> {
        let json = serde_json::to_string(view).map_err(persistence)?;
        let changed = self
            .connection
            .execute(
                "UPDATE operations SET view_json=?1 WHERE operation_id=?2",
                params![json, view.operation_id],
            )
            .map_err(persistence)?;
        if changed != 1 {
            return Err(persistence(
                "state update did not identify exactly one operation",
            ));
        }
        Ok(())
    }

    pub(crate) fn append(
        &mut self,
        operation_id: &str,
        frame: &JournalFrame,
        earliest: u64,
    ) -> BridgeResult<()> {
        let transaction = self.connection.transaction().map_err(persistence)?;
        transaction.execute(
            "INSERT INTO output_frames(operation_id,sequence,stream,raw_bytes,decoded_text) VALUES(?1,?2,?3,?4,?5)",
            params![operation_id, frame.sequence, frame.stream, frame.bytes, frame.text]
        ).map_err(persistence)?;
        transaction
            .execute(
                "DELETE FROM output_frames WHERE operation_id=?1 AND sequence<?2",
                params![operation_id, earliest],
            )
            .map_err(persistence)?;
        let changed = transaction
            .execute(
                "UPDATE operations SET latest_sequence=?1 WHERE operation_id=?2",
                params![frame.sequence, operation_id],
            )
            .map_err(persistence)?;
        if changed != 1 {
            return Err(persistence(
                "output update did not identify exactly one operation",
            ));
        }
        transaction.commit().map_err(persistence)
    }

    pub(crate) fn prune(&mut self, operation_id: &str, earliest: u64) -> BridgeResult<()> {
        self.connection
            .execute(
                "DELETE FROM output_frames WHERE operation_id=?1 AND sequence<?2",
                params![operation_id, earliest],
            )
            .map_err(persistence)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::OperationState;

    #[test]
    fn journal_reopens_exact_bytes_and_idempotency_evidence() {
        let directory = tempfile::tempdir().unwrap();
        let state = crate::ipc::private_test_state(directory.path());
        let id = uuid::Uuid::new_v4().to_string();
        {
            let mut journal = Journal::open(&state).unwrap();
            journal
                .insert(&JournalOperation {
                    key: "retry-key".into(),
                    request_digest: "request-digest".into(),
                    config_identity: "configuration-digest".into(),
                    view: OperationView {
                        operation_id: id.clone(),
                        executable: "/usr/bin/test".into(),
                        args: vec![],
                        cwd: "/tmp".into(),
                        deadline_ms: 1000,
                        state: OperationState::Accepted,
                        created_at_ms: 1,
                        updated_at_ms: 1,
                        exit_code: None,
                        detail: None,
                        ownership_scope: "test".into(),
                        cancellation_requested: false,
                        output_complete: false,
                    },
                    latest_sequence: 0,
                    frames: VecDeque::new(),
                })
                .unwrap();
            journal
                .append(
                    &id,
                    &JournalFrame {
                        sequence: 1,
                        stream: "stdout".into(),
                        bytes: vec![0, 255, 128],
                        text: "\0��".into(),
                    },
                    1,
                )
                .unwrap();
        }
        let journal = Journal::open(&state).unwrap();
        let loaded = journal.load().unwrap();
        assert_eq!(loaded[0].key, "retry-key");
        assert_eq!(loaded[0].config_identity, "configuration-digest");
        assert_eq!(loaded[0].frames[0].bytes, [0, 255, 128]);
        assert_eq!(loaded[0].latest_sequence, 1);
        assert!(matches!(loaded[0].view.state, OperationState::Accepted));
    }

    #[cfg(unix)]
    #[test]
    fn journal_rejects_hardlinks_and_sidecar_collisions_without_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("unrelated");
        std::fs::write(&target, b"keep this exact content").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        let state = directory.path().join("state");
        std::fs::create_dir(&state).unwrap();
        std::fs::hard_link(&target, state.join("operations.sqlite3")).unwrap();
        assert!(Journal::open(&state).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"keep this exact content");
        std::fs::remove_file(state.join("operations.sqlite3")).unwrap();
        std::os::unix::fs::symlink(&target, state.join("operations.sqlite3-wal")).unwrap();
        assert!(Journal::open(&state).is_err());
        assert!(!state.join("operations.sqlite3").exists());
        assert_eq!(std::fs::read(&target).unwrap(), b"keep this exact content");
    }

    #[cfg(windows)]
    #[test]
    fn journal_rejects_hardlinks_and_sidecar_collisions_without_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("unrelated");
        std::fs::write(&target, b"keep this exact content").unwrap();
        let state = crate::ipc::private_test_state(directory.path());
        std::fs::hard_link(&target, state.join("operations.sqlite3")).unwrap();
        assert!(Journal::open(&state).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"keep this exact content");
        std::fs::remove_file(state.join("operations.sqlite3")).unwrap();
        std::fs::hard_link(&target, state.join("operations.sqlite3-wal")).unwrap();
        assert!(Journal::open(&state).is_err());
        assert!(!state.join("operations.sqlite3").exists());
        assert_eq!(std::fs::read(&target).unwrap(), b"keep this exact content");
    }
}
