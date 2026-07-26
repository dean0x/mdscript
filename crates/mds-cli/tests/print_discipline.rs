//! Print-discipline guard — a CI-enforced invariant over `crates/mds-cli/src/**`
//! (CWE-150 / CWE-117 / PF-004 / #176).
//!
//! # Why this exists
//!
//! Three review rounds of #176 each found a *different* bare `eprintln!` that
//! interpolated an untrusted value onto a terminal: the `*_collecting_warnings` sites,
//! then `lint.rs`'s unknown-`mds.json`-rule warning, then the walker's depth-limit
//! warning inside `output.rs` itself. Each round fixed its findings correctly and each
//! time the next reviewer found another one, because the property was only ever
//! asserted about the sites someone remembered to enumerate. That is the PF-004 failure
//! mode, and no amount of careful reading closes it — the search is unbounded.
//!
//! This test converts the unbounded search into a bounded, machine-checked invariant:
//!
//! > **Every value interpolated by a print macro in `crates/mds-cli/src/**` is passed
//! > through one of the escape helpers, or appears in the explicit allowlist below with
//! > a written justification.**
//!
//! A new `eprintln!("… {}", path.display())` anywhere in the crate fails this test and
//! names the file, line, and offending expression.
//!
//! # Scope — what this guard does and does not cover
//!
//! **Covered:**
//! - `println!` / `eprintln!` / `print!` / `eprint!` in `crates/mds-cli/src/**`, for the
//!   union of their inline captures (`{name}`) and their positional arguments.
//! - `format!` invocations lexically inside an `eprint_warning(…)` call. `eprint_warning`
//!   applies HUMAN-mode escaping, which preserves `\n` by design so multi-line frames
//!   render — so routing a hostile filename through it is **not** sufficient on its own.
//!   That was M2: `lint.rs` already called `eprint_warning`, and an `mds.json` rule name
//!   of `x\nClean: totally-real.mds\n` still forged three standalone status lines. The
//!   governing rule (spec §7.5) is per FIELD: prose HUMAN, interpolated
//!   identifiers/filenames/causes WIRE. This guard enforces the second half.
//!
//! **Not covered, deliberately, and not claimed to be:**
//! - `miette::miette!(…)` report construction. Those reports are rendered by
//!   `eprint_error`, which escapes message, help, and label text in HUMAN mode before
//!   miette sees them — so no raw control byte reaches stderr from that path — but a
//!   `\n` in an interpolated path survives inside the rendered frame. Frame content is
//!   indented and box-drawn rather than emitted as a bare status line, so it is a weaker
//!   surface than the ones above; it is a known residual, not a closed one.
//! - `write!` / `writeln!` into an in-memory `String` or into stdout via
//!   `crate::build::write_stdout`. Compiled template output is the command's *product*
//!   and must stay byte-faithful.
//! - `crates/mds-core/**`. Core does not print except through `emit_warnings`, which
//!   escapes in HUMAN mode; the identifiers its warning producers interpolate
//!   (`resolver.rs`'s module filename, `evaluator.rs`'s `@include` alias) are WIRE-escaped
//!   at construction instead.
//!
//! # The escape helpers are not special-cased
//!
//! `eprint_error` and `eprint_warning` are the two functions that actually write to the
//! stream, and neither gets a blanket exemption. `eprint_error` passes with no allowlist
//! entry at all: its single interpolated argument *is* `render_error_sanitized(report)`.
//! `eprint_warning` passes via one narrow, written-out allowlist entry, because its
//! argument is HUMAN-escaped prose — and HUMAN mode is deliberately *not* in
//! [`SANITIZERS`], since it preserves `\n` and so cannot make an identifier safe.
//!
//! # PF-013 evidence
//!
//! - **Positive:** [`the_guard_flags_a_bare_interpolating_print`] proves the scanner
//!   reports the exact expression from a synthetic violation.
//! - **Negative:** [`cli_print_sites_sanitize_every_interpolated_value`] proves the real
//!   sources are clean.
//! - **Non-vacuity:** the same test asserts the scanner actually found the crate's
//!   modules, its print sites, and its interpolations, so it cannot pass because the
//!   parser silently returned nothing.
//! - **Allowlist rot:** [`every_allowlist_entry_is_live`] fails if an allowlist entry
//!   stops matching anything, so exemptions cannot outlive the code that needed them.

use std::path::{Path, PathBuf};

// ── Configuration ─────────────────────────────────────────────────────────────

/// Macros that write directly to a terminal stream.
const PRINT_MACROS: &[&str] = &["eprintln!", "println!", "eprint!", "print!"];

/// Functions whose return value is escape-safe by construction. An interpolated
/// expression is accepted when it *is* a call to one of these (with any module path
/// prefix, and through any number of leading `&`).
///
/// **WIRE only.** HUMAN-mode `mds::sanitize_control_chars` is deliberately absent: it
/// preserves `\n`, so it does not make an interpolated identifier safe (that was M2).
/// The one place HUMAN mode is correct for a whole line — `eprint_warning`'s own body,
/// which escapes *prose* — is an explicit allowlist entry below, so the exception is
/// visible instead of blanket.
const SANITIZERS: &[&str] = &[
    // crates/mds-cli/src/output.rs — WIRE, for single-line values.
    "safe_path",
    "safe_file_display",
    "safe_inline",
    // crates/mds-core — the WIRE escape entry point.
    "sanitize_control_chars_wire",
    // crates/mds-cli/src/output.rs — renders a report whose inputs were escaped first.
    "render_error_sanitized",
];

/// Values that are printed unescaped **on purpose**, keyed by `(file, expression)`.
///
/// Every entry carries the reason it is safe. An entry without a justification, or one
/// that stops matching (see [`every_allowlist_entry_is_live`]), is the same defect this
/// guard exists to prevent, wearing a different hat.
///
/// Deliberately keyed by *expression*, not by line number, so the list does not rot as
/// code moves — and so an entry cannot silently start covering a different print.
const ALLOWED_UNSANITIZED: &[(&str, &str, &str)] = &[
    // ── The one place HUMAN mode is the correct mode ─────────────────────────
    (
        "output.rs",
        "mds::sanitize_control_chars(w)",
        "`eprint_warning`'s own body. HUMAN mode is correct here and only here: the \
         argument is warning PROSE, which is legitimately multi-line, and escaping its \
         newlines would break multi-line warning bodies. Values interpolated INTO that \
         prose are WIRE-escaped by the caller — which this guard checks separately, by \
         scanning the `format!`s nested inside `eprint_warning` calls.",
    ),
    // ── The compiled artefact itself ─────────────────────────────────────────
    (
        "build.rs",
        "compiled",
        "`print!(\"{compiled}\")` writes the compiled template to STDOUT. This is the \
         command's product, not a diagnostic: `mds build -o - > out.md` must reproduce \
         the artefact byte for byte, so escaping it would corrupt every redirect. \
         Terminal-hazard bytes here originate in the user's own template and are the \
         same bytes `mds build -o file.md` would write to disk.",
    ),
    // ── `&'static str` labels — no runtime data reaches these ────────────────
    (
        "build.rs",
        "kind_label(kind)",
        "Returns one of exactly two `&'static str` literals (`build.rs::kind_label`); \
         it is a compile-time label for an `OutputKind`, not user data.",
    ),
    // ── Integer counters — a `usize`/`u128` cannot carry a control byte ───────
    (
        "build.rs",
        "walk.excluded_by_default",
        "`usize` count of `.mds` files the default-exclusion walker skipped \
         (hidden dirs, node_modules); produced by `collect_mds_files_detailed`.",
    ),
    (
        "build.rs",
        "ok_count",
        "`usize` tally of successful compilations in `mds build <dir>` summary output.",
    ),
    (
        "build.rs",
        "fail_count",
        "`usize` tally of failed compilations in `mds build <dir>` summary output.",
    ),
    (
        "fmt.rs",
        "walk.excluded_by_default",
        "`usize` count of `.mds` files the default-exclusion walker skipped \
         (hidden dirs, node_modules); produced by `collect_mds_files_detailed`.",
    ),
    (
        "fmt.rs",
        "changed_count",
        "`usize` tally of reformatted files in the `mds fmt <dir>` summary line.",
    ),
    (
        "fmt.rs",
        "unchanged_count",
        "`usize` tally of already-formatted files in the `mds fmt <dir>` summary line.",
    ),
    (
        "fmt.rs",
        "fail_count",
        "`usize` tally of files `mds fmt <dir>` could not process, in its summary line.",
    ),
    (
        "lint.rs",
        "walk.excluded_by_default",
        "`usize` count of `.mds` files the default-exclusion walker skipped \
         (hidden dirs, node_modules); produced by `collect_mds_files_detailed`.",
    ),
    (
        "lint.rs",
        "applied_count",
        "`usize` tally of lint fixes actually applied, in the `Partially fixed:` line.",
    ),
    (
        "lint.rs",
        "total_count",
        "`usize` tally of lint fixes planned, in the `Partially fixed:` line.",
    ),
    (
        "lint.rs",
        "mds::MAX_DIAGNOSTICS",
        "`usize` compile-time constant `mds::MAX_DIAGNOSTICS` (the per-file diagnostic cap).",
    ),
    (
        "main.rs",
        "walk.excluded_by_default",
        "`usize` count of `.mds` files the default-exclusion walker skipped \
         (hidden dirs, node_modules); produced by `collect_mds_files_detailed`.",
    ),
    (
        "main.rs",
        "ok_count",
        "`usize` tally of files that passed `mds check <dir>`, in its summary line.",
    ),
    (
        "main.rs",
        "fail_count",
        "`usize` tally of files that failed `mds check <dir>`, in its summary line.",
    ),
    (
        "output.rs",
        "max_depth",
        "`usize` recursion bound; every caller passes the compile-time `MAX_DEPTH` constant.",
    ),
    (
        "watch.rs",
        "dep_count",
        "`usize` count of a compiled template's dependencies, in the `Recompiled` line.",
    ),
    (
        "watch.rs",
        "elapsed",
        "`u128` elapsed milliseconds from `Instant::elapsed().as_millis()` — pure arithmetic.",
    ),
];

// ── The guard ─────────────────────────────────────────────────────────────────

#[test]
fn cli_print_sites_sanitize_every_interpolated_value() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = rust_files(&src_dir);

    // Non-vacuity #1: the crate's modules were actually found and read.
    assert!(
        files.len() >= 6,
        "non-vacuity: expected at least the 6 mds-cli modules under {}, found {}",
        src_dir.display(),
        files.len()
    );

    let mut violations: Vec<String> = Vec::new();
    let mut total_sites = 0usize;
    let mut total_exprs = 0usize;

    for file in &files {
        let name = file_key(file);
        let src = std::fs::read_to_string(file).expect("mds-cli source must be readable");
        for site in collect_sites(&src) {
            total_sites += 1;
            for expr in &site.exprs {
                total_exprs += 1;
                if is_sanitizer_call(expr) {
                    continue;
                }
                if allowlist_justification(&name, expr).is_some() {
                    continue;
                }
                violations.push(format!(
                    "  {}:{}: {} interpolates unsanitized `{}`",
                    name, site.line, site.kind, expr
                ));
            }
        }
    }

    // Non-vacuity #2/#3: the scanner really parsed print sites and their interpolations.
    // Without these, a broken parser would make this test pass by finding nothing.
    assert!(
        total_sites >= 80,
        "non-vacuity: expected at least 80 print sites across mds-cli/src, found {total_sites}"
    );
    assert!(
        total_exprs >= 60,
        "non-vacuity: expected at least 60 interpolated expressions, found {total_exprs}"
    );

    assert!(
        violations.is_empty(),
        "print-discipline violation: {} interpolation(s) reach a terminal stream unescaped.\n\
         \n{}\n\n\
         Fix by wrapping the value in one of {:?} (see crates/mds-cli/src/output.rs), or — \
         if the value genuinely must not be escaped — add it to ALLOWED_UNSANITIZED in \
         this file with a written justification.",
        violations.len(),
        violations.join("\n"),
        SANITIZERS
    );
}

/// An allowlist entry that no longer matches anything is dead weight that quietly widens
/// the exemption surface for whatever gets written next. Fail on it.
#[test]
fn every_allowlist_entry_is_live() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut seen: Vec<(String, String)> = Vec::new();
    for file in rust_files(&src_dir) {
        let name = file_key(&file);
        let src = std::fs::read_to_string(&file).expect("mds-cli source must be readable");
        for site in collect_sites(&src) {
            for expr in site.exprs {
                seen.push((name.clone(), expr));
            }
        }
    }

    let dead: Vec<&str> = ALLOWED_UNSANITIZED
        .iter()
        .filter(|(file, expr, _)| !seen.iter().any(|(f, e)| f == file && e == expr))
        .map(|(_, expr, _)| *expr)
        .collect();

    assert!(
        dead.is_empty(),
        "these ALLOWED_UNSANITIZED entries no longer match any print site and must be \
         deleted: {dead:?}"
    );

    // Every entry must carry a non-trivial justification.
    for (file, expr, why) in ALLOWED_UNSANITIZED {
        assert!(
            why.len() >= 40,
            "allowlist entry {file}/{expr} needs a real justification, got {why:?}"
        );
    }
}

// ── Scanner self-tests (PF-013 positive / negative / robustness) ──────────────

#[test]
fn the_guard_flags_a_bare_interpolating_print() {
    let src = r#"
        fn f(path: &std::path::Path, e: std::io::Error) {
            eprintln!("warning: could not remove {}: {e}", path.display());
        }
    "#;
    let exprs = only_site(src);
    // Positive: BOTH the inline capture and the positional argument are reported.
    assert!(
        exprs.contains(&"e".to_string()),
        "the inline `{{e}}` capture must be reported; got {exprs:?}"
    );
    assert!(
        exprs.contains(&"path.display()".to_string()),
        "the positional `path.display()` argument must be reported; got {exprs:?}"
    );
    for expr in &exprs {
        assert!(
            !is_sanitizer_call(expr),
            "`{expr}` must not be mistaken for a sanitizer call"
        );
    }
}

#[test]
fn the_guard_accepts_sanitized_and_literal_prints() {
    // A sanitized interpolation, through a module path and a leading `&`.
    let exprs = only_site(r#"fn f() { eprintln!("Clean: {}", crate::output::safe_path(p)); }"#);
    assert_eq!(exprs, vec!["crate::output::safe_path(p)".to_string()]);
    assert!(is_sanitizer_call(&exprs[0]));

    // A literal-only print interpolates nothing and is safe by construction.
    assert_eq!(
        only_site(r#"fn f() { eprintln!("Stopped watching."); }"#),
        Vec::<String>::new()
    );

    // `{{` is an escaped brace, not a placeholder.
    assert_eq!(
        only_site(r#"fn f() { println!("use {{x}} to interpolate"); }"#),
        Vec::<String>::new()
    );

    // A sanitizer nested inside another call is NOT accepted — the outer call could
    // undo the escape.
    assert!(!is_sanitizer_call("wrap(safe_path(p))"));
    assert!(!is_sanitizer_call("format!(\"{}\", safe_path(p))"));
    // A method named like a sanitizer on some other receiver is not accepted either.
    assert!(!is_sanitizer_call("thing.safe_path()"));
}

#[test]
fn the_guard_covers_format_inside_eprint_warning() {
    // HUMAN-mode `eprint_warning` does not make an interpolated identifier safe (M2).
    let src = r#"
        fn f(name: &str) {
            eprint_warning(&format!("warning: unknown lint rule '{name}'"));
        }
    "#;
    let sites = collect_sites(src);
    assert_eq!(
        sites.len(),
        1,
        "the format! inside eprint_warning must be a site"
    );
    assert_eq!(sites[0].exprs, vec!["name".to_string()]);
    assert!(sites[0].kind.contains("eprint_warning"));

    // …and it passes once the identifier is WIRE-escaped.
    let fixed = r#"
        fn f(name: &str) {
            eprint_warning(&format!(
                "warning: unknown lint rule '{}'",
                safe_inline(name)
            ));
        }
    "#;
    let sites = collect_sites(fixed);
    assert_eq!(sites[0].exprs, vec!["safe_inline(name)".to_string()]);
    assert!(is_sanitizer_call(&sites[0].exprs[0]));
}

#[test]
fn the_guard_ignores_comments_and_string_literals() {
    // A print macro named inside a comment or a string must not be scanned — otherwise
    // the rustdoc that *documents* this rule would trip it.
    let src = r#"
        /// Never write `eprintln!("{}", path.display())` — use safe_path.
        // eprintln!("{}", dir.display());
        fn f() {
            let s = "eprintln!(\"{}\", nope.display())";
            /* block: eprintln!("{}", also_nope.display()); */
            let _ = s;
        }
    "#;
    assert!(
        collect_sites(src).is_empty(),
        "comments and string literals must not be scanned as code; got {:?}",
        collect_sites(src)
    );
}

#[test]
fn the_guard_rejects_a_dynamic_format_string() {
    // If the first argument is not a string literal we cannot see the placeholders, so
    // the whole invocation is reported rather than skipped. Fail safe, not open.
    let exprs = only_site(r#"fn f() { eprintln!(FMT, y); }"#);
    assert_eq!(exprs, vec!["FMT, y".to_string()]);
    assert!(!is_sanitizer_call(&exprs[0]));
}

// ── Implementation ────────────────────────────────────────────────────────────

/// One print-like invocation and the expressions it interpolates.
#[derive(Debug)]
struct Site {
    line: usize,
    /// `eprintln!`, `print!`, or `eprint_warning(format!)`.
    kind: String,
    exprs: Vec<String>,
}

fn only_site(src: &str) -> Vec<String> {
    let sites = collect_sites(src);
    assert_eq!(
        sites.len(),
        1,
        "expected exactly one print site in the fixture"
    );
    sites.into_iter().next().expect("checked above").exprs
}

fn file_key(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string()
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    // Bounded: the source tree is finite and acyclic (no symlinks are followed because
    // `read_dir` entries are checked with `file_type`, which does not traverse).
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

/// Collect every print-like site in one Rust source file.
fn collect_sites(src: &str) -> Vec<Site> {
    let masked = mask_comments(src);
    let mut sites = Vec::new();

    for inv in find_invocations(&masked, PRINT_MACROS) {
        sites.push(Site {
            line: inv.line,
            kind: inv.name.clone(),
            exprs: interpolated_exprs(&inv.body),
        });
    }

    // `eprint_warning` escapes its argument in HUMAN mode, which preserves `\n`. Any
    // value interpolated into the string it is handed must therefore be WIRE-escaped in
    // its own right — so scan the `format!` invocations inside each call.
    for call in find_invocations(&masked, &["eprint_warning"]) {
        for inner in find_invocations(&call.body, &["format!"]) {
            sites.push(Site {
                line: call.line + inner.line.saturating_sub(1),
                kind: "eprint_warning(format!)".to_string(),
                exprs: interpolated_exprs(&inner.body),
            });
        }
    }

    sites
}

/// Is `expr` a direct call to one of [`SANITIZERS`], possibly module-qualified and
/// possibly behind leading `&`?
fn is_sanitizer_call(expr: &str) -> bool {
    let e = expr.trim_start_matches(['&', ' ']).trim();
    let Some(open) = e.find('(') else {
        return false;
    };
    let callee = e[..open].trim();
    if callee.is_empty() {
        return false;
    }
    // Every path segment must be a plain identifier — this rejects `thing.safe_path`,
    // `wrap(safe_path`, `format!` and friends.
    let segments: Vec<&str> = callee.split("::").collect();
    if !segments.iter().all(|s| is_ident(s)) {
        return false;
    }
    segments
        .last()
        .is_some_and(|last| SANITIZERS.contains(last))
}

fn allowlist_justification(file: &str, expr: &str) -> Option<&'static str> {
    ALLOWED_UNSANITIZED
        .iter()
        .find(|(f, e, _)| *f == file && *e == expr)
        .map(|(_, _, why)| *why)
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

struct Invocation {
    /// 1-based line of the macro/function name within the scanned text.
    line: usize,
    name: String,
    body: String,
}

/// Find every `name(...)` / `name!(...)` invocation, with balanced-paren bodies.
///
/// `text` must already have had its comments masked; string, raw-string, and char
/// literals are skipped so parentheses inside them do not unbalance the scan.
fn find_invocations(text: &str, names: &[&str]) -> Vec<Invocation> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        // Skip literals so a `"("` inside a string is not treated as code.
        if let Some(next) = skip_literal(text, b, i) {
            i = next;
            continue;
        }
        let mut matched: Option<&str> = None;
        for name in names {
            if text[i..].starts_with(name) && !prev_is_ident(b, i) {
                matched = Some(name);
                break;
            }
        }
        let Some(name) = matched else {
            i += 1;
            continue;
        };
        let mut j = i + name.len();
        while j < b.len() && (b[j] as char).is_ascii_whitespace() {
            j += 1;
        }
        if j >= b.len() || b[j] != b'(' {
            i += name.len();
            continue;
        }
        let Some(close) = matching_paren(text, b, j) else {
            i += name.len();
            continue;
        };
        out.push(Invocation {
            line: text[..i].matches('\n').count() + 1,
            name: (*name).to_string(),
            body: text[j + 1..close].to_string(),
        });
        i = close + 1;
    }
    out
}

/// Union of a format invocation's inline captures and its positional arguments.
fn interpolated_exprs(body: &str) -> Vec<String> {
    let trimmed = body.trim_start();
    let Some((fmt_inner, fmt_end)) = parse_string_literal(trimmed) else {
        // Not a literal format string — we cannot see the placeholders, so report the
        // whole invocation rather than assume it is safe.
        let whole = normalize(body);
        return if whole.is_empty() {
            Vec::new()
        } else {
            vec![whole]
        };
    };
    let mut exprs = placeholder_exprs(&fmt_inner);
    let rest = trimmed[fmt_end..].trim_start();
    if let Some(args) = rest.strip_prefix(',') {
        for arg in split_top_level(args) {
            let a = strip_named_arg(arg.trim());
            let a = normalize(a);
            if !a.is_empty() {
                exprs.push(a);
            }
        }
    }
    exprs
}

/// Named captures written inline in the format string (`{e}`, `{max_depth}`).
///
/// Positional `{}` / `{0}` placeholders consume an argument instead and are collected
/// from the argument list. A placeholder whose format spec uses a `$` reference
/// (`{:>width$}`) is reported verbatim so it must be justified rather than silently
/// skipped.
fn placeholder_exprs(fmt: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = fmt.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'{' if i + 1 < bytes.len() && bytes[i + 1] == b'{' => i += 2,
            b'}' if i + 1 < bytes.len() && bytes[i + 1] == b'}' => i += 2,
            b'{' => {
                let Some(rel) = fmt[i..].find('}') else { break };
                let inner = &fmt[i + 1..i + rel];
                let (name, spec) = match inner.find(':') {
                    Some(c) => (&inner[..c], &inner[c + 1..]),
                    None => (inner, ""),
                };
                if spec.contains('$') {
                    out.push(format!("{{{inner}}}"));
                } else if !name.is_empty() && !name.chars().all(|c| c.is_ascii_digit()) {
                    out.push(name.to_string());
                }
                i += rel + 1;
            }
            _ => i += 1,
        }
    }
    out
}

/// `kind_name = expr` → `expr`; anything else is returned unchanged.
fn strip_named_arg(arg: &str) -> &str {
    let Some(eq) = arg.find('=') else { return arg };
    // Not `==`, `!=`, `>=`, `<=`, `+=` …
    if arg.as_bytes().get(eq + 1) == Some(&b'=') {
        return arg;
    }
    if eq > 0
        && matches!(
            arg.as_bytes()[eq - 1],
            b'=' | b'!' | b'<' | b'>' | b'+' | b'-' | b'*' | b'/' | b'%' | b'&' | b'|' | b'^'
        )
    {
        return arg;
    }
    if !is_ident(arg[..eq].trim()) {
        return arg;
    }
    arg[eq + 1..].trim()
}

fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Split on top-level commas, honouring nesting and literals.
fn split_top_level(s: &str) -> Vec<String> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < b.len() {
        if let Some(next) = skip_literal(s, b, i) {
            i = next;
            continue;
        }
        match b[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                out.push(s[start..i].to_string());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(s[start..].to_string());
    out.retain(|a| !a.trim().is_empty());
    out
}

/// Parse a Rust string literal at the start of `s`, returning its inner text and the
/// byte index just past the closing delimiter.
fn parse_string_literal(s: &str) -> Option<(String, usize)> {
    let b = s.as_bytes();
    if b.is_empty() {
        return None;
    }
    if b[0] == b'r' {
        let mut j = 1usize;
        let mut hashes = 0usize;
        while j < b.len() && b[j] == b'#' {
            hashes += 1;
            j += 1;
        }
        if j < b.len() && b[j] == b'"' {
            let close = format!("\"{}", "#".repeat(hashes));
            let rel = s[j + 1..].find(&close)?;
            return Some((s[j + 1..j + 1 + rel].to_string(), j + 1 + rel + close.len()));
        }
        return None;
    }
    if b[0] != b'"' {
        return None;
    }
    let mut i = 1usize;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return Some((s[1..i].to_string(), i + 1)),
            _ => i += 1,
        }
    }
    None
}

/// Replace every comment byte with a space (newlines preserved) so line numbers and
/// byte offsets stay stable while comment text disappears from the scan.
fn mask_comments(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0usize;
    while i < b.len() {
        if let Some(next) = skip_literal(src, b, i) {
            i = next;
            continue;
        }
        if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            while i < b.len() && b[i] != b'\n' {
                out[i] = b' ';
                i += 1;
            }
            continue;
        }
        if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            let mut depth = 0usize;
            while i < b.len() {
                if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
                    depth += 1;
                    out[i] = b' ';
                    out[i + 1] = b' ';
                    i += 2;
                } else if b[i] == b'*' && i + 1 < b.len() && b[i + 1] == b'/' {
                    depth -= 1;
                    out[i] = b' ';
                    out[i + 1] = b' ';
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    if b[i] != b'\n' {
                        out[i] = b' ';
                    }
                    i += 1;
                }
            }
            continue;
        }
        i += 1;
    }
    String::from_utf8(out).expect("masking replaces comment bytes with ASCII spaces")
}

/// If a string / raw-string / char literal starts at `i`, return the index just past it.
fn skip_literal(src: &str, b: &[u8], i: usize) -> Option<usize> {
    // Raw string, optionally byte-prefixed: r"…", r#"…"#, br#"…"#
    let raw_start = if b[i] == b'r' && !prev_is_ident(b, i) {
        Some(i)
    } else if b[i] == b'b' && !prev_is_ident(b, i) && b.get(i + 1) == Some(&b'r') {
        Some(i + 1)
    } else {
        None
    };
    if let Some(r) = raw_start {
        let mut j = r + 1;
        let mut hashes = 0usize;
        while j < b.len() && b[j] == b'#' {
            hashes += 1;
            j += 1;
        }
        if j < b.len() && b[j] == b'"' {
            let close = format!("\"{}", "#".repeat(hashes));
            return Some(match src[j + 1..].find(&close) {
                Some(rel) => j + 1 + rel + close.len(),
                None => b.len(),
            });
        }
    }
    if b[i] == b'"' {
        let mut j = i + 1;
        while j < b.len() {
            match b[j] {
                b'\\' => j += 2,
                b'"' => return Some(j + 1),
                _ => j += 1,
            }
        }
        return Some(b.len());
    }
    if b[i] == b'\'' {
        // Escaped char literal: '\n', '\u{1b}', '\''
        if b.get(i + 1) == Some(&b'\\') {
            let mut j = i + 2;
            while j < b.len() && b[j] != b'\'' {
                j += 1;
            }
            return Some((j + 1).min(b.len()));
        }
        // Plain char literal: 'x' (any codepoint width). Otherwise it is a lifetime or
        // a loop label, which carries no literal text to skip.
        if let Some(ch) = src[i + 1..].chars().next() {
            let after = i + 1 + ch.len_utf8();
            if b.get(after) == Some(&b'\'') {
                return Some(after + 1);
            }
        }
        return None;
    }
    None
}

fn prev_is_ident(b: &[u8], i: usize) -> bool {
    i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')
}

/// Index of the `)` matching the `(` at `open`.
fn matching_paren(src: &str, b: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = open;
    while i < b.len() {
        if let Some(next) = skip_literal(src, b, i) {
            i = next;
            continue;
        }
        match b[i] {
            b'(' => depth += 1,
            b')' => {
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
