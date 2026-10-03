//! Write-funnel guard (#227, #160): every artifact `mds` writes from production code must
//! go through the single atomic choke point in `crate::write` — `atomic_write_file`, or,
//! for a rewrite of a file just read, `replace_if_unchanged`, or, for a new file that
//! must never replace one, `create_new`, which share its tail — and every file it
//! removes through `remove_proven`, below the same anchor.
//!
//! # Why this exists
//!
//! `atomic_write_file` is temp-file + sync + rename below the write's anchor: a crash or a
//! mid-write error never leaves a truncated artifact, nothing is written through a
//! symlink below the anchor or at the target, and a replaced file keeps its mode;
//! `remove_proven` removes a file only below its anchor, through no symlink, once it is
//! proven. A raw `std::fs::write` — or a raw `create_dir_all`, `create_dir`, `mkdirat`,
//! `openat`, `rename`, `renameat`, `renameat_with`, `linkat`, `unlinkat`, path-based
//! `fs::set_permissions` or `remove_file` — at any *one* remaining site
//! silently forfeits all of that for the artifact it touches, and "did we remember every
//! write site?" is an unbounded search that three reviewers can each answer differently.
//! This test converts it into a machine-checked invariant: a raw write in
//! `crates/mds-cli/src/**` is a failure unless it appears in [`ALLOWED_RAW_WRITES`] with a
//! written justification.
//!
//! It also pins the tail of the primitive itself ([`primitive_pin_violations`]): the unix
//! arm syncs the temporary file and then its directory, renames with `renameat` and
//! restores a mode with `fchmod`; the Windows arm syncs the temporary file and persists it.
//!
//! # Scope and lexical limits (what this guard does NOT see)
//!
//! The scan is lexical. It matches the needles in [`NEEDLES`] after masking comment and
//! string-literal text and stripping `#[cfg(test)] mod … { … }` blocks (test code
//! legitimately writes fixtures with `std::fs::write`). It therefore does NOT catch:
//!
//! - `OpenOptions::new(…).write(true)` without `.create_new(` — a hand-opened `File` that
//!   may already exist — followed by `write_all`. No such site exists in this crate
//!   today. Needling `OpenOptions::new(` was rejected deliberately: it would fire on
//!   read-only opens too, and an allow-list full of read-only entries is an allow-list
//!   nobody reads.
//! - A write reached through an alias (`use std::fs::write as w;`) or a helper in another
//!   crate.
//! - `#[cfg(test)] fn` items outside a `mod tests` block (the crate has none).
//!
//! The scanner helpers below are copied from `crates/mds-core/tests/yaml_funnel.rs`:
//! integration-test binaries are separate crates and cannot share code across crates.

use std::path::{Path, PathBuf};

/// Raw write entry points that must be funnelled. `std::fs::write(` contains
/// `fs::write(`, so the short form matches both the qualified and imported spellings;
/// `.create_new(` is a file a hand-built `OpenOptions` creates; `create_dir_all(` and
/// `create_dir(` create directories by path, following any symlink in it, and `mkdirat(`
/// one relative to a descriptor; `openat(` is a descriptor-relative open the write
/// primitive alone should make; `fs::set_permissions(` changes a mode by path, which a
/// swapped component redirects (the primitive uses `fchmod` on its own descriptor);
/// `fs::rename(` — `std::fs::rename(` and rustix's alike — moves a file by path, and
/// `renameat(` relative to a descriptor, `renameat_with(` with flags; `fs::linkat(` gives
/// a file a second name relative to a descriptor, and `unlinkat(` removes one;
/// `remove_file(` — `std::fs::remove_file(` and an imported `fs::remove_file(` alike —
/// removes a file by path, through any symlink on the way. `create_dir(`
/// is not part of `create_dir_all(`, nor `fs::rename(` of `fs::renameat(`, nor `renameat(`
/// of `renameat_with(`, nor `fs::linkat(` of `fs::unlinkat(` — which is why the link's
/// needle is the qualified spelling — so each call is counted once.
const NEEDLES: &[&str] = &[
    "fs::write(",
    "File::create(",
    ".create_new(",
    "create_dir_all(",
    "create_dir(",
    "mkdirat(",
    "openat(",
    "fs::set_permissions(",
    "fs::rename(",
    "renameat(",
    "renameat_with(",
    "fs::linkat(",
    "unlinkat(",
    "remove_file(",
];

/// Production sites that may keep a raw write: `(file basename, needle, max hits, why)`.
///
/// `max` is exact, not a ceiling with slack: an entry whose site count drops to zero is
/// reported as dead by [`write_sites_are_funnelled`] and by
/// [`every_allowlist_entry_is_live`], so a removed site cannot leave a stale licence
/// behind for a future raw write to hide under.
const ALLOWED_RAW_WRITES: &[(&str, &str, usize, &str)] = &[
    (
        "watch.rs",
        ".create_new(",
        1,
        "test-only readiness marker: created new at <path>.tmp, never through an entry \
         already there, then renamed — already atomic",
    ),
    (
        "watch.rs",
        "fs::rename(",
        1,
        "test-only readiness marker: <path>.tmp, created new, renamed onto the marker path \
         an absolute environment variable names — the rename is its atomic step",
    ),
    (
        "watch.rs",
        "remove_file(",
        1,
        "test-only readiness marker: an entry already at <path>.tmp — a leftover, or a \
         planted link, the entry itself and never what a link points to — removed before \
         the marker is created new once more; no file mds writes",
    ),
    (
        "write.rs",
        "create_dir_all(",
        2,
        "the primitive creates a missing anchor by path, as the user typed it, in its unix \
         and its Windows arm (#160); nothing below the anchor is created this way",
    ),
    (
        "write.rs",
        "create_dir(",
        1,
        "the primitive's Windows arm creates a missing directory below the anchor by path, \
         once its parent is checked not to be a link — the residual SECURITY.md documents \
         (#160)",
    ),
    (
        "write.rs",
        "mkdirat(",
        1,
        "the primitive's unix walk creates a missing directory below the anchor in the one \
         above it (#160)",
    ),
    (
        "write.rs",
        "openat(",
        7,
        "the primitive's unix walk: the anchor (opened, then again once created), each \
         directory below it without following a symlink (opened, then again once \
         created), the temporary file, created new without following one, the file a \
         rewrite reads again before it replaces it, or a removal's proof reads before it \
         is removed, opened read-only without following one, and the file a new file's \
         commit writes in place on a filesystem without hard links, created new without \
         following one (#160)",
    ),
    (
        "write.rs",
        "renameat(",
        1,
        "the primitive's unix tail: the temporary file renamed over the target in the \
         directory the walk opened (#160)",
    ),
    (
        "write.rs",
        "renameat_with(",
        1,
        "a new file's commit (`mds init` without `--force`): the temporary file renamed \
         onto the target in the directory the walk opened, never over a file there \
         (#160)",
    ),
    (
        "write.rs",
        "fs::linkat(",
        1,
        "a new file's commit where no rename that never replaces is to be had: the \
         temporary file linked to the target in the directory the walk opened, which \
         fails on a file there (#160)",
    ),
    (
        "write.rs",
        "unlinkat(",
        2,
        "the temporary file's guard: a temporary file not renamed over its target — a \
         failed write's, or a linked one's — removed from the directory the walk opened; \
         and `remove_proven`'s unix arm: a file proven, removed from the directory the \
         walk opened while its name is still that file (#160)",
    ),
    (
        "write.rs",
        "remove_file(",
        1,
        "`remove_proven`'s Windows arm: a file proven, removed by path once each directory \
         below the anchor is checked not to be a link — the residual SECURITY.md documents \
         (#160)",
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

        // Lexical pin for the tail of the primitive itself: the funnel is only worth
        // enforcing while what sits at the end of it still syncs and renames.
        if name == "write.rs" {
            let pins = primitive_pin_violations(&code);
            assert!(
                pins.is_empty(),
                "the write primitive's tail moved — the funnel guard is pointless if the \
                 choke point stops being durable or anchored:\n{}",
                pins.join("\n")
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
        "non-vacuity: write.rs was never scanned — the walk did not reach the choke point"
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
         Route the write through `crate::write::atomic_write_file` — `Parents::Create` \
         makes it create the directories an output goes in. If a site genuinely must \
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

    // So is the third: a file a hand-built `OpenOptions` creates new.
    assert_eq!(
        scan_violation_count(
            "fn f(p: &Path) { let _ = OpenOptions::new().write(true).create_new(true).open(p); }"
        ),
        1,
        "a planted create_new open in a plain fn must be flagged"
    );

    // And the anchored write's own entry points (#160): a directory created by path or
    // relative to a descriptor, a descriptor-relative open, a mode changed by path, a
    // rename by path or relative to a descriptor, and a removal relative to a descriptor
    // or by path — each counted once.
    for planted in [
        "fn f(p: &Path) { let _ = std::fs::create_dir_all(p); }",
        "fn f(p: &Path) { let _ = std::fs::create_dir(p); }",
        "fn f(d: BorrowedFd, n: &OsStr) { let _ = rustix::fs::mkdirat(d, n, Mode::from_raw_mode(0o777)); }",
        "fn f(d: BorrowedFd, n: &OsStr) { let _ = rustix::fs::openat(d, n, OFlags::RDONLY, Mode::empty()); }",
        "fn f(p: &Path) { let _ = std::fs::set_permissions(p, Permissions::from_mode(0o600)); }",
        "fn f(a: &Path, b: &Path) { let _ = std::fs::rename(a, b); }",
        "fn f(a: &Path, b: &Path) { let _ = rustix::fs::rename(a, b); }",
        "fn f(d: BorrowedFd, a: &OsStr, b: &OsStr) { let _ = rustix::fs::renameat(d, a, d, b); }",
        "fn f(d: BorrowedFd, a: &OsStr, b: &OsStr) { let _ = fs::renameat_with(d, a, d, b, RenameFlags::NOREPLACE); }",
        "fn f(d: BorrowedFd, a: &OsStr, b: &OsStr) { let _ = rustix::fs::linkat(d, a, d, b, AtFlags::empty()); }",
        "fn f(d: BorrowedFd, a: &OsStr) { let _ = rustix::fs::unlinkat(d, a, AtFlags::empty()); }",
        "fn f(p: &Path) { let _ = std::fs::remove_file(p); }",
        "fn f(p: &Path) { let _ = fs::remove_file(p); }",
    ] {
        assert_eq!(scan_violation_count(planted), 1, "must be flagged: {planted}");
    }
    // A mode set through the file's own descriptor is not a path-based call.
    assert_eq!(
        scan_violation_count(
            "fn f(file: &File, p: Permissions) { let _ = file.set_permissions(p); }"
        ),
        0,
        "File::set_permissions works on the open file"
    );
}

/// The primitive's tail, pinned on synthetic sources: a unix arm that syncs the temporary
/// file and its directory, renames with `renameat` and restores a mode with `fchmod`, and
/// a Windows arm that syncs the temporary file and persists it, pass; each one dropped,
/// alone, fails.
#[test]
fn the_primitive_pins_flag_a_tail_that_stopped_syncing() {
    let unix = |body: &str| {
        format!("mod unix {{ fn w() {{ {body} }} }}\nmod windows {{ fn w() {{ t.as_file().sync_all(); t.persist(p); }} }}")
    };
    let complete =
        unix("fs::fchmod(&f, m); f.sync_all(); fs::renameat(d, t, d, n); dir.sync_all();");
    assert_eq!(primitive_pin_violations(&complete), Vec::<String>::new());

    for (dropped, body) in [
        (
            "the directory sync",
            "fs::fchmod(&f, m); f.sync_all(); fs::renameat(d, t, d, n);",
        ),
        (
            "renameat",
            "fs::fchmod(&f, m); f.sync_all(); std::fs::rename(t, n); dir.sync_all();",
        ),
        (
            "fchmod",
            "f.sync_all(); fs::renameat(d, t, d, n); dir.sync_all();",
        ),
    ] {
        assert_eq!(
            primitive_pin_violations(&unix(body)).len(),
            1,
            "dropping {dropped} must be flagged"
        );
    }
    let windows = complete.replace("t.as_file().sync_all();", "");
    assert_eq!(
        primitive_pin_violations(&windows).len(),
        1,
        "the Windows sync"
    );
    let windows = complete.replace("t.persist(p);", "");
    assert_eq!(
        primitive_pin_violations(&windows).len(),
        1,
        "the Windows persist"
    );
    assert_eq!(
        primitive_pin_violations("fn f() {}").len(),
        2,
        "a primitive with neither arm"
    );
}

/// What the primitive's masked source `code` no longer does at its tail: one line per
/// pin missed. The unix arm (`mod unix`) must call `.sync_all()` exactly twice — the
/// temporary file before the rename and its directory after it, the durable tier's two
/// syncs — rename with `renameat(` and restore a replaced file's mode with `fchmod(`;
/// the Windows arm (`mod windows`) must call `.sync_all()` exactly once and `.persist(`.
fn primitive_pin_violations(code: &str) -> Vec<String> {
    let mut missed = Vec::new();
    match module_block(code, "unix") {
        Some(unix) => {
            let syncs = count_occurrences(unix, ".sync_all()");
            if syncs != 2 {
                missed.push(format!(
                    "  mod unix: {syncs} .sync_all() call(s); the temporary file and its \
                     directory need one each"
                ));
            }
            for (needle, why) in [
                (
                    "renameat(",
                    "the rename relative to the directory it walked to",
                ),
                (
                    "fchmod(",
                    "the mode restored on the temporary file's own descriptor",
                ),
            ] {
                if !unix.contains(needle) {
                    missed.push(format!("  mod unix: no {needle}: {why}"));
                }
            }
        }
        None => missed.push("  no mod unix".to_owned()),
    }
    match module_block(code, "windows") {
        Some(windows) => {
            let syncs = count_occurrences(windows, ".sync_all()");
            if syncs != 1 || !windows.contains(".persist(") {
                missed.push(format!(
                    "  mod windows: {syncs} .sync_all() call(s) and .persist( {}",
                    if windows.contains(".persist(") {
                        "present"
                    } else {
                        "missing"
                    }
                ));
            }
        }
        None => missed.push("  no mod windows".to_owned()),
    }
    missed
}

/// The body of `mod name { … }` in already-masked source, brace-matched.
fn module_block<'a>(code: &'a str, name: &str) -> Option<&'a str> {
    let header = format!("mod {name} {{");
    let start = code.find(&header)?;
    let open = start + header.len() - 1;
    let close = match_brace(code, open)?;
    Some(&code[open..=close])
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
