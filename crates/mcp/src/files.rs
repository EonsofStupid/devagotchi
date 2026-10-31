//! `read_multiple_files`: several files in one call, over whatever reaches the broker (this
//! machine's local IPC, or the hub's link to a machine). Each is one bounded read; a file
//! that cannot be read is reported beside the others, never failing the call.

use std::future::Future;

use devagotchi_contracts::{
    BridgeError, FileContent, FileReadInput, MAX_FILE_READ_BYTES, RequestBody, ResponseBody,
};
use schemars::JsonSchema;
use serde::Serialize;

use crate::unexpected;

/// Most files one call reads.
pub const MAX_FILES: usize = 20;

#[derive(Debug, Serialize, JsonSchema)]
pub struct FileResult {
    /// The path as it was asked for.
    pub path: String,
    /// The file's first page (read on with read_file from `next_offset`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<FileContent>,
    /// Why this file could not be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<BridgeError>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Files {
    /// One entry per path, in the order asked.
    pub files: Vec<FileResult>,
}

/// Read the first `max_bytes` (1..=24576) of each of `paths` (1..=20) through `call`.
pub async fn read_many<C, F>(call: C, paths: Vec<String>, max_bytes: usize) -> Result<Files, BridgeError>
where
    C: Fn(RequestBody) -> F,
    F: Future<Output = Result<ResponseBody, BridgeError>>,
{
    if !(1..=MAX_FILES).contains(&paths.len()) {
        return Err(BridgeError::new(
            "invalid_request",
            format!("paths must hold 1..={MAX_FILES} files"),
        ));
    }
    if !(1..=MAX_FILE_READ_BYTES).contains(&max_bytes) {
        return Err(BridgeError::new(
            "invalid_request",
            format!("max_bytes_each must be 1..={MAX_FILE_READ_BYTES}"),
        ));
    }
    let mut files = Vec::with_capacity(paths.len());
    for path in paths {
        let read = call(RequestBody::FileRead(FileReadInput {
            path: path.clone(),
            offset: 0,
            max_bytes,
        }))
        .await;
        let (content, error) = match read {
            Ok(ResponseBody::File(content)) => (Some(content), None),
            Ok(other) => (None, Some(unexpected("file", &other))),
            Err(error) => (None, Some(error)),
        };
        files.push(FileResult {
            path,
            content,
            error,
        });
    }
    Ok(Files { files })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(path: &str, text: &str) -> FileContent {
        FileContent {
            path: path.into(),
            size: text.len() as u64,
            offset: 0,
            text: Some(text.into()),
            bytes_base64: None,
            next_offset: text.len() as u64,
            eof: true,
            modified_ms: None,
        }
    }

    #[tokio::test]
    async fn every_file_is_answered_in_order_and_one_failure_spoils_nothing() {
        let call = |body: RequestBody| async move {
            let RequestBody::FileRead(input) = body else {
                panic!("only reads")
            };
            assert_eq!(input.max_bytes, 100);
            match input.path.as_str() {
                "/missing" => Err(BridgeError::new("path_not_found", "/missing does not exist")),
                path => Ok(ResponseBody::File(content(path, "hello"))),
            }
        };
        let paths = vec!["/a".to_owned(), "/missing".to_owned(), "/b".to_owned()];
        let files = read_many(call, paths, 100).await.unwrap().files;
        let asked: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(asked, ["/a", "/missing", "/b"]);
        assert_eq!(files[0].content.as_ref().unwrap().text.as_deref(), Some("hello"));
        assert_eq!(files[1].error.as_ref().unwrap().code, "path_not_found");
        assert!(files[1].content.is_none());
        assert!(files[2].content.is_some());
    }

    fn nothing_is_read(_: RequestBody) -> std::future::Ready<Result<ResponseBody, BridgeError>> {
        panic!("a refused call reads nothing")
    }

    #[tokio::test]
    async fn too_many_or_none_or_oversized_pages_are_refused() {
        assert!(read_many(nothing_is_read, Vec::new(), 100).await.is_err());
        let many = vec!["/a".to_owned(); MAX_FILES + 1];
        assert!(read_many(nothing_is_read, many, 100).await.is_err());
        let one = vec!["/a".to_owned()];
        assert!(read_many(nothing_is_read, one, MAX_FILE_READ_BYTES + 1).await.is_err());
    }
}
