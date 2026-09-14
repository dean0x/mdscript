//! Write-funnel guard (#227): every artifact `mds` writes from production code must go
//! through the single atomic choke point `crate::output::atomic_write_file`.
//!
//! # Why this exists
//!
//! `atomic_write_file` is temp-file + fsync + rename: a crash or a mid-write error never
//! leaves a truncated artifact, and a symlink at the target is refused. A raw
//! `std::fs::write` at any *one* remaining site silently forfeits all of that for the
//! artifact it writes — and "did we remember every write site?" is an unbounded search
//! that three reviewers can each answer differently. This test converts it into a
//! machine-checked invariant: a raw write in `crates/mds-cli/src/**` is a failure unless
//! it appears in [`ALLOWED_RAW_WRITES`] with a written justification.
//!
//! # Scope and lexical limits (what this guard does NOT see)
//!
//! The scan is lexical. It matches the two needles in [`NEEDLES`] after masking comment
//! and string-literal text and stripping `#[cfg(test)] mod … { … }` blocks (test code
//! legitimately writes fixtures with `std::fs::write`). It therefore does NOT catch:
//!
//! - `OpenOptions::new(…).write(true)` followed by `write_all` on a hand-opened `File`.
//!   No such site exists in this crate today. Needling `OpenOptions::new(` was rejected
//!   deliberately: it would fire on read-only opens too, and an allow-list full of
//!   read-only entries is an allow-list nobody reads.
//! - A write reached through an alias (`use std::fs::write as w;`) or a helper in another
//!   crate.
//! - `#[cfg(test)] fn` items outside a `mod tests` block (the crate has none).
//!
//! The scanner helpers below are copied from `crates/mds-core/tests/yaml_funnel.rs`:
//! integration-test binaries are separate crates and cannot share code across crates.

use std::path::{Path, PathBuf};

/// Raw write entry points that must be funnelled. `std::fs::write(` contains
/// `fs::write(`, so the short form matches both the qualified and imported spellings.
const NEEDLES: &[&str] = &["fs::write(", "File::create("];

/// Production sites that may keep a raw write: `(file basename, needle, max hits, why)`.
///
/// `max` is exact, not a ceiling with slack: an entry whose site count drops to zero is
/// reported as dead by [`write_sites_are_funnelled`] and by
/// [`every_allowlist_entry_is_live`], so a removed site cannot leave a stale licence
/// behind for a future raw write to hide under.
const ALLOWED_RAW_WRITES: &[(&str, &str, usize, &str)] = &[
    (
        "main.rs",
        "fs::write(",
        1,
        "mds init creates a NEW file after an explicit exists-check; nothing to truncate (#227)",
    ),
    (
        "watch.rs",
        "fs::write(",
        1,
        "test-only readiness marker: written to <path>.tmp then renamed — already atomic",
    ),
];

#[test]
fn write_sites_are_funnelled() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = rust_files(&src_dir);

    // Non-vacuity: the source tree was actually walked.
    assert!(
        files.len() >= 5,
        "non-vacuity: expected to walk at least 5 source files under {}, found {}",
        src_dir.display(),
        files.len()
    );

    let mut violations: Vec<String> = Vec::new();
    // Hits observed per allow-list entry, indexed in step with ALLOWED_RAW_WRITES.
    let mut allowed_seen = vec![0usize; ALLOWED_RAW_WRITES.len()];
    let mut durability_pin_checked = false;

    for file in &files {
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .expect("source file names are UTF-8");
        let raw = std::fs::read_to_string(file).expect("source must be readable");
        let code = strip_cfg_test_mods(&mask_comments_and_strings(&raw));

        // Lexical pin for the durability tail of the primitive itself: the funnel is
        // only worth enforcing while what sits at the end of it still fsyncs and renames.
        if name == "output.rs" {
            assert!(
                code.contains(".sync_all()"),
                "output.rs must still call .sync_all() before the rename — the funnel \
                 guard is pointless if the choke point stops being durable"
            );
            assert!(
                code.contains(".persist("),
                "output.rs must still finish the write with a .persist() rename"
            );
            durability_pin_checked = true;
        }

        for needle in NEEDLES {
            let hits = needle_lines(&code, needle);
            if hits.is_empty() {
                continue;
            }
            let allowed = match ALLOWED_RAW_WRITES
                .iter()
                .position(|(f, n, _, _)| *f == name && n == needle)
            {
                Some(idx) => {
                    allowed_seen[idx] += hits.len();
                    ALLOWED_RAW_WRITES[idx].2
                }
                None => 0,
            };
            for line in hits.iter().skip(allowed) {
                violations.push(format!(
                    "  {name}:{line}: raw `{needle}` ({allowed} allow-listed for this file)"
                ));
            }
        }
    }

    assert!(
        durability_pin_checked,
        "non-vacuity: output.rs was never scanned — the walk did not reach the choke point"
    );

    // Anti-rot: an allow-list entry whose site is gone would licence a future raw write.
    let dead: Vec<String> = ALLOWED_RAW_WRITES
        .iter()
        .zip(&allowed_seen)
        .filter(|((_, _, max, _), seen)| *seen != max)
        .map(|((f, n, max, _), seen)| format!("  {f}: `{n}` expected {max} hit(s), found {seen}"))
        .collect();
    assert!(
        dead.is_empty(),
        "allow-list entry is dead or drifted ({} entr(ies)). Update ALLOWED_RAW_WRITES \
         to match reality — a stale entry is a licence for a raw write nobody reviewed.\n{}",
        dead.len(),
        dead.join("\n")
    );

    assert!(
        violations.is_empty(),
        "raw write sites outside the atomic funnel ({} found).\n{}\n\n\
         Route the write through `crate::output::atomic_write_file` (and create the parent \
         directory first — the primitive deliberately does not). If a site genuinely must \
         stay raw, add it to ALLOWED_RAW_WRITES with a written justification.",
        violations.len(),
        violations.join("\n")
    );
}

/// Positive self-check (the guard must be observed rejecting something before "no
/// violations" is evidence of anything). Operates on synthetic source strings only — no
/// filesystem access, so it cannot be satisfied by the repo happening to be clean.
#[test]
fn the_guard_flags_a_planted_raw_write() {
    // A raw write in ordinary production code is a violation.
    assert_eq!(
        scan_violation_count("fn f(p: &Path, s: &str) { std::fs::write(&p, s).unwrap(); }"),
        1,
        "a planted std::fs::write in a plain fn must be flagged"
    );

    // The same call inside a #[cfg(test)] mod is not.
    assert_eq!(
        scan_violation_count(
            "fn f() {}\n#[cfg(test)]\nmod tests {\n    fn t(p: &Path) { std::fs::write(&p, \"x\").unwrap(); }\n}\n"
        ),
        0,
        "a write inside #[cfg(test)] mod tests must not be flagged"
    );

    // Inside a line comment it is not.
    assert_eq!(
        scan_violation_count("fn f() {\n    // std::fs::write(&p, s);\n}\n"),
        0,
        "a needle inside a // comment must not be flagged"
    );

    // Inside a string literal it is not.
    assert_eq!(
        scan_violation_count("fn f() -> &'static str { \"std::fs::write(&p, s)\" }"),
        0,
        "a needle inside a string literal must not be flagged"
    );

    // The second needle is live too.
    assert_eq!(
        scan_violation_count("fn f(p: &Path) { let _ = std::fs::File::create(p); }"),
        1,
        "a planted File::create in a plain fn must be flagged"
    );
}

/// Anti-rot companion: every allow-list entry names a file that exists and still contains
/// at least one masked hit of its needle.
#[test]
fn every_allowlist_entry_is_live() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for (file, needle, max, why) in ALLOWED_RAW_WRITES {
        let path = src_dir.join(file);
        assert!(
            path.is_file(),
            "allow-list names {file}, which does not exist under {} (justification: {why})",
            src_dir.display()
        );
        let raw = std::fs::read_to_string(&path).expect("source must be readable");
        let code = strip_cfg_test_mods(&mask_comments_and_strings(&raw));
        let hits = count_occurrences(&code, needle);
        assert_eq!(
            hits, *max,
            "allow-list expects {max} raw `{needle}` in {file}, found {hits} \
             (justification: {why})"
        );
    }
}

// ── Scanner ───────────────────────────────────────────────────────────────────

/// Total needle hits in `src` after masking and `#[cfg(test)]` stripping.
fn scan_violation_count(src: &str) -> usize {
    let code = strip_cfg_test_mods(&mask_comments_and_strings(src));
    NEEDLES.iter().map(|n| count_occurrences(&code, n)).sum()
}

/// 1-based line numbers of every non-overlapping `needle` occurrence in `code`.
///
/// `mask_comments_and_strings` preserves both byte offsets and newlines, so a line
/// number computed over the masked text is the line number in the original source.
fn needle_lines(code: &str, needle: &str) -> Vec<usize> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    // Bounded: `start` advances by at least `needle.len()` each iteration.
    while let Some(pos) = code[start..].find(needle) {
        let abs = start + pos;
        lines.push(code[..abs].matches('\n').count() + 1);
        start = abs + needle.len();
    }
    lines
}

/// Count non-overlapping occurrences of `needle` in `haystack`.
fn count_occurrences(haystack: &str, needle: &str) -> usize {
    let mut count = 0;
    let mut start = 0;
    // Bounded: `start` advances by at least `needle.len()` each iteration.
    while let Some(pos) = haystack[start..].find(needle) {
        count += 1;
        start += pos + needle.len();
    }
    count
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

/// Replace the CONTENT of line comments, block comments, string literals and char
/// literals with spaces, preserving overall length and newlines. This keeps needles that
/// appear in rustdoc or inside a string from counting, and makes the brace matching in
/// [`strip_cfg_test_mods`] safe (no braces hide inside strings/comments).
fn mask_comments_and_strings(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = vec![b' '; b.len()];
    // Copy newlines through so line structure is preserved.
    for (i, &c) in b.iter().enumerate() {
        if c == b'\n' {
            out[i] = b'\n';
        }
    }
    let mut i = 0usize;
    while i < b.len() {
        // Line comment.
        if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // Block comment (not nested — Rust allows nesting, but the codebase does not rely
        // on it here; a needle would have to sit inside a nested comment to escape).
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                i += 1;
            }
            i = (i + 2).min(b.len());
            continue;
        }
        // Raw string: r"...", r#"..."#, r##"..."##, ... — only when `r` starts a token.
        if b[i] == b'r'
            && matches!(b.get(i + 1), Some(&b'"') | Some(&b'#'))
            && !prev_is_ident_byte(b, i)
        {
            let mut hashes = 0usize;
            let mut j = i + 1;
            while b.get(j) == Some(&b'#') {
                hashes += 1;
                j += 1;
            }
            if b.get(j) == Some(&b'"') {
                j += 1;
                // Scan to a `"` followed by exactly `hashes` `#`.
                while j < b.len() {
                    if b[j] == b'"' && (0..hashes).all(|k| b.get(j + 1 + k) == Some(&b'#')) {
                        j += 1 + hashes;
                        break;
                    }
                    j += 1;
                }
                i = j.min(b.len());
                continue;
            }
        }
        // Normal string literal.
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
            i = j.min(b.len());
            continue;
        }
        // Char literal `'x'` / `'\n'` — but not a lifetime `'a`. Only treat as a literal
        // when a closing quote follows within a few bytes.
        if b[i] == b'\'' {
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
                i = j.min(b.len());
                continue;
            }
        }
        // Not a masked region: copy the byte through verbatim.
        out[i] = b[i];
        i += 1;
    }
    String::from_utf8(out).expect("masking preserves UTF-8 boundaries on ASCII delimiters")
}

/// Remove every `#[cfg(test)]`-guarded item from already-masked source. Handles both an
/// inline `mod name { ... }` (brace-matched) and a `mod name;` / `#[path=...] mod name;`
/// declaration. Individual `#[cfg(test)] fn ...` items are left in place; this crate
/// places all test code inside `mod tests`.
fn strip_cfg_test_mods(code: &str) -> String {
    let mut result = code.to_string();
    // Bounded: at most one removal per `#[cfg(test)]` occurrence, and each iteration
    // either removes a block or blanks the attribute, so no occurrence is seen twice.
    while let Some(attr) = result.find("#[cfg(test)]") {
        // Find the next `mod` keyword after the attribute.
        let after = attr + "#[cfg(test)]".len();
        let Some(mod_rel) = result[after..].find("mod ") else {
            // No module follows (e.g. a cfg(test) fn) — blank the attribute and move on.
            result.replace_range(attr..after, &" ".repeat(after - attr));
            continue;
        };
        let mod_start = after + mod_rel;
        // Look for the block-open `{` or the statement-terminating `;`.
        let brace = result[mod_start..].find('{');
        let semi = result[mod_start..].find(';');
        match (brace, semi) {
            (Some(bo), semi_opt) if semi_opt.is_none_or(|s| bo < s) => {
                let open = mod_start + bo;
                if let Some(close) = match_brace(&result, open) {
                    result.replace_range(attr..=close, "");
                } else {
                    result.replace_range(attr..open, "");
                }
            }
            (_, Some(so)) => {
                // `mod name;` declaration — remove the attribute + statement.
                let end = mod_start + so + 1;
                result.replace_range(attr..end, "");
            }
            _ => {
                result.replace_range(attr..after, &" ".repeat(after - attr));
            }
        }
    }
    result
}

/// Is the byte before `i` part of an identifier (so `r` is a suffix, not a raw-string
/// prefix, e.g. `str` / `for`)?
fn prev_is_ident_byte(b: &[u8], i: usize) -> bool {
    i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')
}

/// Index of the `}` matching the `{` at `open` in already-masked text.
fn match_brace(text: &str, open: usize) -> Option<usize> {
    let b = text.as_bytes();
    let mut depth = 0i32;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}
