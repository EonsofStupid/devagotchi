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
/// Widest or tallest image returned: the Claude API refuses a larger one, and an image it
/// refused stays in the conversation and fails every later request.
pub const MAX_IMAGE_SIDE: u32 = 8000;

/// An image as read: its bytes, base64, and what it is.
#[derive(Debug)]
pub struct Image {
    pub path: String,
    pub mime_type: &'static str,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    pub base64: String,
}

impl Image {
    /// The tool's answer: a line saying what it is, then the image itself.
    pub fn into_result(self) -> CallToolResult {
        CallToolResult::success(vec![
            ContentBlock::text(format!(
                "{}: {}, {}x{}, {} bytes",
                self.path, self.mime_type, self.width, self.height, self.size
            )),
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

fn be16(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?)))
}

fn le16(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?)))
}

fn le24(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at + 3)?;
    Some(u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16)
}

/// Width and height from the image's own header: PNG's IHDR, GIF's screen, JPEG's frame
/// (SOFn) marker, WebP's VP8, VP8L or VP8X chunk.
pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    match sniff(bytes)? {
        "image/png" => {
            let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
            let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
            Some((width, height))
        }
        "image/gif" => Some((le16(bytes, 6)?, le16(bytes, 8)?)),
        "image/webp" => match bytes.get(12..16)? {
            b"VP8X" => Some((le24(bytes, 24)? + 1, le24(bytes, 27)? + 1)),
            b"VP8L" => {
                let bits = u32::from_le_bytes(bytes.get(21..25)?.try_into().ok()?);
                Some(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1))
            }
            b"VP8 " => Some((le16(bytes, 26)? & 0x3FFF, le16(bytes, 28)? & 0x3FFF)),
            _ => None,
        },
        _ => {
            // JPEG: walk the segments to the first frame header.
            let mut at = 2;
            loop {
                while *bytes.get(at)? != 0xFF {
                    at += 1;
                }
                while *bytes.get(at)? == 0xFF {
                    at += 1;
                }
                let marker = *bytes.get(at)?;
                at += 1;
                if matches!(marker, 0xD8 | 0x01 | 0xD0..=0xD7) {
                    continue;
                }
                let length = be16(bytes, at)? as usize;
                if matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
                    return Some((be16(bytes, at + 5)?, be16(bytes, at + 3)?));
                }
                if length < 2 {
                    return None;
                }
                at += length;
            }
        }
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

fn changed(path: &str) -> BridgeError {
    BridgeError::new(
        "changed_while_reading",
        format!("{path} changed while it was read, or its pages did not join up; read it again"),
    )
}

/// Read the image at `path` through `call`, page by page. Each page must join the last
/// exactly and the file must keep its size and time, so pages of two files are never
/// joined and a broker that makes no progress is never followed.
pub async fn read_image<C, F>(call: C, path: String) -> Result<Image, BridgeError>
where
    C: Fn(RequestBody) -> F,
    F: Future<Output = Result<ResponseBody, BridgeError>>,
{
    let mut bytes = Vec::new();
    let mut offset = 0u64;
    let mut first: Option<(u64, Option<u64>, String)> = None;
    let mut realigned_at = None;
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
        let (size, modified, shown) = match &first {
            Some(first) => first.clone(),
            None => (page.size, page.modified_ms, page.path.clone()),
        };
        if (page.size, page.modified_ms) != (size, modified) {
            return Err(changed(&shown));
        }
        // A text page that would start inside a character starts at the next one: step
        // back to that character's first byte, already read, and read on from there.
        let shift = page.offset.wrapping_sub(offset);
        if page.text.is_some() && (1..=3).contains(&shift) && realigned_at != Some(offset) {
            let tail = bytes.len().saturating_sub(3);
            let Some(lead) = (tail..bytes.len()).rev().find(|i| bytes[*i] & 0xC0 != 0x80) else {
                return Err(changed(&shown));
            };
            offset -= (bytes.len() - lead) as u64;
            bytes.truncate(lead);
            realigned_at = Some(offset);
            continue;
        }
        if page.offset != offset {
            return Err(changed(&shown));
        }
        let eof = page.eof;
        let next = page.next_offset;
        let read = page_bytes(page.text, page.bytes_base64)?;
        if next != offset + read.len() as u64 || (next == offset && !eof) {
            return Err(changed(&shown));
        }
        bytes.extend(read);
        if first.is_none() {
            let Some(mime_type) = sniff(&bytes) else {
                return Err(BridgeError::new(
                    "not_an_image",
                    format!("{shown} is not a PNG, JPEG, GIF or WebP image; read_file reads its bytes"),
                ));
            };
            if size > MAX_IMAGE_BYTES {
                return Err(BridgeError::new(
                    "too_large",
                    format!(
                        "{shown} is a {mime_type} of {size} bytes; read_image returns images up to {MAX_IMAGE_BYTES} bytes (scale it down with run first)"
                    ),
                ));
            }
            bytes.reserve(size as usize);
            first = Some((size, modified, shown.clone()));
        }
        if bytes.len() as u64 > size {
            return Err(changed(&shown));
        }
        if !eof {
            offset = next;
            continue;
        }
        if bytes.len() as u64 != size {
            return Err(changed(&shown));
        }
        let mime_type = sniff(&bytes).ok_or_else(|| changed(&shown))?;
        let (width, height) = dimensions(&bytes).ok_or_else(|| {
            BridgeError::new(
                "not_an_image",
                format!("{shown} is a {mime_type} whose size could not be read; it may be damaged"),
            )
        })?;
        if width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE {
            return Err(BridgeError::new(
                "too_large",
                format!(
                    "{shown} is {width}x{height}; AI clients refuse images over {MAX_IMAGE_SIDE} pixels on a side (scale it down with run first)"
                ),
            ));
        }
        return Ok(Image {
            path: shown,
            mime_type,
            size,
            width,
            height,
            base64: BASE64.encode(&bytes),
        });
    }
}

#[cfg(test)]
mod tests;
