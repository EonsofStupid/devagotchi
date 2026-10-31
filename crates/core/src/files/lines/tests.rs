use std::io::Write;

use super::*;

fn file_with(text: &[u8]) -> (tempfile::NamedTempFile, File, u64) {
    let mut named = tempfile::NamedTempFile::new().unwrap();
    named.write_all(text).unwrap();
    let file = File::open(named.path()).unwrap();
    (named, file, text.len() as u64)
}

fn ask(start_line: i64, line_count: Option<u32>, max_bytes: usize) -> Ask {
    Ask {
        start_line,
        line_count,
        max_bytes,
        offset: 0,
    }
}

fn read(text: &[u8], start: i64, count: Option<u32>, max: usize) -> BridgeResult<Lines> {
    let (_keep, mut file, size) = file_with(text);
    read_lines(&mut file, size, ask(start, count, max))
}

/// As `read`, with the counted and scanned limits set small to reach the large-file paths.
fn read_small_limits(text: &[u8], ask: Ask, counted: u64, scan: u64) -> BridgeResult<Lines> {
    let (_keep, mut file, size) = file_with(text);
    read_lines_within(&mut file, size, ask, counted, scan)
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
fn reading_on_from_next_offset_scans_nothing_and_matches() {
    let text: String = (1..=50).map(|n| format!("line {n}\n")).collect();
    let (_keep, mut file, size) = file_with(text.as_bytes());
    let first = read_lines(&mut file, size, ask(1, Some(10), 1024)).unwrap();
    let mut resumed = ask(first.next_line.unwrap() as i64, Some(10), 1024);
    resumed.offset = first.next_offset as i64;
    let second = read_lines(&mut file, size, resumed).unwrap();
    let scanned = read_lines(&mut file, size, ask(11, Some(10), 1024)).unwrap();
    assert_eq!(second, scanned);
    assert!(second.text.starts_with("line 11\n"));

    let mut wrong = ask(11, None, 1024);
    wrong.offset = first.next_offset as i64 + 2;
    assert_eq!(read_lines(&mut file, size, wrong).unwrap_err().code, "invalid_request");
    let mut tail_with_offset = ask(-3, None, 1024);
    tail_with_offset.offset = 7;
    assert!(read_lines(&mut file, size, tail_with_offset).is_err());
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
    assert_eq!((empty.text.as_str(), empty.total_lines, empty.start_line), ("", Some(0), None));
    let empty_tail = read(b"", -5, None, 1024).unwrap();
    assert_eq!(
        (empty_tail.text.as_str(), empty_tail.start_line, empty_tail.total_lines),
        ("", None, Some(0)),
        "an empty file has no first line"
    );
}

#[test]
fn a_page_holds_whole_lines_and_a_long_line_goes_on_by_bytes() {
    let page = read(b"aaaa\nbbbb\ncccc\n", 1, None, 12).unwrap();
    assert_eq!(page.text, "aaaa\nbbbb\n", "the line cut by the page is left for the next");
    assert_eq!(page.next_line, Some(3));
    let long = read("é".repeat(10).as_bytes(), 1, None, 7).unwrap();
    assert_eq!(long.text, "ééé", "a cut character waits for the next read");
    assert_eq!((long.next_line, long.next_offset), (None, 6));
    let tiny = read("é\n".as_bytes(), 1, None, 1).unwrap_err();
    assert_eq!(tiny.code, "invalid_request", "a page smaller than one character");
}

#[test]
fn crlf_lines_coloured_logs_and_binary_files() {
    let windows = read(b"one\r\ntwo\r\n", 2, None, 1024).unwrap();
    assert_eq!(windows.text, "two\r\n");
    let coloured = read(b"ok\n\x1b[31merror\x1b[0m\n", -1, None, 1024).unwrap();
    assert_eq!(coloured.text, "\x1b[31merror\x1b[0m\n", "ANSI colours are text");
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
    // A newline exactly on a chunk boundary.
    let mut edge = vec![b'x'; CHUNK - 1];
    edge.extend(b"\nnext\n");
    let after = read(&edge, 2, None, 1024).unwrap();
    assert_eq!(after.text, "next\n");
    assert_eq!(read(&edge, -1, None, 1024).unwrap().text, "next\n");
}

#[test]
fn past_the_counted_size_numbers_are_known_only_from_a_known_line() {
    let text = b"a\nb\nc\nd\ne\n";
    let tail = read_small_limits(text, ask(-2, None, 1024), 4, SCANNED_BYTES).unwrap();
    assert_eq!(tail.text, "d\ne\n");
    assert_eq!((tail.start_line, tail.total_lines, tail.next_line), (None, None, None));
    let to_end = read_small_limits(text, ask(4, None, 1024), 4, SCANNED_BYTES).unwrap();
    assert_eq!((to_end.text.as_str(), to_end.total_lines), ("d\ne\n", Some(5)));
    let middle = read_small_limits(text, ask(2, Some(1), 1024), 4, SCANNED_BYTES).unwrap();
    assert_eq!((middle.total_lines, middle.next_line), (None, Some(3)));
}

#[test]
fn scanning_stops_at_its_limit_both_ways() {
    let text: String = (1..=100).map(|n| format!("line {n}\n")).collect();
    let far = read_small_limits(text.as_bytes(), ask(90, None, 64), 4, 100).unwrap_err();
    assert_eq!(far.code, "too_far");
    let deep = read_small_limits(text.as_bytes(), ask(-90, None, 64), 4, 100).unwrap_err();
    assert_eq!(deep.code, "too_far");
    assert!(read_small_limits(text.as_bytes(), ask(2, None, 64), 4, 100).is_ok());
}

#[test]
fn a_log_that_grows_while_read_is_read_as_it_was() {
    // The size taken when the call began covers 5 lines; the file now holds 10.
    let grown: String = (1..=10).map(|n| format!("{n}\n")).collect();
    let (_keep, mut file, _) = file_with(grown.as_bytes());
    let before = 10; // "1\n" .. "5\n"
    let tail = read_lines(&mut file, before, ask(-2, None, 1024)).unwrap();
    assert_eq!(tail.text, "4\n5\n");
    assert_eq!((tail.start_line, tail.total_lines, tail.eof), (Some(4), Some(5), true));
    let page = read_lines(&mut file, before, ask(3, None, 1024)).unwrap();
    assert_eq!((page.text.as_str(), page.next_offset), ("3\n4\n5\n", 10));
    // Shorter than when the call began: said so, not a broken read.
    let shrunk = read_lines(&mut file, 400, ask(-2, None, 1024)).unwrap_err();
    assert_eq!(shrunk.code, "changed_while_reading");
}
