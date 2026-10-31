//! `read_image`: an image file handed to the AI as an image it can see, over whatever
//! reaches the broker (this machine's local IPC, or the hub's link to a machine). The
//! broker reads bounded pages; this reads them in order, checks the bytes are a PNG, JPEG,
//! GIF or WebP image, and returns them as one MCP image block.

use std::future::Future;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use devagotchi_contracts::{
    BridgeError, FileReadInput, MAX_FILE_READ_BYTES, RequestBody, ResponseBody,
};

use rmcp::model::{CallToolResult, ContentBlock};

use crate::unexpected;

/// Largest image returned: its base64 stays under the 5 MB an AI client takes per image.
pub const MAX_IMAGE_BYTES: u64 = 3_750_000;

/// An image as read: its bytes, base64, and what it is.
#[derive(Debug)]
pub struct Image {
    pub path: String,
    pub mime_type: &'static str,
    pub size: u64,
    pub base64: String,
}

impl Image {
    /// The tool's answer: a line saying what it is, then the image itself.
    pub fn into_result(self) -> CallToolResult {
        CallToolResult::success(vec![
            ContentBlock::text(format!("{}: {}, {} bytes", self.path, self.mime_type, self.size)),
            ContentBlock::image(self.base64, self.mime_type),
        ])
    }
}

/// The image type from the file's first bytes, never from its name.
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, ..] => Some("image/png"),
        [0xFF, 0xD8, 0xFF, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', b'7' | b'9', b'a', ..] => Some("image/gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("image/webp"),
        _ => None,
    }
}

fn page_bytes(text: Option<String>, base64: Option<String>) -> Result<Vec<u8>, BridgeError> {
    match (text, base64) {
        (Some(text), _) => Ok(text.into_bytes()),
        (None, Some(encoded)) => BASE64
            .decode(encoded)
            .map_err(|_| BridgeError::new("invalid_response", "the broker's bytes are not base64")),
        (None, None) => Ok(Vec::new()),
    }
}

/// Read the image at `path` through `call`, page by page.
pub async fn read_image<C, F>(call: C, path: String) -> Result<Image, BridgeError>
where
    C: Fn(RequestBody) -> F,
    F: Future<Output = Result<ResponseBody, BridgeError>>,
{
    let mut bytes = Vec::new();
    let mut offset = 0u64;
    let mut shown = path.clone();
    loop {
        let page = match call(RequestBody::FileRead(FileReadInput {
            path: path.clone(),
            offset: offset as i64,
            max_bytes: MAX_FILE_READ_BYTES,
            start_line: None,
            line_count: None,
        }))
        .await?
        {
            ResponseBody::File(page) => page,
            other => return Err(unexpected("file", &other)),
        };
        if page.size > MAX_IMAGE_BYTES {
            return Err(BridgeError::new(
                "too_large",
                format!(
                    "{} is {} bytes; read_image returns images up to {MAX_IMAGE_BYTES} bytes (scale it down with run first)",
                    page.path, page.size
                ),
            ));
        }
        if page.offset != offset {
            return Err(BridgeError::new(
                "changed_while_reading",
                format!("{} changed while it was read; read it again", page.path),
            ));
        }
        if offset == 0 {
            shown = page.path.clone();
        }
        let next = page.next_offset;
        let eof = page.eof;
        bytes.extend(page_bytes(page.text, page.bytes_base64)?);
        if offset == 0 && sniff(&bytes).is_none() {
            return Err(BridgeError::new(
                "not_an_image",
                format!("{shown} is not a PNG, JPEG, GIF or WebP image; read_file reads its bytes"),
            ));
        }
        if eof || next == offset {
            let mime_type = sniff(&bytes).ok_or_else(|| {
                BridgeError::new(
                    "not_an_image",
                    format!("{shown} is not a PNG, JPEG, GIF or WebP image"),
                )
            })?;
            return Ok(Image {
                path: shown,
                mime_type,
                size: bytes.len() as u64,
                base64: BASE64.encode(&bytes),
            });
        }
        offset = next;
    }
}

#[cfg(test)]
mod tests {
    use devagotchi_contracts::FileContent;

    use super::*;

    /// A broker holding `bytes` at `/img`, answering reads as the real one does: pages of
    /// at most the asked size, binary as base64.
    fn broker(bytes: Vec<u8>) -> impl Fn(RequestBody) -> std::future::Ready<Result<ResponseBody, BridgeError>> {
        move |body| {
            let RequestBody::FileRead(input) = body else {
                panic!("only reads")
            };
            let start = (input.offset as usize).min(bytes.len());
            let end = (start + input.max_bytes).min(bytes.len());
            std::future::ready(Ok(ResponseBody::File(FileContent {
                path: "/img".into(),
                size: bytes.len() as u64,
                offset: start as u64,
                text: None,
                bytes_base64: Some(BASE64.encode(&bytes[start..end])),
                next_offset: end as u64,
                eof: end >= bytes.len(),
                modified_ms: None,
                start_line: None,
                next_line: None,
                total_lines: None,
            })))
        }
    }

    fn png(len: usize) -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.extend((0..len).map(|i| (i % 251) as u8));
        bytes
    }

    #[tokio::test]
    async fn an_image_over_many_pages_comes_back_whole() {
        let bytes = png(3 * MAX_FILE_READ_BYTES + 17);
        let image = read_image(broker(bytes.clone()), "/img".into()).await.unwrap();
        assert_eq!(image.mime_type, "image/png");
        assert_eq!(image.size, bytes.len() as u64);
        assert_eq!(BASE64.decode(image.base64).unwrap(), bytes);
    }

    #[tokio::test]
    async fn what_is_not_an_image_or_too_large_is_refused_at_the_first_page() {
        let text = read_image(broker(b"just words".to_vec()), "/img".into()).await;
        assert_eq!(text.unwrap_err().code, "not_an_image");
        let huge = read_image(broker(png(MAX_IMAGE_BYTES as usize)), "/img".into()).await;
        assert_eq!(huge.unwrap_err().code, "too_large");
    }

    #[test]
    fn types_come_from_the_first_bytes() {
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff(b"GIF89a...."), Some("image/gif"));
        assert_eq!(sniff(b"RIFF\x10\x00\x00\x00WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff(b"RIFF\x10\x00\x00\x00WAVEfmt "), None);
        assert_eq!(sniff(b"<svg"), None);
    }
}
