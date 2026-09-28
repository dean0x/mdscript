//! Funnel guard (#415): the output cap is enforced at one append helper and reported
//! by one error constructor.
//!
//! A cap checked at only some appends is silently bypassed by the others (PF-004
//! shape), and a cap checked after a node has finished lets a loop spend all of its
//! memory before the check sees it. This test turns "did every accumulator switch to
//! the capped append?" into a machine-checked invariant:
//!
//! * the output-cap message — a string literal STARTING `output exceeds maximum size`
//!   — appears in production code only in `error.rs`, inside
//!   `MdsError::output_size_exceeded`. The match is anchored at the literal's opening
//!   quote, so the `mds::builtin` messages of `replace()` and `join()` in `builtins.rs`
//!   (`"replace() output exceeds …"`) are not the output cap and do not count;
//! * `push_capped` is defined once, and every output accumulator calls it the expected
//!   number of times with no raw append (`push_str`, `push`, `+=`, …) left in its body.
//!
//! Scope: `crates/mds-core/src/**`, excluding `*_tests.rs` files and `#[cfg(test)]`
//! items. Comments never count; string literals count only for the message check.

use std::path::{Path, PathBuf};

/// The output-cap message, anchored at the opening quote of its string literal.
const MESSAGE: &str = "\"output exceeds maximum size";

/// The same text unanchored: the `builtins.rs` messages carry it mid-literal.
const MESSAGE_UNANCHORED: &str = "output exceeds maximum size";

/// The one file allowed to hold the message, and the constructor that must hold it.
const CHOKE_POINT: &str = "error.rs";
const CONSTRUCTOR: &str = "fn output_size_exceeded(";

/// The capped append helper.
const HELPER_DEF: &str = "fn push_capped(";
const HELPER_CALL: &str = "push_capped(";

/// Every output accumulator: its file under `src/`, the anchor that opens it, and how
/// many capped appends it makes. `evaluate_nodes` has one, after its `match`: every
/// arm that renders text (Text, EscapedBrace, Interpolation, If, For, Include,
/// Message, Block) yields it there.
const ACCUMULATORS: &[(&str, &str, usize)] = &[
    ("evaluator.rs", "fn evaluate_nodes(", 1),
    ("evaluator.rs", "fn evaluate_for(", 1),
    ("resolver.rs", "fn evaluate_regions_with_map(", 1),
];

/// Appends that bypass the cap.
const RAW_APPENDS: &[&str] = &[
    "push_str(",
    ".push(",
    "insert_str(",
    ".insert(",
    "+=",
    ".extend(",
    "write!(",
    "writeln!(",
];

#[test]
fn output_cap_is_funnelled_through_one_helper_and_one_constructor() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = rust_files(&src_dir);

    // Non-vacuity: the source tree was actually walked.
    assert!(
        files.len() >= 10,
        "non-vacuity: expected to walk at least 10 source files under {}, found {}",
        src_dir.display(),
        files.len()
    );

    let mut violations: Vec<String> = Vec::new();
    let mut choke_point_hits = 0usize;
    let mut unanchored_elsewhere = 0usize;
    let mut helper_defs = 0usize;

    for file in &files {
        if file
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with("_tests.rs"))
        {
            continue;
        }
        let raw = std::fs::read_to_string(file).expect("source must be readable");
        let code = mask(&raw, true);
        let code_and_strings = blank_ranges(&mask(&raw, false), &cfg_test_ranges(&code));
        let code = blank_ranges(&code, &cfg_test_ranges(&code));
        let name = rel(&src_dir, file);

        helper_defs += code.matches(HELPER_DEF).count();

        let hits: Vec<usize> = code_and_strings
            .match_indices(MESSAGE)
            .map(|(at, _)| at)
            .collect();
        if name == CHOKE_POINT {
            choke_point_hits += hits.len();
            if hits.is_empty() {
                continue;
            }
            let body = function_range(&code, CONSTRUCTOR, &name);
            for at in &hits {
                assert!(
                    body.contains(at),
                    "{name}: the output-cap message must be built inside \
                     `MdsError::output_size_exceeded`, found one outside it"
                );
            }
        } else {
            for _ in &hits {
                violations.push(format!("  {name}: contains the output-cap message"));
            }
            unanchored_elsewhere += code_and_strings.matches(MESSAGE_UNANCHORED).count();
        }
    }

    // Non-vacuity: the scanner finds the message where it must live, and the anchor —
    // not a blind matcher — is what excludes the built-in messages, which carry the
    // same words mid-literal.
    assert_eq!(
        choke_point_hits, 1,
        "non-vacuity: `{CHOKE_POINT}` must build the output-cap message exactly once"
    );
    assert!(
        unanchored_elsewhere >= 2,
        "non-vacuity: the `replace()` and `join()` messages must still be seen unanchored \
         (found {unanchored_elsewhere}), or the anchored match proves nothing"
    );
    assert!(
        violations.is_empty(),
        "output-cap message outside `MdsError::output_size_exceeded` ({} found):\n{}\n\n\
         Build the error with `MdsError::output_size_exceeded()` instead.",
        violations.len(),
        violations.join("\n")
    );

    assert_eq!(helper_defs, 1, "`push_capped` must be defined exactly once");

    for (file, anchor, calls) in ACCUMULATORS {
        let path = src_dir.join(file);
        let raw = std::fs::read_to_string(&path).expect("source must be readable");
        let code = mask(&raw, true);
        let code = blank_ranges(&code, &cfg_test_ranges(&code));
        let body = &code[function_range(&code, anchor, file)];

        assert_eq!(
            body.matches(HELPER_CALL).count(),
            *calls,
            "{file}: `{anchor}` must append through `push_capped` exactly {calls} time(s)"
        );
        for raw_append in RAW_APPENDS {
            assert!(
                !body.contains(raw_append),
                "{file}: `{anchor}` must not append with `{raw_append}` — every output \
                 append goes through `push_capped`, which checks the cap first"
            );
        }
    }
}

/// Byte range of the function opened by the unique `anchor`, from the anchor to its
/// matching closing brace, in masked source.
fn function_range(code: &str, anchor: &str, file: &str) -> std::ops::Range<usize> {
    let hits: Vec<usize> = code.match_indices(anchor).map(|(at, _)| at).collect();
    assert_eq!(
        hits.len(),
        1,
        "{file}: anchor `{anchor}` must occur exactly once in production code; found {}",
        hits.len()
    );
    let open = code[hits[0]..]
        .find('{')
        .map(|r| hits[0] + r)
        .unwrap_or_else(|| panic!("{file}: `{anchor}` has no body"));
    let close =
        match_brace(code, open).unwrap_or_else(|| panic!("{file}: `{anchor}` body is unclosed"));
    hits[0]..close + 1
}

fn rel(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    // Bounded: the source tree is finite and acyclic (read_dir does not traverse symlinks).
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file() && p.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Replace the content of comments — and, when `strings` is set, of string and char
/// literals — with spaces, preserving length and newlines, so byte offsets agree
/// between the two masked forms. Literals are always parsed, so a `//` inside one is
/// never taken for a comment.
fn mask(src: &str, strings: bool) -> String {
    let b = src.as_bytes();
    let mut out: Vec<u8> = b
        .iter()
        .map(|&c| if c == b'\n' { b'\n' } else { b' ' })
        .collect();
    let keep = |out: &mut Vec<u8>, from: usize, to: usize| {
        if !strings {
            out[from..to].copy_from_slice(&b[from..to]);
        }
    };
    let mut i = 0usize;
    // Bounded: `i` advances by at least one on every path.
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                i += 1;
            }
            i = (i + 2).min(b.len());
            continue;
        }
        if b[i] == b'r' && matches!(b.get(i + 1), Some(&b'"') | Some(&b'#')) && !prev_is_ident(b, i)
        {
            let mut hashes = 0usize;
            let mut j = i + 1;
            while b.get(j) == Some(&b'#') {
                hashes += 1;
                j += 1;
            }
            if b.get(j) == Some(&b'"') {
                j += 1;
                while j < b.len() {
                    if b[j] == b'"' && (0..hashes).all(|k| b.get(j + 1 + k) == Some(&b'#')) {
                        j += 1 + hashes;
                        break;
                    }
                    j += 1;
                }
                let end = j.min(b.len());
                keep(&mut out, i, end);
                i = end;
                continue;
            }
        }
        if b[i] == b'"' {
            let mut j = i + 1;
            while j < b.len() {
                if b[j] == b'\\' {
                    j += 2;
                    continue;
                }
                if b[j] == b'"' {
                    j += 1;
                    break;
                }
                j += 1;
            }
            let end = j.min(b.len());
            keep(&mut out, i, end);
            i = end;
            continue;
        }
        if b[i] == b'\'' {
            // A char literal, not a lifetime: a closing quote follows within a few bytes.
            let mut j = i + 1;
            let mut closed = false;
            let mut steps = 0;
            while j < b.len() && steps < 8 {
                if b[j] == b'\\' {
                    j += 2;
                    steps += 1;
                    continue;
                }
                if b[j] == b'\'' {
                    closed = true;
                    j += 1;
                    break;
                }
                j += 1;
                steps += 1;
            }
            if closed {
                let end = j.min(b.len());
                keep(&mut out, i, end);
                i = end;
                continue;
            }
        }
        out[i] = b[i];
        i += 1;
    }
    String::from_utf8(out).expect("masking preserves UTF-8 boundaries on ASCII delimiters")
}

/// Byte ranges of every `#[cfg(test)]`-guarded item in fully masked source, from the
/// attribute to the item's end: its first `;` (a `mod name;` declaration, whose body
/// is an external `*_tests.rs` skipped separately) or the brace matching its first `{`
/// (an inline `mod tests { … }`, or a test-only `fn`). The item ends there, never at
/// a later `mod` — a `#[cfg(test)] fn` above `mod tests` would otherwise hide all the
/// production code between them.
fn cfg_test_ranges(code: &str) -> Vec<std::ops::Range<usize>> {
    const ATTR: &str = "#[cfg(test)]";
    let mut ranges = Vec::new();
    let mut from = 0usize;
    // Bounded: each iteration moves `from` past the attribute it examined.
    while let Some(rel) = code[from..].find(ATTR) {
        let attr = from + rel;
        let item = attr + ATTR.len();
        from = item;
        let end = match (code[item..].find('{'), code[item..].find(';')) {
            (Some(bo), semi) if semi.is_none_or(|s| bo < s) => {
                match_brace(code, item + bo).map(|close| close + 1)
            }
            (_, Some(so)) => Some(item + so + 1),
            _ => None,
        };
        if let Some(end) = end {
            ranges.push(attr..end);
            from = end;
        }
    }
    ranges
}

/// `text` with every byte in `ranges` (except newlines) replaced by a space.
fn blank_ranges(text: &str, ranges: &[std::ops::Range<usize>]) -> String {
    let mut out = text.as_bytes().to_vec();
    for range in ranges {
        for byte in &mut out[range.clone()] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    }
    String::from_utf8(out).expect("blanking with ASCII spaces keeps UTF-8 valid")
}

/// Is the byte before `i` part of an identifier (so `r` is a suffix, not a raw-string
/// prefix, e.g. `str` / `for`)?
fn prev_is_ident(b: &[u8], i: usize) -> bool {
    i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')
}

/// Index of the `}` matching the `{` at `open` in masked text.
fn match_brace(text: &str, open: usize) -> Option<usize> {
    let b = text.as_bytes();
    let mut depth = 0i32;
    // Bounded: `i` walks forward over a finite slice.
    for (i, &c) in b.iter().enumerate().skip(open) {
        match c {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}
