//! The setup video and its poster (`media.rs`): served whole with `Accept-Ranges`, one byte
//! range as `206`, `416` past the end, `304` for the ETag the browser holds, and cached for a
//! year under URLs that carry their content hash.

use axum::{
    body::to_bytes,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::Response,
};

use super::media::{POSTER, Ranged, VIDEO, parse, poster, poster_url, version, video, video_url};

fn with(pairs: &[(header::HeaderName, &str)]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in pairs {
        headers.insert(name.clone(), HeaderValue::from_str(value).unwrap());
    }
    headers
}

fn get<'a>(response: &'a Response, name: header::HeaderName) -> Option<&'a str> {
    response.headers().get(name).map(|v| v.to_str().unwrap())
}

async fn body(response: Response) -> Vec<u8> {
    to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec()
}

#[test]
fn the_files_are_an_mp4_and_a_png_small_enough_to_build_in() {
    assert_eq!(&VIDEO[4..8], b"ftyp", "an MP4");
    assert!(VIDEO.len() <= 4_500_000, "{} bytes", VIDEO.len());
    assert_eq!(&POSTER[..8], b"\x89PNG\r\n\x1a\n");
    assert!(POSTER.len() <= 600_000, "{} bytes", POSTER.len());
    assert_eq!(version().len(), 16);
    assert_eq!(video_url(), format!("/assets/turnkey.mp4?v={}", version()));
    assert_eq!(
        poster_url(),
        format!("/assets/turnkey-poster.png?v={}", version())
    );
}

#[tokio::test]
async fn the_whole_video_says_it_takes_ranges_and_is_cached_by_its_hash() {
    let response = video(HeaderMap::new()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(get(&response, header::CONTENT_TYPE), Some("video/mp4"));
    assert_eq!(get(&response, header::ACCEPT_RANGES), Some("bytes"));
    assert_eq!(
        get(&response, header::CACHE_CONTROL),
        Some("public, max-age=31536000, immutable")
    );
    assert_eq!(
        get(&response, header::X_CONTENT_TYPE_OPTIONS),
        Some("nosniff")
    );
    let etag = get(&response, header::ETAG).unwrap().to_string();
    assert!(
        etag.starts_with('"') && etag.ends_with('"') && etag.len() == 34,
        "{etag}"
    );
    assert_eq!(body(response).await, VIDEO);

    // The browser's copy is current: nothing to send.
    let again = video(with(&[(header::IF_NONE_MATCH, &etag)])).await;
    assert_eq!(again.status(), StatusCode::NOT_MODIFIED);
    assert!(body(again).await.is_empty());
}

#[tokio::test]
async fn safari_gets_the_byte_ranges_it_asks_for() {
    let total = VIDEO.len();
    // Safari's first probe: two bytes.
    let probe = video(with(&[(header::RANGE, "bytes=0-1")])).await;
    assert_eq!(probe.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        get(&probe, header::CONTENT_RANGE),
        Some(format!("bytes 0-1/{total}").as_str())
    );
    assert_eq!(get(&probe, header::CONTENT_TYPE), Some("video/mp4"));
    assert_eq!(body(probe).await, &VIDEO[..2]);

    // From an offset to the end, and the last bytes.
    let rest = video(with(&[(header::RANGE, "bytes=1000-")])).await;
    assert_eq!(rest.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        get(&rest, header::CONTENT_RANGE),
        Some(format!("bytes 1000-{}/{total}", total - 1).as_str())
    );
    assert_eq!(body(rest).await, &VIDEO[1000..]);
    let tail = video(with(&[(header::RANGE, "bytes=-500")])).await;
    assert_eq!(body(tail).await, &VIDEO[total - 500..]);

    // An end past the file is cut to the file.
    let over = video(with(&[(
        header::RANGE,
        &format!("bytes=10-{}", total + 99),
    )]))
    .await;
    assert_eq!(
        get(&over, header::CONTENT_RANGE),
        Some(format!("bytes 10-{}/{total}", total - 1).as_str())
    );

    // A start past the end cannot be met.
    let past = video(with(&[(header::RANGE, &format!("bytes={total}-"))])).await;
    assert_eq!(past.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        get(&past, header::CONTENT_RANGE),
        Some(format!("bytes */{total}").as_str())
    );
    assert!(body(past).await.is_empty());
}

#[tokio::test]
async fn a_range_it_cannot_serve_or_one_for_an_old_version_gets_the_whole_file() {
    let total = VIDEO.len();
    for range in ["bytes=0-1,4-5", "items=0-1", "bytes=9-3", "bytes=x-"] {
        let response = video(with(&[(header::RANGE, range)])).await;
        assert_eq!(response.status(), StatusCode::OK, "{range}");
        assert_eq!(body(response).await.len(), total, "{range}");
    }
    let stale = video(with(&[
        (header::RANGE, "bytes=0-1"),
        (header::IF_RANGE, "\"an-older-video\""),
    ]))
    .await;
    assert_eq!(stale.status(), StatusCode::OK);
    let etag = get(&video(HeaderMap::new()).await, header::ETAG)
        .unwrap()
        .to_string();
    let current = video(with(&[
        (header::RANGE, "bytes=0-1"),
        (header::IF_RANGE, &etag),
    ]))
    .await;
    assert_eq!(current.status(), StatusCode::PARTIAL_CONTENT);
}

#[test]
fn ranges_parse_as_rfc_9110_reads_them() {
    let at = |v: &str| parse(&HeaderValue::from_str(v).unwrap(), 100);
    assert_eq!(at("bytes=0-0"), Ranged::Within(0, 0));
    assert_eq!(at("bytes=0-99"), Ranged::Within(0, 99));
    assert_eq!(at("bytes=50-"), Ranged::Within(50, 99));
    assert_eq!(at("bytes=-1"), Ranged::Within(99, 99));
    assert_eq!(at("bytes=-500"), Ranged::Within(0, 99));
    assert_eq!(at("bytes=90-1000"), Ranged::Within(90, 99));
    assert_eq!(at("bytes=100-"), Ranged::Unsatisfiable);
    assert_eq!(at("bytes=-0"), Ranged::Unsatisfiable);
    assert_eq!(at("bytes=5-4"), Ranged::Whole);
    assert_eq!(at("bytes=0-1,5-6"), Ranged::Whole);
    assert_eq!(at("pages=1-2"), Ranged::Whole);
}

#[tokio::test]
async fn the_poster_is_a_png_under_the_same_rules() {
    let response = poster(HeaderMap::new()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(get(&response, header::CONTENT_TYPE), Some("image/png"));
    assert!(get(&response, header::ETAG).is_some());
    assert_eq!(body(response).await, POSTER);
}
