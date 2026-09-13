//! Funnel guard (#162): every frontmatter YAML parse in `mds-core` production code must
//! go through the single budgeted choke point in `resolver/frontmatter.rs`.
//!
//! A resource limit enforced on only one parse site is silently bypassed by any other
//! `serde_yaml_ng::from_str` / `serde_yaml_ng::Deserializer` call (PF-004 shape). This
//! test converts "did we remember every parse site?" into a machine-checked invariant:
//! the two parse entry points may appear ONLY in `resolver/frontmatter.rs`, anywhere else
//! in `src/**` is a violation.
//!
//! Scope: `crates/mds-core/src/**`, excluding `*_tests.rs` files and `#[cfg(test)]`
//! modules (test code legitimately calls the raw parser to assert parity). Comment and
//! string-literal text is masked so a needle named in rustdoc or a string does not count.

use std::path::{Path, PathBuf};

/// The two `serde_yaml_ng` parse entry points that must be funnelled.
const NEEDLES: &[&str] = &["serde_yaml_ng::from_str", "serde_yaml_ng::Deserializer"];

/// The one file allowed to contain them (path suffix, OS-agnostic).
const CHOKE_POINT: &[&str] = &["resolver", "frontmatter.rs"];

#[test]
fn yaml_parse_sites_are_funnelled() {
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
    let mut scanned_needles = 0usize;
    let mut choke_point_seen = false;

    for file in &files {
        if file
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with("_tests.rs"))
        {
            continue;
        }
        let is_choke = is_choke_point(file);
        let raw = std::fs::read_to_string(file).expect("source must be readable");
        let code = strip_cfg_test_mods(&mask_comments_and_strings(&raw));

        for needle in NEEDLES {
            if code.contains(needle) {
                scanned_needles += 1;
                if is_choke {
                    choke_point_seen = true;
                } else {
                    violations.push(format!("  {}: contains `{needle}`", rel(&src_dir, file)));
                }
            }
        }
    }

    // Non-vacuity: the scanner really found the needles at the choke point — otherwise a
    // needle-blind bug would make this test pass by finding nothing anywhere.
    assert!(
        choke_point_seen,
        "non-vacuity: expected `resolver/frontmatter.rs` to contain a YAML parse entry \
         point, found none — the funnel guard is not actually matching anything"
    );
    assert!(scanned_needles >= 1, "non-vacuity: no needles matched at all");

    assert!(
        violations.is_empty(),
        "YAML parse sites outside the frontmatter choke point ({} found).\n{}\n\n\
         Route the parse through `resolver::frontmatter::parse_frontmatter_yaml` instead, \
         or (for test code) move it into a `*_tests.rs` file or a `#[cfg(test)]` module.",
        violations.len(),
        violations.join("\n")
    );
}

fn is_choke_point(path: &Path) -> bool {
    let comps: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    comps.len() >= CHOKE_POINT.len()
        && comps[comps.len() - CHOKE_POINT.len()..] == CHOKE_POINT[..]
}

fn rel(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
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
/// declaration (whose body is an external `*_tests.rs`, excluded separately). Individual
/// `#[cfg(test)] fn ...` items are left in place; the codebase places test parse calls
/// inside `mod tests` or `*_tests.rs`, both of which are handled.
fn strip_cfg_test_mods(code: &str) -> String {
    let mut result = code.to_string();
    // Bounded: at most one removal per `#[cfg(test)]` occurrence, and the string only
    // shrinks, so the loop terminates.
    loop {
        let Some(attr) = result.find("#[cfg(test)]") else {
            break;
        };
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
            (Some(bo), semi_opt) if semi_opt.map_or(true, |s| bo < s) => {
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
