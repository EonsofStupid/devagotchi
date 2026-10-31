//! Reading a text file by lines, as Desktop Commander's `read_file` does: from a 1-based
//! line, or the last N lines. Memory stays bounded whatever the file's size: the start is
//! found by scanning in chunks (forward for a line number, backward for a tail), then at
//! most `max_bytes` are read from there. Line counts are exact for files up to 8 MiB, and
//! for any file once the read reaches its end.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use devagotchi_contracts::{BridgeError, BridgeResult, MAX_READ_LINES};

use super::{invalid, text_prefix};

/// Files up to this size get an exact line count.
const COUNTED_BYTES: u64 = 8 * 1024 * 1024;
const CHUNK: usize = 64 * 1024;
const DEFAULT_LINES: u32 = 1000;

/// What a read by lines found.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Lines {
    pub(super) text: String,
    /// Byte offset of the text.
    pub(super) offset: u64,
    /// Byte offset after it.
    pub(super) next_offset: u64,
    pub(super) start_line: Option<u64>,
    pub(super) next_line: Option<u64>,
    pub(super) total_lines: Option<u64>,
    pub(super) eof: bool,
}

/// Lines in the whole file: newlines, plus a last line with none.
fn count_lines(file: &mut File, size: u64) -> std::io::Result<u64> {
    file.seek(SeekFrom::Start(0))?;
    let mut chunk = vec![0; CHUNK];
    let (mut lines, mut last) = (0u64, b'\n');
    loop {
        let read = file.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        lines += chunk[..read].iter().filter(|b| **b == b'\n').count() as u64;
        last = chunk[read - 1];
    }
    Ok(lines + u64::from(size > 0 && last != b'\n'))
}

/// Byte offset where 1-based line `line` starts, scanning forward; `Err(lines)` with the
/// file's line count when it has fewer lines.
fn line_start(file: &mut File, size: u64, line: u64) -> std::io::Result<Result<u64, u64>> {
    if line == 1 {
        return Ok(if size > 0 { Ok(0) } else { Err(0) });
    }
    file.seek(SeekFrom::Start(0))?;
    let mut chunk = vec![0; CHUNK];
    let (mut seen, mut position) = (0u64, 0u64);
    loop {
        let read = file.read(&mut chunk)?;
        if read == 0 {
            // `seen` newlines, plus a last line without one.
            let ended_open = size > 0 && seen_last_byte(file, size)? != b'\n';
            return Ok(Err(seen + u64::from(ended_open)));
        }
        for (index, byte) in chunk[..read].iter().enumerate() {
            if *byte == b'\n' {
                seen += 1;
                if seen == line - 1 {
                    let start = position + index as u64 + 1;
                    // A newline that ends the file starts no line.
                    return Ok(if start < size { Ok(start) } else { Err(seen) });
                }
            }
        }
        position += read as u64;
    }
}

fn seen_last_byte(file: &mut File, size: u64) -> std::io::Result<u8> {
    let mut byte = [0];
    file.seek(SeekFrom::Start(size - 1))?;
    file.read_exact(&mut byte)?;
    Ok(byte[0])
}

/// Byte offset where the last `count` lines start, scanning backward.
fn tail_start(file: &mut File, size: u64, count: u64) -> std::io::Result<u64> {
    if size == 0 {
        return Ok(0);
    }
    // A newline that ends the file closes the last line rather than starting another.
    let mut end = size;
    if seen_last_byte(file, size)? == b'\n' {
        end -= 1;
    }
    let mut chunk = vec![0; CHUNK];
    let mut found = 0u64;
    while end > 0 {
        let from = end.saturating_sub(CHUNK as u64);
        let length = (end - from) as usize;
        file.seek(SeekFrom::Start(from))?;
        file.read_exact(&mut chunk[..length])?;
        for index in (0..length).rev() {
            if chunk[index] == b'\n' {
                found += 1;
                if found == count {
                    return Ok(from + index as u64 + 1);
                }
            }
        }
        end = from;
    }
    Ok(0)
}

pub(super) fn read_lines(
    file: &mut File,
    size: u64,
    start_line: i64,
    line_count: Option<u32>,
    max_bytes: usize,
) -> BridgeResult<Lines> {
    let io = |error: std::io::Error| BridgeError::new("io_error", error.to_string());
    if start_line == 0 {
        return Err(invalid("start_line counts from 1, or back from the end when negative"));
    }
    if line_count.is_some_and(|n| !(1..=MAX_READ_LINES).contains(&n)) {
        return Err(invalid(format!("line_count must be 1..={MAX_READ_LINES}")));
    }
    let total = if size <= COUNTED_BYTES {
        Some(count_lines(file, size).map_err(io)?)
    } else {
        None
    };
    let (offset, first, wanted) = if start_line > 0 {
        let line = start_line as u64;
        match line_start(file, size, line).map_err(io)? {
            Ok(offset) => (offset, Some(line), line_count.unwrap_or(DEFAULT_LINES)),
            Err(lines) => {
                return Ok(Lines {
                    text: String::new(),
                    offset: size,
                    next_offset: size,
                    start_line: None,
                    next_line: None,
                    total_lines: Some(lines),
                    eof: true,
                });
            }
        }
    } else {
        let back = start_line.unsigned_abs();
        let offset = tail_start(file, size, back).map_err(io)?;
        let first = total.map(|lines| lines.saturating_sub(back) + 1);
        let to_end = u32::try_from(back).unwrap_or(u32::MAX).min(MAX_READ_LINES);
        (offset, first, line_count.unwrap_or(to_end))
    };

    file.seek(SeekFrom::Start(offset)).map_err(io)?;
    let mut bytes = Vec::with_capacity(max_bytes);
    file.by_ref()
        .take(max_bytes as u64)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    let reaches_end = offset + bytes.len() as u64 >= size;
    // Whole lines only, up to `wanted` of them; the last one may end at the end of file.
    let mut taken = 0usize;
    let mut lines = 0u32;
    for (index, byte) in bytes.iter().enumerate() {
        if lines == wanted {
            break;
        }
        if *byte == b'\n' {
            lines += 1;
            taken = index + 1;
        }
    }
    if lines < wanted && reaches_end && taken < bytes.len() {
        lines += 1;
        taken = bytes.len();
    }
    // One line longer than the page: as much of it as fits, read on by bytes.
    let partial = lines == 0 && !bytes.is_empty();
    if partial {
        taken = bytes.len();
    }
    let mut kept = &bytes[..taken];
    let readable = text_prefix(kept).ok_or_else(|| {
        BridgeError::new(
            "not_text",
            "This file is not UTF-8 text; read it by bytes (offset and max_bytes)",
        )
    })?;
    if partial {
        kept = &kept[..readable];
    } else if readable != kept.len() {
        return Err(BridgeError::new(
            "not_text",
            "This file is not UTF-8 text; read it by bytes (offset and max_bytes)",
        ));
    }
    let next_offset = offset + kept.len() as u64;
    let eof = next_offset >= size;
    let total = total.or_else(|| eof.then(|| first.map(|f| f + u64::from(lines) - 1)).flatten());
    Ok(Lines {
        text: String::from_utf8(kept.to_vec()).map_err(|_| invalid("not UTF-8"))?,
        offset,
        next_offset,
        start_line: first,
        next_line: (!eof && !partial).then(|| first.map(|f| f + u64::from(lines))).flatten(),
        total_lines: total,
        eof,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn file_with(text: &[u8]) -> (tempfile::NamedTempFile, File, u64) {
        let mut named = tempfile::NamedTempFile::new().unwrap();
        named.write_all(text).unwrap();
        let file = File::open(named.path()).unwrap();
        (named, file, text.len() as u64)
    }

    fn read(text: &[u8], start: i64, count: Option<u32>, max: usize) -> BridgeResult<Lines> {
        let (_keep, mut file, size) = file_with(text);
        read_lines(&mut file, size, start, count, max)
    }

    #[test]
    fn lines_from_a_number_with_where_to_go_on() {
        let lines = read(b"one\ntwo\nthree\nfour\n", 2, Some(2), 1024).unwrap();
        assert_eq!(lines.text, "two\nthree\n");
        assert_eq!((lines.start_line, lines.next_line), (Some(2), Some(4)));
        assert_eq!((lines.total_lines, lines.eof), (Some(4), false));
        assert_eq!((lines.offset, lines.next_offset), (4, 14));
        let rest = read(b"one\ntwo\nthree\nfour\n", 4, None, 1024).unwrap();
        assert_eq!((rest.text.as_str(), rest.next_line, rest.eof), ("four\n", None, true));
    }

    #[test]
    fn the_last_lines_counted_from_the_end() {
        let tail = read(b"a\nb\nc\nd", -2, None, 1024).unwrap();
        assert_eq!(tail.text, "c\nd", "a last line without a newline counts");
        assert_eq!((tail.start_line, tail.total_lines, tail.eof), (Some(3), Some(4), true));
        let ended = read(b"a\nb\nc\n", -1, None, 1024).unwrap();
        assert_eq!(ended.text, "c\n", "a newline that ends the file starts no line");
        let all = read(b"a\nb\n", -10, None, 1024).unwrap();
        assert_eq!((all.text.as_str(), all.start_line), ("a\nb\n", Some(1)));
    }

    #[test]
    fn past_the_end_and_empty_files_answer_empty_with_the_count() {
        let past = read(b"a\nb\n", 5, None, 1024).unwrap();
        assert_eq!((past.text.as_str(), past.total_lines, past.eof), ("", Some(2), true));
        let empty = read(b"", 1, None, 1024).unwrap();
        assert_eq!((empty.text.as_str(), empty.total_lines), ("", Some(0)));
        let empty_tail = read(b"", -5, None, 1024).unwrap();
        assert_eq!(empty_tail.text, "");
    }

    #[test]
    fn a_page_holds_whole_lines_and_a_long_line_goes_on_by_bytes() {
        let page = read(b"aaaa\nbbbb\ncccc\n", 1, None, 12).unwrap();
        assert_eq!(page.text, "aaaa\nbbbb\n", "the line cut by the page is left for the next");
        assert_eq!(page.next_line, Some(3));
        let long = read("é".repeat(10).as_bytes(), 1, None, 7).unwrap();
        assert_eq!(long.text, "ééé", "a cut character waits for the next read");
        assert_eq!((long.next_line, long.next_offset), (None, 6));
    }

    #[test]
    fn crlf_lines_and_binary_files() {
        let windows = read(b"one\r\ntwo\r\n", 2, None, 1024).unwrap();
        assert_eq!(windows.text, "two\r\n");
        assert_eq!(read(b"\x00\x01\x02\n", 1, None, 1024).unwrap_err().code, "not_text");
        assert_eq!(read(b"a\n", 0, None, 1024).unwrap_err().code, "invalid_request");
        assert!(read(b"a\n", 1, Some(0), 1024).is_err());
        assert!(read(b"a\n", 1, Some(MAX_READ_LINES + 1), 1024).is_err());
    }

    #[test]
    fn a_large_file_is_scanned_in_chunks_both_ways() {
        // Longer than one chunk, line numbers checked across chunk edges.
        let text: String = (1..=20_000).map(|n| format!("line {n}\n")).collect();
        assert!(text.len() > 2 * CHUNK);
        let middle = read(text.as_bytes(), 15_001, Some(2), 1024).unwrap();
        assert_eq!(middle.text, "line 15001\nline 15002\n");
        let tail = read(text.as_bytes(), -3, None, 1024).unwrap();
        assert_eq!(tail.text, "line 19998\nline 19999\nline 20000\n");
        assert_eq!(tail.start_line, Some(19_998));
    }
}
