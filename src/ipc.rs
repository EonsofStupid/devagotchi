//! Private per-user local IPC. This authorizes an OS user, not individual applications.
//! Framing and the request exchange are shared; endpoint creation, privacy, and
//! peer identity are native (`unix`, `windows`).

use crate::contracts::*;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

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

/// One framed request and response on an already authenticated connection.
async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S, body: RequestBody) -> BridgeResult<ResponseBody> {
    let bytes = serde_json::to_vec(&WireRequest { api_version: API_VERSION, request: body })
        .map_err(|e| BridgeError::new("serialize", e.to_string()))?;
    write_frame(stream, &bytes, MAX_FRAME_BYTES).await?;
    let bytes = read_frame(stream, MAX_RESPONSE_BYTES).await?;
    let response: WireResponse = serde_json::from_slice(&bytes)
        .map_err(|e| BridgeError::new("invalid_response", e.to_string()))?;
    match response {
        WireResponse::Ok(body) => Ok(body),
        WireResponse::Error(error) => Err(error),
    }
}

/// A private state directory for tests, created as the broker creates one.
#[cfg(test)]
pub(crate) fn private_test_state(parent: &std::path::Path) -> std::path::PathBuf {
    #[cfg(unix)]
    {
        // tempfile creates its directories with mode 0700.
        parent.to_path_buf()
    }
    #[cfg(windows)]
    {
        let state = parent.join("state");
        let operator = crate::win32::Operator::current().unwrap();
        crate::win32::create_private_directory(&state, &operator.user).unwrap();
        state
    }
}
