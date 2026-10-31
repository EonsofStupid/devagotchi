use std::sync::Mutex;

use devagotchi_contracts::FileContent;

use super::*;

fn page(path: &str, size: u64, offset: u64, text: Option<String>, base64: Option<String>, next: u64) -> FileContent {
    FileContent {
        path: path.into(),
        size,
        offset,
        text,
        bytes_base64: base64,
        next_offset: next,
        eof: next >= size,
        modified_ms: Some(1),
        start_line: None,
        next_line: None,
        total_lines: None,
    }
}

/// A broker holding `bytes` at `/img` that answers reads as the real one does: a page that
/// is readable text comes back as text, and one that starts inside a UTF-8 character
/// starts at the next character instead (`crates/core/src/files.rs`); anything else as
/// base64. Pages are `page_bytes` long.
fn broker(
    bytes: Vec<u8>,
    page_bytes: usize,
) -> impl Fn(RequestBody) -> std::future::Ready<Result<ResponseBody, BridgeError>> {
    move |body| {
        let RequestBody::FileRead(input) = body else {
            panic!("only reads")
        };
        let size = bytes.len() as u64;
        let start = (input.offset as usize).min(bytes.len());
        let end = (start + input.max_bytes.min(page_bytes)).min(bytes.len());
        let chunk = &bytes[start..end];
        let lead = if start > 0 {
            chunk.iter().take(3).take_while(|b| **b & 0xC0 == 0x80).count()
        } else {
            0
        };
        let text_from = |skip: usize| {
            let rest = &chunk[skip..];
            match std::str::from_utf8(rest) {
                Ok(text) if !text.chars().any(|c| c.is_control() && c != '\n') => Some(text.len()),
                Err(e) if e.error_len().is_none() && e.valid_up_to() > 0 => {
                    let text = std::str::from_utf8(&rest[..e.valid_up_to()]).unwrap();
                    (!text.chars().any(|c| c.is_control() && c != '\n')).then_some(e.valid_up_to())
                }
                _ => None,
            }
        };
        let answer = match (lead, text_from(lead), text_from(0)) {
            (lead, Some(len), _) if lead > 0 => page(
                "/img",
                size,
                (start + lead) as u64,
                Some(String::from_utf8(chunk[lead..lead + len].to_vec()).unwrap()),
                None,
                (start + lead + len) as u64,
            ),
            (_, _, Some(len)) => page(
                "/img",
                size,
                start as u64,
                Some(String::from_utf8(chunk[..len].to_vec()).unwrap()),
                None,
                (start + len) as u64,
            ),
            _ => page("/img", size, start as u64, None, Some(BASE64.encode(chunk)), end as u64),
        };
        std::future::ready(Ok(ResponseBody::File(answer)))
    }
}

/// A PNG of the given size whose IHDR says `width` x `height`, padded with `filler`.
fn png(width: u32, height: u32, filler: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend(13u32.to_be_bytes());
    bytes.extend(b"IHDR");
    bytes.extend(width.to_be_bytes());
    bytes.extend(height.to_be_bytes());
    bytes.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
    bytes.extend(filler);
    bytes
}

#[tokio::test]
async fn an_image_over_many_pages_comes_back_whole() {
    let filler: Vec<u8> = (0..3 * MAX_FILE_READ_BYTES + 17).map(|i| (i % 251) as u8).collect();
    let bytes = png(10, 10, &filler);
    let image = read_image(broker(bytes.clone(), MAX_FILE_READ_BYTES), "/img".into())
        .await
        .unwrap();
    assert_eq!((image.mime_type, image.width, image.height), ("image/png", 10, 10));
    assert_eq!(BASE64.decode(image.base64).unwrap(), bytes);
}

/// Long runs of readable text (an XMP block with é) come back from the broker as text
/// pages, and a page that starts inside a character starts one byte late.
#[tokio::test]
async fn text_inside_an_image_across_a_page_edge_is_joined_exactly() {
    for shift in 0..4 {
        let mut text = "x".repeat(40 - shift);
        text.push_str(&"é".repeat(60));
        let bytes = png(4, 4, text.as_bytes());
        let image = read_image(broker(bytes.clone(), 64), "/img".into()).await.unwrap();
        assert_eq!(BASE64.decode(image.base64).unwrap(), bytes, "shift {shift}");
    }
}

#[tokio::test]
async fn too_tall_or_too_wide_is_refused_before_it_reaches_the_client() {
    let tall = read_image(broker(png(1280, 12_000, b"pixels"), 64), "/img".into()).await;
    assert_eq!(tall.unwrap_err().code, "too_large");
    let wide = read_image(broker(png(8001, 10, b"pixels"), 64), "/img".into()).await;
    assert_eq!(wide.unwrap_err().code, "too_large");
    assert!(read_image(broker(png(8000, 8000, b"pixels"), 64), "/img".into()).await.is_ok());
}

#[tokio::test]
async fn what_is_not_an_image_or_too_large_is_refused_at_the_first_page() {
    let text = read_image(broker(b"just words".to_vec(), 64), "/img".into()).await;
    assert_eq!(text.unwrap_err().code, "not_an_image");
    let words = vec![b'w'; MAX_IMAGE_BYTES as usize + 1];
    let big_text = read_image(broker(words, 64), "/img".into()).await;
    assert_eq!(big_text.unwrap_err().code, "not_an_image", "the type is checked first");
    let huge = read_image(broker(png(10, 10, &vec![0; MAX_IMAGE_BYTES as usize]), MAX_FILE_READ_BYTES), "/img".into()).await;
    assert_eq!(huge.unwrap_err().code, "too_large");
}

/// A machine answers what its link says: a reply that makes no progress, claims more
/// bytes than it carries, or changes the file's size mid-read never loops or joins.
#[tokio::test]
async fn a_broker_that_misreports_is_refused_not_followed() {
    let head = png(4, 4, &[0; 100]);
    let first = BASE64.encode(&head[..50]);
    let replies: Vec<FileContent> = vec![
        page("/img", 116, 0, None, Some(first.clone()), 50),
        page("/img", 116, 50, None, Some(BASE64.encode(&head[50..60])), 50),
    ];
    let stuck = scripted(replies);
    assert_eq!(read_image(stuck, "/img".into()).await.unwrap_err().code, "changed_while_reading");

    let replies = vec![
        page("/img", 116, 0, None, Some(first.clone()), 50),
        page("/img", 116, 50, None, Some(BASE64.encode(&head[50..60])), 90),
    ];
    let overclaims = scripted(replies);
    assert_eq!(read_image(overclaims, "/img".into()).await.unwrap_err().code, "changed_while_reading");

    let replies = vec![
        page("/img", 116, 0, None, Some(first), 50),
        page("/img", 200, 50, None, Some(BASE64.encode(&head[50..100])), 100),
    ];
    let swapped = scripted(replies);
    assert_eq!(read_image(swapped, "/img".into()).await.unwrap_err().code, "changed_while_reading");
}

fn scripted(
    replies: Vec<FileContent>,
) -> impl Fn(RequestBody) -> std::future::Ready<Result<ResponseBody, BridgeError>> {
    let replies = Mutex::new(replies.into_iter());
    move |_| {
        let next = replies.lock().unwrap().next().expect("asked past the script");
        std::future::ready(Ok(ResponseBody::File(next)))
    }
}

#[test]
fn types_and_sizes_come_from_the_first_bytes() {
    assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
    assert_eq!(sniff(b"GIF89a...."), Some("image/gif"));
    assert_eq!(sniff(b"RIFF\x10\x00\x00\x00WEBPVP8 "), Some("image/webp"));
    assert_eq!(sniff(b"RIFF\x10\x00\x00\x00WAVEfmt "), None);
    assert_eq!(sniff(b"<svg"), None);

    assert_eq!(dimensions(&png(640, 480, b"")), Some((640, 480)));
    assert_eq!(dimensions(b"GIF89a\x40\x01\xf0\x00rest"), Some((320, 240)));
    // JPEG: an APP0 segment, then SOF0 with height 300, width 400.
    let jpeg = [
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x01, 0x2C,
        0x01, 0x90, 0x03, 0x01, 0x22, 0x00,
    ];
    assert_eq!(dimensions(&jpeg), Some((400, 300)));
    // WebP VP8X: canvas width-1 and height-1 as 24-bit little endian.
    let mut webp = b"RIFF\x00\x00\x00\x00WEBPVP8X\x0a\x00\x00\x00\x00\x00\x00\x00".to_vec();
    webp.extend([0x1F, 0x03, 0x00, 0x57, 0x02, 0x00]);
    assert_eq!(dimensions(&webp), Some((800, 600)));
    // WebP VP8L: 14-bit width-1 and height-1 after the signature byte.
    let mut lossless = b"RIFF\x00\x00\x00\x00WEBPVP8L\x00\x00\x00\x00\x2f".to_vec();
    let packed: u32 = 99 | (49 << 14);
    lossless.extend(packed.to_le_bytes());
    assert_eq!(dimensions(&lossless), Some((100, 50)));
    // WebP VP8 (lossy): 14-bit width and height after the start code.
    let mut lossy = b"RIFF\x00\x00\x00\x00WEBPVP8 \x00\x00\x00\x00\x00\x00\x00\x9d\x01\x2a".to_vec();
    lossy.extend(640u16.to_le_bytes());
    lossy.extend(480u16.to_le_bytes());
    assert_eq!(dimensions(&lossy), Some((640, 480)));
}
