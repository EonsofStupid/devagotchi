//! Reading a text file by lines, as Desktop Commander's `read_file` does: from a 1-based
//! line, or the last N lines. Work stays bounded whatever the file's size: the start is
//! found by scanning in chunks (forward for a line number, backward for a tail) over at
//! most 256 MiB, or not at all when the caller passes the `next_offset` that came with
//! `next_line`; then at most `max_bytes` are read. Every read stops at the size the file
//! had when the call began, so a log that grows meanwhile is read as it was. Line counts
//! are exact for files up to 8 MiB, and once a read from a known line reaches the end.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use devagotchi_contracts::{BridgeError, BridgeResult, MAX_READ_LINES};

use super::{invalid, text_prefix};

/// Files up to this size get an exact line count.
pub(super) const COUNTED_BYTES: u64 = 8 * 1024 * 1024;
/// Most bytes one call scans to find where its lines start.
const SCANNED_BYTES: u64 = 256 * 1024 * 1024;
const CHUNK: usize = 64 * 1024;
const DEFAULT_LINES: u32 = 1000;

/// What a read by lines asks for.
#[derive(Debug, Clone, Copy)]
pub(super) struct Ask {
    pub(super) start_line: i64,
    pub(super) line_count: Option<u32>,
    pub(super) max_bytes: usize,
    /// With a positive `start_line`: the byte offset where that line starts (the
    /// `next_offset` that came with it), so nothing is scanned. 0: find it.
    pub(super) offset: i64,
}

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

/// Lines in `bytes`: its newlines, plus a last line with none.
pub(super) fn count(bytes: &[u8]) -> u64 {
    let breaks = bytes.iter().filter(|b| **b == b'\n').count() as u64;
    breaks + u64::from(bytes.last().is_some_and(|b| *b != b'\n'))
}

fn changed(error: std::io::Error) -> BridgeError {
    if error.kind() == std::io::ErrorKind::UnexpectedEof {
        BridgeError::new(
            "changed_while_reading",
            "The file got shorter while it was read; read it again",
        )
    } else {
        BridgeError::new("io_error", error.to_string())
    }
}

fn too_far() -> BridgeError {
    BridgeError::new(
        "too_far",
        format!(
            "Finding that line would scan more than {} MiB of this file; read on from a byte offset (offset), pass the next_offset that came with next_line, or count from the end",
            SCANNED_BYTES / (1024 * 1024)
        ),
    )
}

/// Lines in the file's first `size` bytes.
fn count_lines(file: &mut File, size: u64) -> std::io::Result<u64> {
    file.seek(SeekFrom::Start(0))?;
    let mut chunk = vec![0; CHUNK];
    let (mut lines, mut last, mut left) = (0u64, b'\n', size);
    while left > 0 {
        let want = (left as usize).min(CHUNK);
        file.read_exact(&mut chunk[..want])?;
        lines += chunk[..want].iter().filter(|b| **b == b'\n').count() as u64;
        last = chunk[want - 1];
        left -= want as u64;
    }
    Ok(lines + u64::from(size > 0 && last != b'\n'))
}

/// Byte offset where 1-based line `line` starts, scanning forward through at most `scan`
/// bytes; `Ok(Err(lines))` with the line count when the file has fewer lines.
fn line_start(
    file: &mut File,
    size: u64,
    line: u64,
    scan: u64,
) -> BridgeResult<Result<u64, u64>> {
    if line == 1 {
        return Ok(if size > 0 { Ok(0) } else { Err(0) });
    }
    file.seek(SeekFrom::Start(0)).map_err(changed)?;
    let mut chunk = vec![0; CHUNK];
    let (mut seen, mut position, mut last) = (0u64, 0u64, b'\n');
    while position < size {
        if position >= scan {
            return Err(too_far());
        }
        let want = (size - position).min(scan - position).min(CHUNK as u64) as usize;
        file.read_exact(&mut chunk[..want]).map_err(changed)?;
        for (index, byte) in chunk[..want].iter().enumerate() {
            if *byte == b'\n' {
                seen += 1;
                if seen == line - 1 {
                    let start = position + index as u64 + 1;
                    // A newline that ends the file starts no line.
                    return Ok(if start < size { Ok(start) } else { Err(seen) });
                }
            }
        }
        last = chunk[want - 1];
        position += want as u64;
    }
    Ok(Err(seen + u64::from(size > 0 && last != b'\n')))
}

/// Byte offset where the last `lines` lines start, scanning backward through at most
/// `scan` bytes.
fn tail_start(file: &mut File, size: u64, lines: u64, scan: u64) -> BridgeResult<u64> {
    if size == 0 {
        return Ok(0);
    }
    let mut chunk = vec![0; CHUNK];
    let mut end = size;
    let mut found = 0u64;
    let mut first = true;
    while end > 0 {
        let scanned = size - end;
        if scanned >= scan {
            return Err(too_far());
        }
        let from = end.saturating_sub((scan - scanned).min(CHUNK as u64));
        let length = (end - from) as usize;
        file.seek(SeekFrom::Start(from)).map_err(changed)?;
        file.read_exact(&mut chunk[..length]).map_err(changed)?;
        let mut upto = length;
        // A newline that ends the file closes the last line rather than starting another.
        if first && chunk[length - 1] == b'\n' {
            upto -= 1;
        }
        first = false;
        for index in (0..upto).rev() {
            if chunk[index] == b'\n' {
                found += 1;
                if found == lines {
                    return Ok(from + index as u64 + 1);
                }
            }
        }
        end = from;
    }
    Ok(0)
}

pub(super) fn read_lines(file: &mut File, size: u64, ask: Ask) -> BridgeResult<Lines> {
    read_lines_within(file, size, ask, COUNTED_BYTES, SCANNED_BYTES)
}

fn read_lines_within(
    file: &mut File,
    size: u64,
    ask: Ask,
    counted: u64,
    scan: u64,
) -> BridgeResult<Lines> {
    if ask.start_line == 0 {
        return Err(invalid("start_line counts from 1, or back from the end when negative"));
    }
    if ask
        .line_count
        .is_some_and(|n| !(1..=MAX_READ_LINES).contains(&n))
    {
        return Err(invalid(format!("line_count must be 1..={MAX_READ_LINES}")));
    }
    if ask.offset < 0 || (ask.start_line < 0 && ask.offset != 0) {
        return Err(invalid(
            "with start_line, offset is only the next_offset that came with a next_line",
        ));
    }
    let total = if size <= counted {
        Some(count_lines(file, size).map_err(changed)?)
    } else {
        None
    };
    let (offset, first, wanted) = if ask.start_line > 0 {
        let line = ask.start_line as u64;
        let resume = ask.offset as u64;
        let found = if resume > 0 {
            // The caller says where the line starts; check it is a line's start.
            let mut before = [0];
            let at_start = resume <= size
                && file
                    .seek(SeekFrom::Start(resume - 1))
                    .and_then(|_| file.read_exact(&mut before))
                    .is_ok()
                && before[0] == b'\n';
            if !at_start {
                return Err(invalid(
                    "offset is not where a line starts; pass the next_offset that came with next_line",
                ));
            }
            if resume < size {
                Ok(resume)
            } else {
                Err(total.unwrap_or(line - 1))
            }
        } else {
            line_start(file, size, line, scan)?
        };
        match found {
            Ok(offset) => (offset, Some(line), ask.line_count.unwrap_or(DEFAULT_LINES)),
            Err(lines) => {
                return Ok(Lines {
                    text: String::new(),
                    offset: size,
                    next_offset: size,
                    start_line: None,
                    next_line: None,
                    total_lines: total.or(Some(lines)),
                    eof: true,
                });
            }
        }
    } else {
        let back = ask.start_line.unsigned_abs();
        let offset = tail_start(file, size, back, scan)?;
        let first = total
            .filter(|lines| *lines > 0)
            .map(|lines| lines.saturating_sub(back) + 1);
        let to_end = u32::try_from(back).unwrap_or(u32::MAX).min(MAX_READ_LINES);
        (offset, first, ask.line_count.unwrap_or(to_end))
    };

    file.seek(SeekFrom::Start(offset)).map_err(changed)?;
    let page = (ask.max_bytes as u64).min(size - offset) as usize;
    let mut bytes = vec![0; page];
    file.read_exact(&mut bytes).map_err(changed)?;
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
    let not_text = || {
        BridgeError::new(
            "not_text",
            "This file is not UTF-8 text; read it by bytes (offset and max_bytes)",
        )
    };
    if partial {
        match std::str::from_utf8(kept) {
            Ok(_) => {}
            Err(error) if error.error_len().is_none() && error.valid_up_to() == 0 => {
                return Err(invalid(
                    "max_bytes is smaller than the next character; raise it",
                ));
            }
            Err(_) => {}
        }
        kept = &kept[..text_prefix(kept).ok_or_else(not_text)?];
    } else if text_prefix(kept) != Some(kept.len()) {
        return Err(not_text());
    }
    let next_offset = offset + kept.len() as u64;
    let eof = next_offset >= size;
    let total = total.or_else(|| {
        eof.then(|| first.map(|f| f + u64::from(lines) - 1))
            .flatten()
    });
    Ok(Lines {
        text: String::from_utf8(kept.to_vec()).map_err(|_| not_text())?,
        offset,
        next_offset,
        start_line: first,
        next_line: (!eof && !partial)
            .then(|| first.map(|f| f + u64::from(lines)))
            .flatten(),
        total_lines: total,
        eof,
    })
}

#[cfg(test)]
mod tests;
