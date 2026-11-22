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
//!   quote, so it excludes the `replace()` message and the shared
//!   `upper()`/`lower()`/`join()` message from `builtin_output_too_large` in `builtins.rs`;
//! * `push_capped` is defined once, and every output accumulator calls it the expected
//!   number of times with no raw append (`push_str`, `push`, `+=`, …) left in its body.
//! * The compile-memory message and the frontmatter allowance message each appear
//!   only in their constructors.
//! * The listed files do not contain the four cfg key names `target_arch`,
//!   `target_family`, `target_os` and `target_pointer_width`. Other cfg forms stay
//!   allowed: `cfg(unix)`, `cfg(windows)`, `cfg(not(windows))` and the other target
//!   keys, such as `target_env`. `cfg(not(windows))` is true on wasm32, so this check
//!   does not prove that the listed code is the same on each target.
//! * The budget module code does not use `RefCell` or these panic tokens: `panic`,
//!   `unreachable`, `todo`, `unimplemented`, `unwrap`, `expect` and the assert macros.
//!   The check does not find other code that can panic, such as an index.
//!
//! The message and append checks scan `crates/mds-core/src/**`. They skip `*_tests.rs`,
//! `#[cfg(test)]` items, and comments. Only the message checks scan string literals.
//! The architecture check scans the listed files as text. The budget module check scans code.

use std::path::{Path, PathBuf};

/// The output-cap message, anchored at the opening quote of its string literal.
const MESSAGE: &str = "\"output exceeds maximum size";

/// The same text unanchored: the `builtins.rs` messages carry it mid-literal.
const MESSAGE_UNANCHORED: &str = "output exceeds maximum size";

const BUDGET_MESSAGE: &str = "\"compile memory exceeds maximum of ";
const BUDGET_CONSTRUCTOR: &str = "fn compile_memory_exceeded(";

const ALLOWANCE_MESSAGE: &str = "\"frontmatter YAML expands beyond maximum of ";
const ALLOWANCE_CONSTRUCTOR: &str = "fn frontmatter_expansion_exceeded(";

/// The file and constructors that build the messages.
const CHOKE_POINT: &str = "error.rs";
const MESSAGES: &[(&str, &str)] = &[
    (MESSAGE, "fn output_size_exceeded("),
    (BUDGET_MESSAGE, BUDGET_CONSTRUCTOR),
    (ALLOWANCE_MESSAGE, ALLOWANCE_CONSTRUCTOR),
];

const BUDGET_SOURCE: &str = "evaluator/budget.rs";
const ARCH_INDEPENDENT: &[&str] = &[
    "evaluator.rs",
    "resolver.rs",
    "builtins.rs",
    "sourcemap.rs",
    "lib.rs",
    "scope.rs",
    "value.rs",
    "limits.rs",
    "resolver/charges.rs",
    "resolver/context.rs",
    "resolver/frontmatter.rs",
    "resolver/import_paths.rs",
    "resolver/inheritance.rs",
    "resolver/module.rs",
    "resolver/module_scope.rs",
    BUDGET_SOURCE,
    "evaluator/held.rs",
];

/// The four cfg key names that the listed files must not contain. `cfg(unix)`,
/// `cfg(windows)` and `cfg(not(windows))` stay allowed. `lib.rs` uses `cfg(windows)`
/// and `cfg(not(windows))` for host paths, and `cfg(not(windows))` is true on wasm32.
/// The other target keys, such as `target_env` and `target_vendor`, also stay allowed.
const TARGET_KEYS: &[&str] = &[
    "target_arch",
    "target_family",
    "target_os",
    "target_pointer_width",
];

/// The capped append helper.
const HELPER_DEF: &str = "fn push_capped(";
const HELPER_CALL: &str = "push_capped(";

/// Every output accumulator: its file under `src/`, the anchor that opens it, and how
/// many capped appends it makes. Leaf text reaches the one append in
/// `evaluate_nodes`. Loops and regions pass their buffer to `evaluate_nodes`.
const ACCUMULATORS: &[(&str, &str, usize)] = &[
    ("evaluator.rs", "fn evaluate_nodes(", 1),
    ("evaluator.rs", "fn evaluate_for(", 0),
    ("resolver.rs", "fn evaluate_regions_with_map(", 0),
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

    let mut message_totals = [0; MESSAGES.len()];
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

        for ((message, constructor), total) in MESSAGES.iter().zip(&mut message_totals) {
            *total += message_hits(&name, &code, &code_and_strings, message, constructor)
                .unwrap_or_else(|error| panic!("{error}"));
        }
        if name != CHOKE_POINT {
            unanchored_elsewhere += code_and_strings.matches(MESSAGE_UNANCHORED).count();
        }
    }

    // Non-vacuity: the scanner finds the message where it must live, and the anchor —
    // not a blind matcher — is what excludes the built-in messages, which carry the
    // same words mid-literal.
    for ((message, constructor), total) in MESSAGES.iter().zip(message_totals) {
        assert_eq!(
            total, 1,
            "`{CHOKE_POINT}` must build `{message}` exactly once in `{constructor}`"
        );
    }
    assert!(
        unanchored_elsewhere >= 2,
        "non-vacuity: the unanchored match must find the `replace()` message and the shared \
         `upper()`/`lower()`/`join()` message from `builtin_output_too_large` in `builtins.rs` \
         (found {unanchored_elsewhere})"
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

fn message_hits(
    name: &str,
    code: &str,
    code_and_strings: &str,
    message: &str,
    constructor: &str,
) -> Result<usize, String> {
    let mut hits = code_and_strings.match_indices(message).peekable();
    if hits.peek().is_none() {
        return Ok(0);
    }
    let misplaced = || format!("{name}: build `{message}` only in `{constructor}`");
    if name != CHOKE_POINT {
        return Err(misplaced());
    }
    let body = function_range(code, constructor, name);
    hits.try_fold(0, |total, (at, _)| {
        if body.contains(&at) {
            Ok(total + 1)
        } else {
            Err(misplaced())
        }
    })
}

fn check_budget_source(name: &str, raw: &str) -> Result<(), String> {
    if let Some(key) = TARGET_KEYS.iter().find(|key| raw.contains(**key)) {
        return Err(format!("{name}: do not use `{key}`"));
    }
    if name == BUDGET_SOURCE {
        let code = mask(raw, true);
        let forbidden = code
            .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
            .find(|token| {
                matches!(
                    *token,
                    "assert"
                        | "assert_eq"
                        | "assert_ne"
                        | "debug_assert"
                        | "debug_assert_eq"
                        | "debug_assert_ne"
                        | "expect"
                        | "unwrap"
                        | "panic"
                        | "unreachable"
                        | "todo"
                        | "unimplemented"
                        | "RefCell"
                )
            });
        if let Some(token) = forbidden {
            return Err(format!("{name}: do not use `{token}`"));
        }
    }
    Ok(())
}

/// Check each listed file, with its text from `read`.
fn check_listed_sources(read: impl Fn(&str) -> String) -> Result<(), String> {
    ARCH_INDEPENDENT
        .iter()
        .try_for_each(|name| check_budget_source(name, &read(name)))
}

#[test]
fn budget_rules_apply_to_each_source_file() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    check_listed_sources(|name| {
        std::fs::read_to_string(src_dir.join(name)).expect("source must be readable")
    })
    .unwrap_or_else(|error| panic!("{error}"));
}

#[test]
fn arch_independent_lists_every_resolver_and_evaluator_child() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut walked = Vec::new();
    for folder in ["resolver", "evaluator"] {
        let before = walked.len();
        for entry in std::fs::read_dir(src_dir.join(folder)).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            assert!(
                !kind.is_dir() && !kind.is_symlink(),
                "unexpected source entry: {entry:?}"
            );
            let path = entry.path();
            let name = rel(&src_dir, &path);
            if kind.is_file()
                && path.extension().is_some_and(|ext| ext == "rs")
                && !name.ends_with("_tests.rs")
            {
                walked.push(name);
            }
        }
        assert!(walked.len() > before, "no production files in {folder}/");
    }
    assert!(
        walked.len() >= 9,
        "expected at least nine children: {walked:?}"
    );
    let missing = unlisted(&walked, ARCH_INDEPENDENT);
    assert!(
        missing.is_empty(),
        "add these files to ARCH_INDEPENDENT: {missing:?}"
    );
}

fn unlisted<'a>(walked: &'a [String], listed: &[&str]) -> Vec<&'a str> {
    let mut missing: Vec<_> = walked
        .iter()
        .map(String::as_str)
        .filter(|name| !listed.contains(name))
        .collect();
    missing.sort_unstable();
    missing
}

#[test]
fn arch_independent_completeness_flags_an_unlisted_child() {
    let walked = [
        "resolver/inheritance.rs".to_owned(),
        "resolver/planted.rs".to_owned(),
    ];
    assert_eq!(unlisted(&walked, ARCH_INDEPENDENT), ["resolver/planted.rs"]);
}

#[test]
fn budget_gates_reject_planted_text() {
    for (message, constructor, limit) in [
        (BUDGET_MESSAGE, BUDGET_CONSTRUCTOR, "209715200"),
        (ALLOWANCE_MESSAGE, ALLOWANCE_CONSTRUCTOR, "52428800"),
    ] {
        let literal = format!("{message}{limit} bytes\"");
        let valid = format!("{constructor}) -> &'static str {{ {literal} }}");
        let extra = format!("fn other() -> &'static str {{ {literal} }}");
        let at = |name: &str, raw: &str| {
            message_hits(
                name,
                &mask(raw, true),
                &mask(raw, false),
                message,
                constructor,
            )
        };
        // Control: the literal inside its constructor passes.
        assert_eq!(at(CHOKE_POINT, &valid), Ok(1));
        for (name, raw) in [
            ("resolver/frontmatter.rs", extra.clone()),
            (CHOKE_POINT, format!("{valid}\n{extra}")),
        ] {
            assert_eq!(
                at(name, &raw),
                Err(format!("{name}: build `{message}` only in `{constructor}`"))
            );
        }
    }
    for (key, value) in TARGET_KEYS.iter().zip(["wasm32", "wasm", "unknown", "32"]) {
        assert_eq!(
            check_budget_source(
                "evaluator.rs",
                &format!("#[cfg({key} = \"{value}\")] fn charge() {{}}")
            ),
            Err(format!("evaluator.rs: do not use `{key}`"))
        );
    }
    // Control: the host-path cfg forms stay allowed. `cfg(not(windows))` is true on
    // wasm32.
    for form in ["unix", "windows", "not(windows)"] {
        assert_eq!(
            check_budget_source("lib.rs", &format!("#[cfg({form})] fn native() {{}}")),
            Ok(())
        );
    }
    // The file list reaches `limits.rs`.
    let planted = |name: &str| {
        if name == "limits.rs" {
            "#[cfg(target_pointer_width = \"32\")] pub(crate) const MAX: usize = 1;".into()
        } else {
            String::new()
        }
    };
    assert_eq!(
        check_listed_sources(planted),
        Err("limits.rs: do not use `target_pointer_width`".into())
    );
    for token in ["assert", "unreachable", "todo", "unimplemented"] {
        assert_eq!(
            check_budget_source(BUDGET_SOURCE, &format!("fn charge() {{ {token}!(); }}")),
            Err(format!("evaluator/budget.rs: do not use `{token}`"))
        );
    }
}

#[test]
fn mask_keeps_long_lifetimes_and_masks_a_complete_char_escape() {
    let prefix = "fn f<'long_lifetime>() { out += ";
    let literal = r"'\u{10ffff}'";
    let raw = format!("{prefix}{literal}; }}");
    assert_eq!(
        mask(&raw, true),
        format!("{prefix}{}; }}", " ".repeat(literal.len()))
    );
    assert_eq!(mask(&raw, false), raw);
}

#[test]
fn mask_keeps_an_append_between_a_lifetime_and_a_nearby_char() {
    let raw = "'a:{s+='x';}";
    assert_eq!(mask(raw, true), "'a:{s+=   ;}");
    assert_eq!(mask(raw, false), raw);
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
            // A char literal holds exactly one character or one complete escape.
            // A lifetime ends at its identifier, not at a nearby literal's quote.
            let mut j = i + 1;
            let first = src[j..].chars().next();
            if first == Some('\\') {
                j += 1;
                match b.get(j) {
                    Some(b'u') if b.get(j + 1) == Some(&b'{') => {
                        j += 2;
                        // Bounded by the source length, not by an escape-size guess.
                        while j < b.len() && !matches!(b[j], b'}' | b'\'') {
                            j += 1;
                        }
                        if b.get(j) == Some(&b'}') {
                            j += 1;
                        }
                    }
                    Some(b'x') => j = (j + 3).min(b.len()),
                    Some(_) => j += 1,
                    None => {}
                }
            } else if let Some(ch) = first {
                j += ch.len_utf8();
            }
            if b.get(j) == Some(&b'\'') {
                let end = j + 1;
                keep(&mut out, i, end);
                i = end;
                continue;
            }
            if first.is_some_and(|ch| ch == '_' || ch.is_alphabetic()) {
                for ch in src[j..].chars() {
                    if ch != '_' && !ch.is_alphanumeric() {
                        break;
                    }
                    j += ch.len_utf8();
                }
                out[i..j].copy_from_slice(&b[i..j]);
                i = j;
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
