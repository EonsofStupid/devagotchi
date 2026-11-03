//! The site's files in `assets/`: pictures, the video, fonts. Each is served from this hub
//! at `/assets/<name>?v=<hash>`, the first 16 hex digits of its SHA-256, so a page always
//! names the exact bytes it means and the file is cached for a year. Only types a page can
//! load under the pages' CSP are taken; no HTML, script or stylesheet comes from a site.

use axum::body::Bytes;
use sha2::{Digest, Sha256};

pub const MAX_ASSET_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// The hub's own files under `/assets/`, which no site file may take the name of.
const RESERVED: [&str; 3] = ["hub.css", "passkey.js", "push.js"];

const TYPES: [(&str, &str); 11] = [
    ("svg", "image/svg+xml"),
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("webp", "image/webp"),
    ("gif", "image/gif"),
    ("ico", "image/x-icon"),
    ("mp4", "video/mp4"),
    ("webm", "video/webm"),
    ("woff2", "font/woff2"),
    ("woff", "font/woff"),
];

#[derive(Clone)]
pub struct Asset {
    pub name: String,
    pub bytes: Bytes,
    pub mime: &'static str,
    /// The `?v=` on its URL.
    pub version: String,
    /// Its strong ETag.
    pub etag: String,
}

impl std::fmt::Debug for Asset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Asset({}, {} bytes)", self.name, self.bytes.len())
    }
}

pub fn check_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 100
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b));
    if !ok {
        return Err("a name is lowercase letters, digits, '.', '_' and '-'".into());
    }
    if RESERVED.contains(&name) {
        return Err("that name is the hub's own file".into());
    }
    mime(name).map(|_| ())
}

fn mime(name: &str) -> Result<&'static str, String> {
    let extension = name
        .rsplit_once('.')
        .map(|(_, ext)| ext)
        .unwrap_or_default();
    TYPES
        .iter()
        .find(|(ext, _)| *ext == extension)
        .map(|(_, mime)| *mime)
        .ok_or_else(|| {
            format!(
                "an asset is one of {}",
                TYPES
                    .iter()
                    .map(|(ext, _)| *ext)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

impl Asset {
    pub fn new(name: &str, bytes: Bytes) -> Result<Self, String> {
        check_name(name)?;
        let mime = mime(name)?;
        let digest = hex::encode(Sha256::digest(&bytes));
        let asset = Self {
            name: name.to_string(),
            mime,
            version: digest[..16].to_string(),
            etag: format!("\"{}\"", &digest[..32]),
            bytes,
        };
        asset.check_contents()?;
        Ok(asset)
    }

    pub fn extension(&self) -> &str {
        self.name
            .rsplit_once('.')
            .map(|(_, ext)| ext)
            .unwrap_or_default()
    }

    pub fn url(&self) -> String {
        format!("/assets/{}?v={}", self.name, self.version)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).trim_end().to_string()
    }

    /// The file is what its name says, and an SVG carries nothing that runs or fetches.
    fn check_contents(&self) -> Result<(), String> {
        let bytes = &self.bytes[..];
        let starts = |magic: &[u8]| bytes.starts_with(magic);
        let ok = match self.extension() {
            "svg" => {
                let text = std::str::from_utf8(bytes).map_err(|_| "an SVG is UTF-8 text")?;
                return check_svg(text);
            }
            "png" => starts(b"\x89PNG\r\n\x1a\n"),
            "jpg" | "jpeg" => starts(&[0xFF, 0xD8, 0xFF]),
            "webp" => starts(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
            "gif" => starts(b"GIF8"),
            "ico" => starts(&[0, 0, 1, 0]),
            "mp4" => bytes.get(4..8) == Some(b"ftyp"),
            "webm" => starts(&[0x1A, 0x45, 0xDF, 0xA3]),
            "woff2" => starts(b"wOF2"),
            "woff" => starts(b"wOFF"),
            _ => false,
        };
        if ok {
            Ok(())
        } else {
            Err(format!("is not a {} file", self.extension()))
        }
    }
}

/// Nothing in an SVG that runs, embeds a page, or fetches from anywhere: references stay
/// inside the file (`#id`).
fn check_svg(text: &str) -> Result<(), String> {
    let lower = text.to_ascii_lowercase();
    if !lower.contains("<svg") {
        return Err("an SVG has an <svg> element".into());
    }
    for refused in [
        "<script",
        "<foreignobject",
        "javascript:",
        "<!entity",
        "<iframe",
        "<embed",
        "<object",
    ] {
        if lower.contains(refused) {
            return Err(format!("an SVG here may not hold {refused}"));
        }
    }
    // Event handlers: an attribute named on… (onload=, onclick=).
    let bytes = lower.as_bytes();
    for (i, _) in lower.match_indices("on") {
        let before = i.checked_sub(1).map(|j| bytes[j]);
        if matches!(before, Some(b' ' | b'\n' | b'\t' | b'\r')) {
            let name_end = lower[i..]
                .find(|c: char| !c.is_ascii_alphabetic())
                .map(|n| i + n)
                .unwrap_or(lower.len());
            if name_end > i + 2 && lower[name_end..].trim_start().starts_with('=') {
                return Err("an SVG here may not have event handlers (on… attributes)".into());
            }
        }
    }
    for marker in ["href=\"", "href='", "url("] {
        for (i, _) in lower.match_indices(marker) {
            let value = lower[i + marker.len()..].trim_start_matches(['"', '\'']);
            if !value.starts_with('#') {
                return Err(format!(
                    "an SVG here refers only to its own parts ({marker}#…), never to another file"
                ));
            }
        }
    }
    Ok(())
}

/// More for an SVG drawn inside a page: the page's CSP refuses inline styles, so a
/// `<style>` or `style=` in it would be ignored. Colour it with the hub's classes or
/// presentation attributes.
pub fn check_inline_svg(text: &str) -> Result<(), String> {
    let lower = text.to_ascii_lowercase();
    if !lower.trim_start().starts_with("<svg") {
        return Err("starts with <svg> (no XML prolog or comment before it)".into());
    }
    if lower.contains("<style") || lower.contains(" style=") {
        return Err("the pages' CSP refuses inline styles: use presentation attributes or the stylesheet's classes".into());
    }
    Ok(())
}
