//! No brand, domain or operator in the hub's code: every name a person or an AI reads comes
//! from the site. This walks `crates/hub/src` the way the compiler does, from `lib.rs`
//! through each `mod`, reads every string literal outside comments and `#[cfg(test)]`
//! items, and every file the hub serves or runs as it is (scripts, styles), and fails on a
//! brand literal. The program's own name in lowercase (`devagotchi`, its binary, paths and
//! service account) is not a brand; the wire names below cannot change without breaking
//! machines and clients already out there.

use std::path::{Path, PathBuf};

/// What may not appear: the default site's product, its domain, its operator.
const BRANDS: [&str; 3] = ["Devagotchi", "devagotchi.com", "AngryVibes"];

/// Wire names that carry the product's name and stay as they are, and why.
const WIRE: [(&str, &str); 2] = [
    // The seat header machines and AI clients already send (`board/mcp.rs`).
    ("X-Devagotchi-Seat", "an HTTP header on the wire"),
    // Where Windows machines that already joined keep the install.
    (
        "$env:LOCALAPPDATA 'Devagotchi'",
        "the install folder of joined Windows PCs",
    ),
];

fn src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// One string literal: where it is and what it says.
struct Literal {
    line: usize,
    text: String,
}

/// What one Rust file holds: its non-test string literals and the modules it declares.
struct Scanned {
    literals: Vec<Literal>,
    /// `(name, declared under #[cfg(test)], #[path])`.
    modules: Vec<(String, bool, Option<String>)>,
}

fn scan(text: &str) -> Scanned {
    let chars: Vec<char> = text.chars().collect();
    let at = |i: usize| chars.get(i).copied().unwrap_or('\0');
    let before = |i: usize| i.checked_sub(1).map(|j| chars[j]).unwrap_or(' ');
    let starts = |i: usize, s: &str| s.chars().enumerate().all(|(k, c)| at(i + k) == c);
    let mut out = Scanned {
        literals: Vec::new(),
        modules: Vec::new(),
    };
    let (mut i, mut line, mut depth) = (0usize, 1usize, 0usize);
    // Brackets and parentheses: a `;` inside them (`[&str; 2]`) ends no item.
    let mut nest = 0usize;
    // An item under #[cfg(test)]: the depth it started at, and whether its body opened.
    let mut test_item: Option<(usize, bool)> = None;
    let mut pending_path: Option<String> = None;
    while i < chars.len() {
        let c = chars[i];
        let hidden = test_item.is_some();
        if c == '\n' {
            line += 1;
            i += 1;
            continue;
        }
        if starts(i, "//") {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if starts(i, "/*") {
            let mut nest = 0;
            while i < chars.len() {
                if starts(i, "/*") {
                    nest += 1;
                    i += 2;
                } else if starts(i, "*/") {
                    nest -= 1;
                    i += 2;
                    if nest == 0 {
                        break;
                    }
                } else {
                    if chars[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
            }
            continue;
        }
        let word_edge = !before(i).is_alphanumeric() && before(i) != '_';
        // Raw strings: r"…", r#"…"#, br#"…"#.
        let raw_start = if word_edge && starts(i, "br") && matches!(at(i + 2), '"' | '#') {
            Some(i + 2)
        } else if word_edge && c == 'r' && matches!(at(i + 1), '"' | '#') {
            Some(i + 1)
        } else {
            None
        };
        if let Some(mut j) = raw_start {
            let mut hashes = 0;
            while at(j) == '#' {
                hashes += 1;
                j += 1;
            }
            if at(j) == '"' {
                let begin = line;
                j += 1;
                let mut body = String::new();
                while j < chars.len() {
                    if chars[j] == '"' && (1..=hashes).all(|k| at(j + k) == '#') {
                        j += 1 + hashes;
                        break;
                    }
                    if chars[j] == '\n' {
                        line += 1;
                    }
                    body.push(chars[j]);
                    j += 1;
                }
                if !hidden {
                    out.literals.push(Literal {
                        line: begin,
                        text: body,
                    });
                }
                i = j;
                continue;
            }
        }
        if c == '"' || (c == 'b' && word_edge && at(i + 1) == '"') {
            let begin = line;
            let mut j = if c == 'b' { i + 2 } else { i + 1 };
            let mut body = String::new();
            while j < chars.len() && chars[j] != '"' {
                if chars[j] == '\\' {
                    body.push(chars[j]);
                    j += 1;
                }
                if at(j) == '\n' {
                    line += 1;
                }
                body.push(at(j));
                j += 1;
            }
            if let Some(path) = pending_path.as_mut().filter(|p| p.is_empty()) {
                *path = body.clone();
            }
            if !hidden {
                out.literals.push(Literal {
                    line: begin,
                    text: body,
                });
            }
            i = j + 1;
            continue;
        }
        if c == '\'' {
            // A char literal ('x', '\n', '\'') or a lifetime ('a).
            if at(i + 1) == '\\' {
                let mut j = i + 3;
                while j < chars.len() && chars[j] != '\'' {
                    j += 1;
                }
                i = j + 1;
            } else if at(i + 2) == '\'' {
                i += 3;
            } else {
                i += 1;
            }
            continue;
        }
        if c == '#' && (at(i + 1) == '[' || (at(i + 1) == '!' && at(i + 2) == '[')) {
            let open = if at(i + 1) == '[' { i + 2 } else { i + 3 };
            let compact: String = chars[open..chars.len().min(open + 64)]
                .iter()
                .filter(|c| !c.is_whitespace())
                .collect();
            if compact.starts_with("cfg(test)]") || compact.starts_with("cfg(all(test") {
                if test_item.is_none() {
                    test_item = Some((depth, false));
                }
            } else if compact.starts_with("path=") {
                pending_path = Some(String::new());
            }
            i += 1;
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let mut j = i;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            if word == "mod" {
                let mut k = j;
                while at(k).is_whitespace() {
                    k += 1;
                }
                let name_start = k;
                while at(k).is_alphanumeric() || at(k) == '_' {
                    k += 1;
                }
                let name: String = chars[name_start..k].iter().collect();
                while at(k).is_whitespace() {
                    k += 1;
                }
                if at(k) == ';' && !name.is_empty() {
                    let path = pending_path.take().filter(|p| !p.is_empty());
                    out.modules.push((name, test_item.is_some(), path));
                }
            }
            i = j;
            continue;
        }
        match c {
            '{' => {
                if let Some((start, false)) = test_item
                    && start == depth
                {
                    test_item = Some((start, true));
                }
                depth += 1;
            }
            '}' => {
                depth = depth.saturating_sub(1);
                if let Some((start, true)) = test_item
                    && depth == start
                {
                    test_item = None;
                }
            }
            '(' | '[' => nest += 1,
            ')' | ']' => nest = nest.saturating_sub(1),
            ';' if nest == 0 => {
                if let Some((start, false)) = test_item
                    && start == depth
                {
                    test_item = None;
                }
                pending_path = None;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// Where `mod name;` in `file` lives.
fn child(file: &Path, name: &str, path: Option<&str>) -> PathBuf {
    let parent = file.parent().expect("a file has a directory");
    if let Some(path) = path {
        return parent.join(path);
    }
    let stem = file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let dir = if matches!(stem, "lib" | "mod" | "main") {
        parent.to_path_buf()
    } else {
        parent.join(stem)
    };
    let flat = dir.join(format!("{name}.rs"));
    if flat.exists() {
        flat
    } else {
        dir.join(name).join("mod.rs")
    }
}

fn brand_in(text: &str) -> Option<&'static str> {
    let mut text = text.to_string();
    for (wire, _) in WIRE {
        text = text.replace(wire, "");
    }
    BRANDS.into_iter().find(|brand| text.contains(brand))
}

#[test]
fn no_brand_literal_is_left_in_the_hubs_code() {
    let root = src();
    let mut found = Vec::new();
    let mut reached = std::collections::BTreeSet::new();
    let mut queue = vec![(root.join("lib.rs"), false)];
    while let Some((file, test_only)) = queue.pop() {
        let text = std::fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
        reached.insert(file.clone());
        let scanned = scan(&text);
        for (name, in_test, path) in scanned.modules {
            queue.push((child(&file, &name, path.as_deref()), test_only || in_test));
        }
        if test_only {
            continue;
        }
        for literal in scanned.literals {
            if let Some(brand) = brand_in(&literal.text) {
                found.push(format!(
                    "{}:{}: {brand:?} in {:?}",
                    file.strip_prefix(&root).unwrap_or(&file).display(),
                    literal.line,
                    literal.text.chars().take(100).collect::<String>()
                ));
            }
        }
    }
    // Every Rust file is reached from lib.rs, so none hides from this.
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                assert!(
                    reached.contains(&path),
                    "{} is in no module",
                    path.display()
                );
            } else {
                // Served or run as it is: the stylesheets, the page scripts, the join scripts.
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                for (n, line) in text.lines().enumerate() {
                    if let Some(brand) = brand_in(line) {
                        found.push(format!(
                            "{}:{}: {brand:?} in {line:?}",
                            path.strip_prefix(&root).unwrap_or(&path).display(),
                            n + 1
                        ));
                    }
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "names belong in the site (crates/hub/site/default), not the code:\n{}",
        found.join("\n")
    );
}

#[test]
fn the_scan_sees_what_it_should_and_skips_what_it_should() {
    let scanned = scan(
        r##"
// "Devagotchi" in a comment
/* "Devagotchi" in a block /* nested */ still */
const A: &str = "shown";
const B: &str = r#"raw "quoted""#;
const C: char = '"';
const N: char = '\n';
fn lifetime<'a>(x: &'a str) -> &'a str { x }
#[tool(description = "a description")]
fn tool() {}
#[cfg(test)]
mod tests;
#[cfg(test)]
#[path = "x/tests.rs"]
mod more;
#[cfg(test)]
mod inline { const D: &str = "hidden"; }
#[cfg(test)]
pub const E: [&str; 1] = ["hidden too"];
mod real;
const F: &str = "after";
"##,
    );
    let texts: Vec<&str> = scanned.literals.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(
        texts,
        ["shown", "raw \"quoted\"", "a description", "after"],
        "{texts:?}"
    );
    assert_eq!(
        scanned.modules,
        [
            ("tests".to_string(), true, None),
            ("more".to_string(), true, Some("x/tests.rs".to_string())),
            ("real".to_string(), false, None),
        ]
    );
    assert_eq!(brand_in("joined Devagotchi"), Some("Devagotchi"));
    assert_eq!(
        brand_in("https://app.devagotchi.com"),
        Some("devagotchi.com")
    );
    assert_eq!(brand_in("X-Devagotchi-Seat must be a name"), None);
    assert_eq!(brand_in("devagotchi login"), None);
}
