//! The site's files under `/assets/` (`site/assets.rs`): its logo, icon, pictures, setup
//! video and fonts. Pages name them with their content hash (`?v=`), so they are cached for
//! a year; the hash is also their ETag. The video answers byte ranges, which Safari needs
//! before it plays anything: one range a request, `206` with `Content-Range`, `416` past
//! the end, the whole file for a range it cannot read or an `If-Range` that no longer
//! matches.

use axum::{
    body::{Body, Bytes},
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};

use crate::AppState;

/// `/assets/<file>`: one of the site's files, or 404.
pub async fn asset(
    State(state): State<AppState>,
    Path(file): Path<String>,
    headers: HeaderMap,
) -> Response {
    let site = state.hub.config.site();
    match site.assets.get(&file) {
        Some(asset) => serve(asset.bytes.clone(), asset.mime, &asset.etag, &headers),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `bytes` as `kind`, whole, as the one range asked for, or not modified.
pub fn serve(bytes: Bytes, kind: &'static str, etag: &str, headers: &HeaderMap) -> Response {
    let total = bytes.len() as u64;
    let matches = |value: &HeaderValue| {
        value.to_str().is_ok_and(|v| {
            v.split(',')
                .any(|tag| tag.trim() == etag || tag.trim() == "*")
        })
    };
    let (status, body, range) = if headers.get(header::IF_NONE_MATCH).is_some_and(matches) {
        (StatusCode::NOT_MODIFIED, Body::empty(), None)
    } else {
        // A range is honoured only for the version it was asked against.
        let fresh = headers
            .get(header::IF_RANGE)
            .is_none_or(|value| value.to_str().is_ok_and(|v| v.trim() == etag));
        match headers
            .get(header::RANGE)
            .filter(|_| fresh)
            .map(|v| parse(v, total))
        {
            Some(Ranged::Within(start, end)) => (
                StatusCode::PARTIAL_CONTENT,
                Body::from(bytes.slice(start as usize..=end as usize)),
                Some(format!("bytes {start}-{end}/{total}")),
            ),
            Some(Ranged::Unsatisfiable) => (
                StatusCode::RANGE_NOT_SATISFIABLE,
                Body::empty(),
                Some(format!("bytes */{total}")),
            ),
            Some(Ranged::Whole) | None => (StatusCode::OK, Body::from(bytes), None),
        }
    };
    let mut response = (status, body).into_response();
    let out = response.headers_mut();
    if status != StatusCode::RANGE_NOT_SATISFIABLE {
        out.insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
    }
    out.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    out.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    out.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if let Ok(value) = HeaderValue::from_str(etag) {
        out.insert(header::ETAG, value);
    }
    if let Some(range) = range.and_then(|r| HeaderValue::from_str(&r).ok()) {
        out.insert(header::CONTENT_RANGE, range);
    }
    response
}

/// What a `Range` header asks of a file `total` bytes long.
#[derive(Debug, PartialEq)]
pub enum Ranged {
    /// Bytes `start..=end`, both inside the file.
    Within(u64, u64),
    /// A well-formed range that starts past the end (or asks for no bytes).
    Unsatisfiable,
    /// Not one byte range this serves (another unit, several ranges, malformed): the whole file.
    Whole,
}

pub fn parse(value: &HeaderValue, total: u64) -> Ranged {
    let Some(spec) = value
        .to_str()
        .ok()
        .and_then(|v| v.trim().strip_prefix("bytes="))
    else {
        return Ranged::Whole;
    };
    if spec.contains(',') {
        return Ranged::Whole;
    }
    let Some((first, last)) = spec.trim().split_once('-') else {
        return Ranged::Whole;
    };
    let number = |text: &str| text.trim().parse::<u64>().ok();
    match (first.trim().is_empty(), number(first), number(last)) {
        // bytes=-N: the last N bytes.
        (true, _, Some(0)) => Ranged::Unsatisfiable,
        (true, _, Some(suffix)) if total > 0 => {
            Ranged::Within(total.saturating_sub(suffix), total - 1)
        }
        (true, _, _) => Ranged::Unsatisfiable,
        (false, Some(start), _) if start >= total => Ranged::Unsatisfiable,
        // bytes=N-: from N to the end.
        (false, Some(start), None) if last.trim().is_empty() => Ranged::Within(start, total - 1),
        (false, Some(start), Some(end)) if end >= start => {
            Ranged::Within(start, end.min(total - 1))
        }
        _ => Ranged::Whole,
    }
}
