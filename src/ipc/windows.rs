//! Windows local IPC: a protected state directory private to the operator's
//! user SID, an exclusive instance lock, and a first-instance named pipe whose
//! explicit DACL grants only that SID. Default named-pipe security is not
//! treated as owner-only protection. Each end also checks that the other runs
//! as the operator's user.

use super::{IO_TIMEOUT, exchange, io_error};
use crate::contracts::*;
use crate::win32::{self, Entry, Operator, SecurityDescriptor};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, PipeMode, ServerOptions};
use tokio::time::{Instant, sleep};
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_SHARING_VIOLATION,
};
use windows_sys::Win32::Security::Authorization::SE_KERNEL_OBJECT;
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
};
use windows_sys::Win32::System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};

const PIPE_PREFIX: &str = r"\\.\pipe\";

fn identity_error(error: std::io::Error) -> BridgeError {
    BridgeError::new(
        "operator_identity",
        format!("Cannot read the operator's Windows identity: {error}"),
    )
}

/// Accepts connections on the broker's named pipe, keeping one listening
/// instance ready while each accepted instance serves its client.
pub struct Listener {
    endpoint: PathBuf,
    descriptor: SecurityDescriptor,
    server: NamedPipeServer,
}

/// One accepted client connection.
pub type Connection = NamedPipeServer;

impl Listener {
    pub async fn accept(&mut self) -> BridgeResult<Connection> {
        loop {
            match self.server.connect().await {
                Ok(()) => break,
                // The client connected and left before the connection was observed.
                Err(error) if error.raw_os_error() == Some(ERROR_NO_DATA as i32) => {
                    self.server = create_instance(&self.endpoint, &self.descriptor, false)
                        .map_err(io_error)?;
                }
                Err(error) => return Err(io_error(error)),
            }
        }
        let next = create_instance(&self.endpoint, &self.descriptor, false).map_err(io_error)?;
        Ok(std::mem::replace(&mut self.server, next))
    }
}

/// Rejects a caller whose process does not run as the broker operator's user.
pub fn verify_peer(connection: &Connection) -> BridgeResult<()> {
    let operator = Operator::current().map_err(identity_error)?;
    let mut pid = 0_u32;
    if unsafe { GetNamedPipeClientProcessId(connection.as_raw_handle(), &mut pid) } == 0 {
        return Err(BridgeError::new(
            "peer_identity",
            format!(
                "Cannot identify the caller process: {}",
                std::io::Error::last_os_error()
            ),
        ));
    }
    let caller = win32::process_user(pid).map_err(|error| {
        BridgeError::new(
            "peer_identity",
            format!("Cannot read the caller's user: {error}"),
        )
    })?;
    if !caller.equals(&operator.user) {
        return Err(BridgeError::new(
            "peer_identity",
            "Caller user does not match the broker operator",
        ));
    }
    Ok(())
}

fn create_instance(
    endpoint: &Path,
    descriptor: &SecurityDescriptor,
    first: bool,
) -> std::io::Result<NamedPipeServer> {
    let mut attributes = descriptor.attributes();
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .pipe_mode(PipeMode::Byte);
    // `attributes` and the descriptor it points at outlive this call.
    unsafe {
        options.create_with_security_attributes_raw(
            endpoint,
            (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
        )
    }
}

/// The endpoint must name a pipe in the local pipe namespace.
fn validate_endpoint(endpoint: &Path) -> BridgeResult<()> {
    let valid = endpoint
        .to_str()
        .and_then(|text| text.strip_prefix(PIPE_PREFIX))
        .is_some_and(|name| !name.is_empty() && name.len() <= 240 && !name.contains(['\\', '\0']));
    if !valid {
        return Err(BridgeError::new(
            "socket_path",
            r"The broker endpoint must be a local named pipe path such as \\.\pipe\devagotchi-broker-…",
        ));
    }
    Ok(())
}

pub async fn request(endpoint: &Path, body: RequestBody) -> BridgeResult<ResponseBody> {
    validate_endpoint(endpoint)?;
    let operator = Operator::current().map_err(identity_error)?;
    let deadline = Instant::now() + IO_TIMEOUT;
    let mut client = loop {
        match ClientOptions::new().open(endpoint) {
            Ok(client) => break client,
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                if Instant::now() >= deadline {
                    return Err(BridgeError::new(
                        "ipc_timeout",
                        "Timed out connecting to broker",
                    ));
                }
                sleep(Duration::from_millis(20)).await;
            }
            Err(error) => return Err(io_error(error)),
        }
    };
    // A pipe created first by another account, or by the operator without the
    // broker's DACL, is not the broker's endpoint.
    let security =
        win32::object_security(client.as_raw_handle(), SE_KERNEL_OBJECT).map_err(io_error)?;
    if !security.owner.equals(&operator.user) || !security.grants_only(&operator.user) {
        return Err(BridgeError::new(
            "socket_permissions",
            "Broker pipe must be owned by the operator and grant access to the operator only",
        ));
    }
    let mut pid = 0_u32;
    if unsafe { GetNamedPipeServerProcessId(client.as_raw_handle(), &mut pid) } == 0 {
        return Err(BridgeError::new(
            "peer_identity",
            format!(
                "Cannot identify the broker process: {}",
                std::io::Error::last_os_error()
            ),
        ));
    }
    let server = win32::process_user(pid).map_err(|error| {
        BridgeError::new(
            "peer_identity",
            format!("Cannot read the broker's user: {error}"),
        )
    })?;
    if !server.equals(&operator.user) {
        return Err(BridgeError::new(
            "peer_identity",
            "Broker peer user does not match the local operator",
        ));
    }
    exchange(&mut client, body).await
}

fn absolute_without_traversal(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
}

/// Existing ancestors must be actual directories, not reparse points.
fn validate_ancestors(path: &Path) -> BridgeResult<()> {
    for ancestor in path.ancestors() {
        let metadata = std::fs::symlink_metadata(ancestor).map_err(io_error)?;
        let attributes = metadata.file_attributes();
        if attributes & FILE_ATTRIBUTE_DIRECTORY == 0
            || attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Err(BridgeError::new(
                "state_path",
                "State directory has a reparse-point or non-directory ancestor",
            ));
        }
    }
    Ok(())
}

fn validate_private_directory(path: &Path, operator: &Operator) -> BridgeResult<()> {
    let entry = win32::open_entry(path).map_err(io_error)?;
    if let Some(defect) =
        win32::private_entry_defect(&entry, Entry::Directory, operator).map_err(io_error)?
    {
        return Err(BridgeError::new(
            "state_permissions",
            format!(
                "State directory {defect}; it must be created by the broker, owned by the operator's user, and private to that user"
            ),
        ));
    }
    Ok(())
}

/// The pipe name is derived from the operator's SID and the canonical state
/// directory, so each operator and state directory has its own endpoint.
fn endpoint_for(directory: &Path, operator: &Operator) -> BridgeResult<PathBuf> {
    let canonical = dunce::canonicalize(directory).map_err(io_error)?;
    let canonical = canonical.to_str().ok_or_else(|| {
        BridgeError::new(
            "state_path",
            "State directory must be representable as UTF-8",
        )
    })?;
    let sid = operator.user.to_sddl().map_err(identity_error)?;
    let digest = Sha256::digest(format!("devagotchi-broker-pipe-v1\0{sid}\0{canonical}"));
    Ok(PathBuf::from(format!(
        "{PIPE_PREFIX}devagotchi-broker-{}",
        &hex::encode(digest)[..32]
    )))
}

pub struct StateGuard {
    _lock: File,
    endpoint: PathBuf,
    operator: Operator,
}

impl StateGuard {
    pub fn acquire(directory: &Path) -> BridgeResult<Self> {
        if !absolute_without_traversal(directory) {
            return Err(BridgeError::new(
                "state_path",
                "State directory must be an absolute path without parent traversal",
            ));
        }
        let operator = Operator::current().map_err(identity_error)?;
        if !directory.try_exists().map_err(io_error)? {
            let parent = directory
                .parent()
                .ok_or_else(|| BridgeError::new("state_path", "Missing state parent"))?;
            // Validate all existing ancestors before creating anything. The leaf is checked below.
            validate_ancestors(parent)?;
            win32::create_private_directory(directory, &operator.user).map_err(io_error)?;
        }
        validate_private_directory(directory, &operator)?;
        // Share mode zero makes the open handle the instance lock; reparse
        // points are opened as themselves and then rejected.
        let lock = match OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .share_mode(0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(directory.join("broker.lock"))
        {
            Ok(lock) => lock,
            Err(error) if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32) => {
                return Err(BridgeError::new(
                    "broker_already_running",
                    "Another broker holds this state directory; no endpoint or journal was changed",
                ));
            }
            Err(error) => return Err(io_error(error)),
        };
        if win32::private_entry_defect(&lock, Entry::File, &operator)
            .map_err(io_error)?
            .is_some()
        {
            return Err(BridgeError::new(
                "lock_permissions",
                "Broker lock must be a private regular file with one link",
            ));
        }
        let endpoint = endpoint_for(directory, &operator)?;
        Ok(Self {
            _lock: lock,
            endpoint,
            operator,
        })
    }

    /// Creates the first instance of the broker's pipe. An existing pipe with
    /// this name, whoever created it, is never shared.
    pub async fn bind(&mut self) -> BridgeResult<Listener> {
        let descriptor =
            SecurityDescriptor::private_to(&self.operator.user, false).map_err(identity_error)?;
        let server = create_instance(&self.endpoint, &descriptor, true).map_err(|error| {
            if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
                BridgeError::new(
                    "socket_in_use",
                    "A pipe with this broker endpoint already exists; refusing to share it",
                )
            } else {
                io_error(error)
            }
        })?;
        Ok(Listener {
            endpoint: self.endpoint.clone(),
            descriptor,
            server,
        })
    }

    /// The endpoint clients pass to `request`: the broker's named-pipe path.
    pub fn endpoint(&self) -> &Path {
        &self.endpoint
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_paths_are_local_pipe_names_only() {
        assert!(validate_endpoint(Path::new(r"\\.\pipe\devagotchi-broker-0123")).is_ok());
        for invalid in [
            r"\\.\pipe\",
            r"\\server\pipe\name",
            r"C:\state\broker.sock",
            r"\\.\pipe\a\b",
        ] {
            assert!(validate_endpoint(Path::new(invalid)).is_err(), "{invalid}");
        }
    }

    #[tokio::test]
    async fn the_state_directory_is_private_locked_and_served_only_to_the_operator() {
        let parent = tempfile::tempdir().unwrap();
        let state = parent.path().join("state");
        let mut guard = StateGuard::acquire(&state).unwrap();
        // A second broker cannot take the same state directory.
        assert_eq!(
            StateGuard::acquire(&state).err().unwrap().code,
            "broker_already_running"
        );
        let mut listener = guard.bind().await.unwrap();
        // The first-instance rule refuses a second pipe with the same name.
        let mut squatter = StateGuard {
            _lock: tempfile::tempfile().unwrap(),
            endpoint: guard.endpoint().to_path_buf(),
            operator: Operator::current().unwrap(),
        };
        assert_eq!(squatter.bind().await.err().unwrap().code, "socket_in_use");
        let server = tokio::spawn(async move {
            let mut connection = listener.accept().await.unwrap();
            verify_peer(&connection).unwrap();
            let raw = super::super::read_frame(&mut connection, MAX_FRAME_BYTES)
                .await
                .unwrap();
            let request: WireRequest = serde_json::from_slice(&raw).unwrap();
            assert!(matches!(request.request, RequestBody::OperationList(_)));
            let response = WireResponse::Ok(ResponseBody::Operations(OperationsPage {
                operations: vec![],
                next_offset: None,
            }));
            super::super::write_frame(
                &mut connection,
                &serde_json::to_vec(&response).unwrap(),
                super::super::MAX_RESPONSE_BYTES,
            )
            .await
            .unwrap();
        });
        let response = request(
            guard.endpoint(),
            RequestBody::OperationList(ListInput {
                offset: 0,
                limit: 1,
            }),
        )
        .await
        .unwrap();
        assert!(matches!(response, ResponseBody::Operations(page) if page.operations.is_empty()));
        server.await.unwrap();
        drop(guard);
        // The protected directory is reusable by the next broker.
        StateGuard::acquire(&state).unwrap();
    }

    #[test]
    fn a_directory_with_inherited_permissions_is_not_private_state() {
        let parent = tempfile::tempdir().unwrap();
        let state = parent.path().join("state");
        std::fs::create_dir(&state).unwrap();
        assert_eq!(
            StateGuard::acquire(&state).err().unwrap().code,
            "state_permissions"
        );
    }
}
