//! Private per-user local IPC. This authorizes an OS user, not individual applications.

use crate::contracts::*;
use std::{path::{Path, PathBuf}, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R, limit: usize) -> BridgeResult<Vec<u8>> {
    let operation = async {
        let length = reader.read_u32().await.map_err(io_error)? as usize;
        if length == 0 || length > limit {
            return Err(BridgeError::new("frame_size", format!("Frame must contain 1..={limit} bytes")));
        }
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes).await.map_err(io_error)?;
        Ok(bytes)
    };
    tokio::time::timeout(IO_TIMEOUT, operation).await
        .map_err(|_| BridgeError::new("ipc_timeout", "Timed out reading a local IPC frame"))?
}

pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, bytes: &[u8], limit: usize) -> BridgeResult<()> {
    if bytes.is_empty() || bytes.len() > limit {
        return Err(BridgeError::new("frame_size", "Serialized response exceeds the wire limit"));
    }
    tokio::time::timeout(IO_TIMEOUT, async {
        writer.write_u32(bytes.len() as u32).await.map_err(io_error)?;
        writer.write_all(bytes).await.map_err(io_error)?;
        writer.flush().await.map_err(io_error)
    }).await.map_err(|_| BridgeError::new("ipc_timeout", "Timed out writing a local IPC frame"))?
}

pub fn io_error(error: std::io::Error) -> BridgeError {
    BridgeError::new("ipc_io", error.to_string())
}

#[cfg(unix)]
pub async fn request(socket: &Path, body: RequestBody) -> BridgeResult<ResponseBody> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    if !socket.is_absolute() {
        return Err(BridgeError::new("socket_path", "The broker socket path must be absolute"));
    }
    let parent = socket.parent().ok_or_else(|| BridgeError::new("socket_path", "Missing socket directory"))?;
    validate_private_directory(parent)?;
    let metadata = std::fs::symlink_metadata(socket).map_err(io_error)?;
    let owner = unsafe { libc::geteuid() };
    if !metadata.file_type().is_socket() || metadata.uid() != owner || metadata.mode() & 0o077 != 0 {
        return Err(BridgeError::new("socket_permissions", "Broker socket must be an owner-only Unix socket"));
    }
    let mut stream = tokio::time::timeout(IO_TIMEOUT, tokio::net::UnixStream::connect(socket)).await
        .map_err(|_| BridgeError::new("ipc_timeout", "Timed out connecting to broker"))?.map_err(io_error)?;
    if stream.peer_cred().map_err(io_error)?.uid() != owner {
        return Err(BridgeError::new("peer_identity", "Broker peer UID does not match the local operator"));
    }
    let bytes = serde_json::to_vec(&WireRequest { api_version: API_VERSION, request: body })
        .map_err(|e| BridgeError::new("serialize", e.to_string()))?;
    write_frame(&mut stream, &bytes, MAX_FRAME_BYTES).await?;
    let bytes = read_frame(&mut stream, MAX_RESPONSE_BYTES).await?;
    let response: WireResponse = serde_json::from_slice(&bytes)
        .map_err(|e| BridgeError::new("invalid_response", e.to_string()))?;
    match response {
        WireResponse::Ok(body) => Ok(body),
        WireResponse::Error(error) => Err(error),
    }
}

#[cfg(not(unix))]
pub async fn request(_: &Path, _: RequestBody) -> BridgeResult<ResponseBody> {
    Err(BridgeError::new("unsupported_platform", "This proof currently implements Unix local IPC only"))
}

#[cfg(unix)]
pub fn validate_private_directory(path: &Path) -> BridgeResult<()> {
    use std::os::unix::fs::MetadataExt;
    if !path.is_absolute() || path.components().any(|p| matches!(p, std::path::Component::ParentDir)) {
        return Err(BridgeError::new("state_path", "State directory must be absolute and contain no parent traversal"));
    }
    let owner = unsafe { libc::geteuid() };
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        let metadata = std::fs::symlink_metadata(&current).map_err(io_error)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(BridgeError::new("state_path", "State directory and its ancestors must be actual directories"));
        }
        if metadata.uid() != owner && metadata.uid() != 0 {
            return Err(BridgeError::new("state_owner", "State directory ancestors must belong to the operator or root"));
        }
        let writable = metadata.mode() & 0o022 != 0;
        let trusted_sticky = metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
        if current != path && writable && !trusted_sticky {
            return Err(BridgeError::new("state_permissions", "A state directory ancestor is writable by another user"));
        }
    }
    let metadata = std::fs::symlink_metadata(path).map_err(io_error)?;
    if metadata.uid() != owner || metadata.mode() & 0o777 != 0o700 {
        return Err(BridgeError::new("state_permissions", "State directory must belong to the operator and have mode 0700"));
    }
    Ok(())
}

#[cfg(unix)]
pub struct StateGuard {
    _lock: std::fs::File,
    socket: PathBuf,
    owned_socket: Option<(u64, u64)>,
}

#[cfg(unix)]
impl StateGuard {
    pub fn acquire(directory: &Path) -> BridgeResult<Self> {
        use std::os::{fd::AsRawFd, unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt}};
        if !directory.is_absolute() || directory.components().any(|p| matches!(p, std::path::Component::ParentDir)) {
            return Err(BridgeError::new("state_path", "State directory must be an absolute path without parent traversal"));
        }
        if !directory.try_exists().map_err(io_error)? {
            let parent = directory.parent().ok_or_else(|| BridgeError::new("state_path", "Missing state parent"))?;
            // Validate all existing ancestors before creating anything. The leaf is checked below.
            validate_ancestors(parent)?;
            std::fs::DirBuilder::new().mode(0o700).create(directory).map_err(io_error)?;
        }
        validate_private_directory(directory)?;
        let lock = std::fs::OpenOptions::new().read(true).write(true).create(true)
            .mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(directory.join("broker.lock")).map_err(io_error)?;
        let metadata = lock.metadata().map_err(io_error)?;
        if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 || metadata.nlink() != 1 {
            return Err(BridgeError::new("lock_permissions", "Broker lock must be a private regular file with one link"));
        }
        let result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            return Err(BridgeError::new("broker_already_running", "Another broker holds this state directory; no socket or journal was changed"));
        }
        Ok(Self { _lock: lock, socket: directory.join("broker.sock"), owned_socket: None })
    }

    pub async fn bind(&mut self) -> BridgeResult<tokio::net::UnixListener> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
        match std::fs::symlink_metadata(&self.socket) {
            Ok(metadata) => {
                if !metadata.file_type().is_socket() || metadata.uid() != unsafe { libc::geteuid() } {
                    return Err(BridgeError::new("socket_collision", "Existing socket path is not an owned Unix socket; refusing to remove it"));
                }
                // The exclusive state lock proves no conforming broker owns this endpoint.
                // An accepting foreign socket must still never be unlinked.
                match tokio::time::timeout(Duration::from_secs(1), tokio::net::UnixStream::connect(&self.socket)).await {
                    Ok(Ok(_)) | Err(_) => return Err(BridgeError::new("socket_in_use", "Existing socket is active or unresponsive; refusing to replace it")),
                    Ok(Err(e)) if matches!(e.kind(), std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound) => {},
                    Ok(Err(e)) => return Err(io_error(e)),
                }
                std::fs::remove_file(&self.socket).map_err(io_error)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            Err(e) => return Err(io_error(e)),
        }
        let listener = tokio::net::UnixListener::bind(&self.socket).map_err(io_error)?;
        let metadata = std::fs::symlink_metadata(&self.socket).map_err(io_error)?;
        self.owned_socket = Some((metadata.dev(), metadata.ino()));
        std::fs::set_permissions(&self.socket, std::fs::Permissions::from_mode(0o600)).map_err(io_error)?;
        Ok(listener)
    }

    pub fn socket(&self) -> &Path { &self.socket }
}

#[cfg(unix)]
fn validate_ancestors(path: &Path) -> BridgeResult<()> {
    use std::os::unix::fs::MetadataExt;
    let owner = unsafe { libc::geteuid() };
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        let m = std::fs::symlink_metadata(&current).map_err(io_error)?;
        if !m.is_dir() || m.file_type().is_symlink() || (m.uid() != owner && m.uid() != 0)
            || (m.mode() & 0o022 != 0 && !(m.uid() == 0 && m.mode() & 0o1000 != 0)) {
            return Err(BridgeError::new("state_path", "State directory has an untrusted or symbolic-link ancestor"));
        }
    }
    Ok(())
}

#[cfg(unix)]
impl Drop for StateGuard {
    fn drop(&mut self) {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        if let (Some(identity), Ok(metadata)) = (self.owned_socket, std::fs::symlink_metadata(&self.socket)) {
            if metadata.file_type().is_socket() && identity == (metadata.dev(), metadata.ino()) {
                if let Err(error) = std::fs::remove_file(&self.socket) {
                    eprintln!("Could not remove broker socket: {error}");
                }
            }
        }
    }
}
