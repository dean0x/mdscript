//! Funnel guard (#162): every frontmatter YAML parse in `mds-core` production code must
//! go through the single budgeted choke point in `resolver/frontmatter.rs`.
//!
//! A resource limit enforced on only one parse site is silently bypassed by any other
//! `serde_yaml_ng::from_str` / `serde_yaml_ng::Deserializer` call (PF-004 shape). This
//! test converts "did we remember every parse site?" into a machine-checked invariant:
//! the two parse entry points may appear ONLY in `resolver/frontmatter.rs`, anywhere else
//! in `src/**` is a violation.
//!
//! JSON parse entry points can occur only in the data funnel and vars reader.
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
    assert!(
        scanned_needles >= 1,
        "non-vacuity: no needles matched at all"
    );

    assert!(
        violations.is_empty(),
        "YAML parse sites outside the frontmatter choke point ({} found).\n{}\n\n\
         Route the parse through `resolver::frontmatter::parse_frontmatter_yaml` instead, \
         or (for test code) move it into a `*_tests.rs` file or a `#[cfg(test)]` module.",
        violations.len(),
        violations.join("\n")
    );
}

#[test]
fn cfg_test_items_do_not_hide_later_production_parses() {
    let items = [
        "fn test_only() { serde_yaml_ng::from_str(ignored); }",
        "impl TestOnly { fn parse() { serde_yaml_ng::from_str(ignored); } }",
        "const TEST_ONLY: () = { serde_yaml_ng::from_str(ignored); };",
        "use serde_yaml_ng::from_str;",
        "mod test_only { fn parse() { serde_yaml_ng::from_str(ignored); } }",
        "mod test_only;",
    ];
    for item in items {
        let raw = format!(
            "#[cfg(test)]\n#[allow(dead_code)]\n#[tokens({{ ; }})]\n{item}\n\
             fn production() {{ serde_yaml_ng::from_str(input); }}\n\
             #[cfg(test)]\n#[path = \"tests.rs\"]\nmod tests;"
        );
        let code = strip_cfg_test_mods(&mask_comments_and_strings(&raw));
        assert_eq!(
            code.matches(NEEDLES[0]).count(),
            1,
            "the production parse must survive after `{item}`: {code}"
        );
        assert!(
            !code.contains("test_only"),
            "the guarded item must be removed"
        );
        assert!(
            !code.contains("mod tests"),
            "the guarded module must be removed"
        );
    }
}

#[test]
fn builtin_production_parse_inserted_in_memory_is_detected() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/builtins.rs");
    let mut raw = std::fs::read_to_string(path).expect("builtins source must be readable");
    let insert_at = raw.find("fn builtin_upper(").expect("production anchor");
    let test_fn = raw.find("#[cfg(test)]").expect("test function attribute");
    let test_mod = raw.find("mod tests {").expect("test module anchor");
    assert!(test_fn < insert_at && insert_at < test_mod);
    let clean = strip_cfg_test_mods(&mask_comments_and_strings(&raw));
    assert!(
        !clean.contains(NEEDLES[0]),
        "the original source has no raw parse"
    );

    raw.insert_str(
        insert_at,
        "fn planted_parse() { serde_yaml_ng::from_str(input); }\n",
    );
    let code = strip_cfg_test_mods(&mask_comments_and_strings(&raw));
    assert_eq!(
        code.matches(NEEDLES[0]).count(),
        1,
        "the planted parse must be found"
    );
}

#[test]
fn json_gate_detects_each_parse_entry_point_in_a_child_module() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let path = src_dir.join("resolver/import_paths.rs");
    assert!(rust_files(&src_dir).contains(&path));
    let source = std::fs::read_to_string(&path).expect("child module must be readable");
    let matches = |raw: &str| json_parse_violations(&src_dir, &path, raw);
    assert!(matches(&source).is_empty());
    for (plant, needle) in [
        ("serde_json::from_str(input);", "from_str"),
        ("serde_json :: from_slice(input);", "from_slice"),
        ("serde_json::from_reader(input);", "from_reader"),
        ("use serde_json::{Value, from_reader};", "from_reader"),
        ("serde_json::de::from_reader(input);", "from_reader"),
        ("serde_json::Deserializer::from_str(input);", "Deserializer"),
    ] {
        let planted = format!("{source}\nfn planted() {{ {plant} }}\n");
        let violations = matches(&planted);
        assert_eq!(
            violations,
            [needle],
            "the planted parse must be found: {plant}"
        );
    }
}

const JSON_OWNERS: &[&str] = &["resolver/frontmatter.rs", "vars_json.rs"];
const JSON_ENTRY_POINTS: &[&str] = &["from_str", "from_slice", "from_reader", "Deserializer"];

#[test]
fn json_parse_sites_are_funnelled() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = rust_files(&src_dir);
    for owner in JSON_OWNERS {
        let path = src_dir.join(owner);
        assert!(files.contains(&path), "missing parse owner: {owner}");
        let raw = std::fs::read_to_string(path).expect("parse owner must be readable");
        assert!(
            !json_parse_sites(&raw).is_empty(),
            "no parse found in {owner}"
        );
    }
    let mut violations = Vec::new();
    for path in files.iter().filter(|path| {
        !path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with("_tests.rs")
    }) {
        let raw = std::fs::read_to_string(path).expect("source must be readable");
        for name in json_parse_violations(&src_dir, path, &raw) {
            violations.push(format!("{}: serde_json::{name}", rel(&src_dir, path)));
        }
    }
    assert!(
        violations.is_empty(),
        "JSON parse sites outside the data funnel and vars reader:\n{}",
        violations.join("\n")
    );
}

fn json_parse_violations(base: &Path, path: &Path, raw: &str) -> Vec<&'static str> {
    if JSON_OWNERS.iter().any(|owner| path == base.join(owner)) {
        return Vec::new();
    }
    json_parse_sites(raw)
}

fn json_parse_sites(raw: &str) -> Vec<&'static str> {
    let code = strip_cfg_test_mods(&mask_comments_and_strings(raw));
    let mut found = Vec::new();
    for (start, _) in code.match_indices("serde_json") {
        if prev_is_ident_byte(code.as_bytes(), start) {
            continue;
        }
        let tail = code[start + "serde_json".len()..].trim_start();
        let Some(tail) = tail.strip_prefix("::").map(str::trim_start) else {
            continue;
        };
        let tail = tail
            .strip_prefix("de")
            .and_then(|rest| rest.trim_start().strip_prefix("::"))
            .map_or(tail, str::trim_start);
        let separator = |c: char| !(c.is_ascii_alphanumeric() || c == '_');
        let names = if tail.starts_with('{') {
            let end = match_brace(tail, 0).expect("JSON import group must have a closing brace");
            &tail[..end]
        } else {
            &tail[..tail.find(separator).unwrap_or(tail.len())]
        };
        for word in names.split(separator) {
            if let Some(&name) = JSON_ENTRY_POINTS.iter().find(|&&name| name == word) {
                found.push(name);
            }
        }
    }
    found
}

fn is_choke_point(path: &Path) -> bool {
    let comps: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    comps.len() >= CHOKE_POINT.len() && comps[comps.len() - CHOKE_POINT.len()..] == CHOKE_POINT[..]
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
        let rd = std::fs::read_dir(&d).expect("source directory must be readable");
        for entry in rd {
            let entry = entry.expect("source directory entry must be readable");
            let p = entry.path();
            let ft = entry
                .file_type()
                .expect("source file type must be readable");
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

/// Remove each `#[cfg(test)]` attribute and only the item it annotates in masked source.
/// Skip whitespace and other attributes, then remove the item's brace-matched body or
/// semicolon-terminated declaration. A test-only function never marks a later module.
fn strip_cfg_test_mods(code: &str) -> String {
    let mut result = code.to_string();
    // Bounded: each iteration removes a guarded item or blanks its attribute.
    while let Some(attr) = result.find("#[cfg(test)]") {
        let after = attr + "#[cfg(test)]".len();
        let b = result.as_bytes();
        let mut item = after;
        // Bounded: advance over whitespace and complete attributes in this source.
        while item < b.len() {
            if b[item].is_ascii_whitespace() {
                item += 1;
            } else if b[item..].starts_with(b"#[") {
                item += 2;
                let mut depth = 1usize;
                while item < b.len() && depth > 0 {
                    match b[item] {
                        b'[' => depth += 1,
                        b']' => depth -= 1,
                        _ => {}
                    }
                    item += 1;
                }
            } else {
                break;
            }
        }
        let brace = result[item..].find('{');
        let semi = result[item..].find(';');
        let end = match (brace, semi) {
            (Some(bo), semi) if semi.is_none_or(|s| bo < s) => {
                match_brace(&result, item + bo).map(|close| close + 1)
            }
            (_, Some(so)) => Some(item + so + 1),
            _ => None,
        };
        if let Some(end) = end {
            result.replace_range(attr..end, "");
        } else {
            result.replace_range(attr..after, &" ".repeat(after - attr));
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
