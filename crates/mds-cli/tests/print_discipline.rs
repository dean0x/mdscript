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
//! > **Every value that reaches a terminal stream from `crates/mds-cli/src/**` — whether
//! > interpolated by a print macro or carried into a sanitizing print helper — is passed
//! > through one of the escape helpers, or appears in an explicit allowlist below with a
//! > written justification.**
//!
//! A new `eprintln!("… {}", path.display())` anywhere in the crate fails this test and
//! names the file, line, and offending expression. So does the same interpolation hoisted
//! into a local and handed to `eprint_warning`, and so does a value whose provenance the
//! scanner cannot establish at all.
//!
//! # Scope — what this guard does and does not cover
//!
//! **Covered:**
//! - `println!` / `eprintln!` / `print!` / `eprint!` in `crates/mds-cli/src/**`, for the
//!   union of their inline captures (`{name}`) and their positional arguments.
//! - `write!` / `writeln!` whose first argument names a terminal stream — `std::io::stderr()`,
//!   `io::stdout()`, or a local whose `let` initialiser names one. The crate contains no
//!   such call today; the rule is here so that the first one cannot arrive unnoticed.
//! - **The argument of every [`SANITIZING_PRINT_HELPERS`] call** (`eprint_warning`).
//!   `eprint_warning` applies HUMAN-mode escaping, which preserves `\n` by design so
//!   multi-line frames render — so routing a hostile filename through it is **not**
//!   sufficient on its own. That was M2: `lint.rs` already called `eprint_warning`, and an
//!   `mds.json` rule name of `x\nClean: totally-real.mds\n` still forged three standalone
//!   status lines. The governing rule (spec §7.5) is per FIELD: prose HUMAN, interpolated
//!   identifiers/filenames/causes WIRE. This guard enforces the second half.
//!
//!   The argument is accepted only in one of three shapes: a string literal, a
//!   whole-expression sanitizer call, or a `format!` whose every interpolation is itself
//!   accepted. A **bare local** is traced one hop through its `let` binding in the same
//!   file and judged by the same rule — so hoisting the message out of the call
//!   (`let msg = format!("… {name}"); eprint_warning(&msg);`) is checked exactly as if it
//!   had been written inline. An expression shape not listed above, and a name with no
//!   visible `let`, are **reported**, not assumed safe.
//!
//!   Because `let` bindings are matched by name file-wide, a name that is *also*
//!   introduced by a non-`let` binder would otherwise be judged by whatever unrelated
//!   `let` of that name happens to exist elsewhere in the file. [`collect_non_let_binders`]
//!   closes that: every `for`-loop variable, function parameter and closure parameter in
//!   the file **poisons** its name, so such an argument is reported rather than resolved.
//!   [`the_guard_refuses_to_resolve_a_non_let_binder`] is the proof. Pattern binders the
//!   collector does not model — `if let` / `while let` / `match`-arm bindings — are
//!   limit 5 under "Accepted limits".
//!
//!   The guard fails closed: a false positive costs one allowlist entry with
//!   a written justification, a false negative costs another review round.
//!
//! **Not covered, deliberately, and not claimed to be:**
//! - `miette::miette!(…)` report construction, and `MdsError` message bodies built in
//!   `crates/mds-core/**`. Both are rendered by `eprint_error`, which escapes message,
//!   help, and label text in HUMAN mode before miette sees them — so no raw control byte
//!   reaches stderr from either path — but a `\n` in an interpolated path or identifier
//!   survives inside the rendered frame. Frame content is indented and `│`-prefixed rather
//!   than emitted as a bare status line, so it is a weaker surface than the ones above; it
//!   is a known residual, not a closed one. Both halves of that residual are disclosed in
//!   spec §7.5 and in the boundary table in `crates/mds-core/src/lint/diagnostic.rs`.
//! - `write!` / `writeln!` into an in-memory `String`, and stdout writes via
//!   `crate::build::write_stdout`. Compiled template output is the command's *product*
//!   and must stay byte-faithful.
//! - `crates/mds-core/**` warning *producers*. Core does not print except through
//!   `emit_warnings`, which escapes in HUMAN mode; the identifiers its warning producers
//!   interpolate are WIRE-escaped at construction instead. This guard is lexical and
//!   cannot follow a value across a crate boundary, so that is a **precondition it
//!   depends on and does not check**; [`ALLOWED_UNTRACED_HELPER_ARGS`] is where the
//!   dependency is written down.
//!
//!   `mds-core` has exactly three warning producers that interpolate a runtime value.
//!   Their status differs and is worth stating exactly, because "upheld by tests" was
//!   claimed here once when it was not true:
//!   - `resolver.rs`'s imported-module filename (the source-map segment-cap warning) —
//!     the only one whose input can actually carry a hostile character, since a module
//!     key is a filesystem path. **Pinned by a test**:
//!     `crates/mds-cli/tests/producer_discipline.rs`, which compiles a module named with
//!     a real ESC byte and asserts the warning that reaches this crate is WIRE-escaped.
//!   - `evaluator.rs`'s two `@include` alias warnings — **upheld by review only, and not
//!     testable today.** The parser admits an `@include` alias only if it matches
//!     `[A-Za-z_][A-Za-z0-9_]*` (`parser.rs`'s `is_valid_identifier` check), so no
//!     hostile character can reach either site; the WIRE call there is defence in depth
//!     against a future parser relaxation. A behavioural test of it would assert on an
//!     input the parser rejects, i.e. it would be vacuous — the PF-013 failure mode — so
//!     none is written.
//!
//! # Accepted limits
//!
//! This is a lexical scanner over Rust text, not a compiler. It is defeated by anyone
//! who sets out to defeat it, and stating the limits plainly is worth more than
//! implying they are closed:
//!
//! 1. **Sanitizers are matched by the last path segment of the callee.** `use
//!    evil::passthrough as safe_path;`, or a locally-defined `fn safe_path` that returns
//!    its input, both satisfy the check while escaping nothing.
//! 2. **Allowlist entries are anti-rot, not anti-reuse.** They are keyed by `(file,
//!    expression)` with no macro or stream constraint, so [`every_allowlist_entry_is_live`]
//!    catches an entry that stops matching, but a *new* variable that reuses an exempted
//!    name in the same file (`compiled`, `ok_count`, `max_depth`) inherits the exemption
//!    silently. The names were chosen to be specific for that reason.
//! 3. **The binding trace is one hop, within one file.** A local initialised from another
//!    local is not followed; it is reported instead. Because bindings are matched by name
//!    across the whole file rather than within the enclosing function, a name bound more
//!    than once is accepted only if *every* binding of it is accepted.
//! 4. **Stream detection for `write!` is by name.** `let out = std::io::stderr()` is
//!    followed, but a handle whose name and initialiser both avoid the words `stdout` and
//!    `stderr` (passed in as a parameter, say) is not recognised as a terminal.
//! 5. **Only three non-`let` binder shapes poison a name.** [`collect_non_let_binders`]
//!    models `for` variables, function parameters and closure parameters — the shapes a
//!    hostile value plausibly arrives in. It does **not** model `if let` / `while let` /
//!    `match`-arm bindings, so a name introduced by one of those and passed bare to
//!    `eprint_warning` is still resolved against the file's `let`s. This is the narrowed
//!    remnant of a wider hole: before limit 5 existed, *every* non-`let` binder was
//!    resolved that way, and `for label in rules { eprint_warning(label) }` in `lint.rs`
//!    passed the guard because the file's three unrelated `let label = safe_path(…)`
//!    bindings were all safe.
//!
//! Every one of these requires writing code that looks wrong on purpose. The bar this
//! guard is built to meet is **accidental** reintroduction — the four times #176 was
//! reopened, it was an ordinary `eprintln!` or an ordinary hoisted `format!`, never an
//! alias. Closing the lexical gaps beyond that bar would need a rustc lint or a
//! `syn`-based analysis over expanded HIR, which is a different tool.
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
//!   reports the exact expression from a synthetic violation;
//!   [`the_guard_follows_a_hoisted_format_binding`] proves the same for a message hoisted
//!   into a local, [`the_guard_reports_an_untraceable_helper_argument`] for one it
//!   cannot resolve at all, and [`the_guard_refuses_to_resolve_a_non_let_binder`] for one
//!   whose name is shadowed by a `for` / parameter / closure binder.
//! - **Negative:** [`cli_print_sites_sanitize_every_interpolated_value`] proves the real
//!   sources are clean.
//! - **Non-vacuity:** the same test asserts the scanner actually found the crate's
//!   modules, its print sites, its interpolations, its `let` bindings, the non-`let`
//!   binders that poison a name, and its calls into the sanitizing print helpers, so it
//!   cannot pass because the parser silently returned nothing.
//! - **Allowlist rot:** [`every_allowlist_entry_is_live`] fails if an entry in either
//!   allowlist stops matching anything, so exemptions cannot outlive the code that
//!   needed them.

use std::path::{Path, PathBuf};

// ── Configuration ─────────────────────────────────────────────────────────────

/// Macros that write directly to a terminal stream.
const PRINT_MACROS: &[&str] = &["eprintln!", "println!", "eprint!", "print!"];

/// Macros that write to whatever sink they are handed. Scanned only when that sink is a
/// terminal stream (see `is_stream_target`) — a `write!` into an in-memory `String` is
/// not a print, and compiled output written to stdout is the command's product.
const STREAM_WRITE_MACROS: &[&str] = &["writeln!", "write!"];

/// Functions that escape their whole argument in HUMAN mode and write it to a stream as
/// one warning.
///
/// HUMAN mode preserves `\n`, so the helper makes *prose* safe and does nothing for an
/// identifier interpolated into that prose. Every argument handed to one of these is
/// therefore classified in its own right (`classify_helper_arg`), including through one
/// hop of `let`-binding — see the module doc.
const SANITIZING_PRINT_HELPERS: &[&str] = &["eprint_warning"];

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
        "STDIN_DISPLAY_LABEL",
        "`&'static str` compile-time constant defined in `output.rs` as `\"<stdin>\"`. \
         It is the uniform stdin source-identity sentinel (AD-211-3 / issue #211); \
         it contains only ASCII printable characters and cannot carry hostile bytes.",
    ),
    (
        "fmt.rs",
        "STDIN_DISPLAY_LABEL",
        "Same `output.rs` constant as the `lint.rs` entry above — `mds fmt -`'s \
         `Would reformat:` status line names the source with the shared sentinel \
         instead of its own literal (AD-211-3).",
    ),
    (
        "main.rs",
        "STDIN_DISPLAY_LABEL",
        "Same `output.rs` constant as the `lint.rs` entry above — `mds check -`'s \
         `OK:` status line names the source with the shared sentinel instead of its \
         own literal (AD-211-3).",
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

/// Arguments to a [`SANITIZING_PRINT_HELPERS`] call that the one-hop binding trace cannot
/// resolve, and that are accepted anyway, keyed by `(file, expression)`.
///
/// Kept separate from [`ALLOWED_UNSANITIZED`] on purpose. An entry here exempts a value
/// **only** in the argument position of a sanitizing print helper; the same name appearing
/// in an `eprintln!` in the same file is still a violation. That is a narrower exemption
/// than the general allowlist grants, which matters because the names in this position are
/// short loop variables.
///
/// Every entry here is a dependency on discipline the guard cannot check. Say so.
const ALLOWED_UNTRACED_HELPER_ARGS: &[(&str, &str, &str)] = &[
    (
        "build.rs",
        "w",
        "`for w in &result.warnings` — `w` is a whole warning string produced by \
         `mds-core`, not a value this crate interpolates. HUMAN mode is the correct mode \
         for it: it is prose, legitimately multi-line. What makes it safe is that \
         `mds-core`'s three untrusted-value warning producers WIRE-escape at construction: \
         `resolver.rs`'s imported-module filename, and `evaluator.rs`'s two `@include` \
         alias warnings — see the boundary table in \
         `crates/mds-core/src/lint/diagnostic.rs`. That is PRODUCER DISCIPLINE, which this \
         lexical guard cannot follow across a crate boundary to confirm. It is upheld by \
         review, plus one test on the only producer whose input can carry a hostile \
         character: `producer_discipline.rs` in this crate. The two alias sites are upheld \
         by review alone — the parser restricts an alias to `[A-Za-z_][A-Za-z0-9_]*`, so \
         testing them would be vacuous (PF-013). See this file's module doc.",
    ),
    (
        "main.rs",
        "w",
        "`for w in &warnings` on the `mds check` file, stdin and directory paths — same \
         value and same reasoning as the `build.rs` entry above: a whole `mds-core` \
         warning string, prose, HUMAN by design, safe because mds-core WIRE-escapes the \
         identifiers it interpolates at construction rather than because this lexical \
         guard checks it. Two entries — this one and `build.rs`'s — cover all five live \
         bare-`w` sites, because the list is keyed by (file, expression).",
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
    let mut total_bindings = 0usize;
    let mut total_non_let = 0usize;
    let mut total_helper_calls = 0usize;

    for file in &files {
        let name = file_key(file);
        let src = std::fs::read_to_string(file).expect("mds-cli source must be readable");
        let masked = mask_comments(&src);
        total_bindings += collect_let_bindings(&masked).len();
        total_non_let += collect_non_let_binders(&masked).len();
        total_helper_calls += find_invocations(&masked, SANITIZING_PRINT_HELPERS).len();
        for site in collect_sites(&src) {
            total_sites += 1;
            for expr in &site.exprs {
                total_exprs += 1;
                if is_sanitizer_call(expr) {
                    continue;
                }
                if justification(&name, expr, &site.kind).is_some() {
                    continue;
                }
                violations.push(format!(
                    "  {}:{}: {} interpolates unsanitized `{}`",
                    name, site.line, site.kind, expr
                ));
            }
        }
    }

    // Non-vacuity #2–#5: the scanner really parsed print sites, their interpolations, the
    // `let` bindings the trace depends on, and the helper calls it classifies. Without
    // these, a broken parser would make this test pass by finding nothing.
    assert!(
        total_sites >= 80,
        "non-vacuity: expected at least 80 print sites across mds-cli/src, found {total_sites}"
    );
    assert!(
        total_exprs >= 60,
        "non-vacuity: expected at least 60 interpolated expressions, found {total_exprs}"
    );
    assert!(
        total_bindings >= 100,
        "non-vacuity: the binding trace is only as good as the bindings it finds; \
         expected at least 100 `let` bindings across mds-cli/src, found {total_bindings}"
    );
    assert!(
        total_non_let >= 50,
        "non-vacuity: the poison set is only as good as the binders it finds; expected at \
         least 50 non-`let` binders (for-loop vars, fn params, closure params) across \
         mds-cli/src, found {total_non_let}"
    );
    assert!(
        total_helper_calls >= 10,
        "non-vacuity: expected at least 10 calls into {SANITIZING_PRINT_HELPERS:?}, \
         found {total_helper_calls}"
    );

    assert!(
        violations.is_empty(),
        "print-discipline violation: {} interpolation(s) reach a terminal stream unescaped.\n\
         \n{}\n\n\
         Fix by wrapping the value in one of {:?} (see crates/mds-cli/src/output.rs), or — \
         if the value genuinely must not be escaped — add it to ALLOWED_UNSANITIZED (or, \
         for an argument the helper trace cannot resolve, ALLOWED_UNTRACED_HELPER_ARGS) in \
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
    // `(file, expr, is_untraced_helper_arg)` for every interpolation the scanner saw.
    let mut seen: Vec<(String, String, bool)> = Vec::new();
    for file in rust_files(&src_dir) {
        let name = file_key(&file);
        let src = std::fs::read_to_string(&file).expect("mds-cli source must be readable");
        for site in collect_sites(&src) {
            let untraced = site.kind.ends_with("(untraced)");
            for expr in site.exprs {
                seen.push((name.clone(), expr, untraced));
            }
        }
    }

    for (list_name, list, want_untraced) in [
        ("ALLOWED_UNSANITIZED", ALLOWED_UNSANITIZED, false),
        (
            "ALLOWED_UNTRACED_HELPER_ARGS",
            ALLOWED_UNTRACED_HELPER_ARGS,
            true,
        ),
    ] {
        let dead: Vec<&str> = list
            .iter()
            .filter(|(file, expr, _)| {
                !seen
                    .iter()
                    .any(|(f, e, u)| f == file && e == expr && *u == want_untraced)
            })
            .map(|(_, expr, _)| *expr)
            .collect();

        assert!(
            dead.is_empty(),
            "these {list_name} entries no longer match any site in the position they \
             exempt, and must be deleted: {dead:?}"
        );

        // Every entry must carry a non-trivial justification.
        for (file, expr, why) in list {
            assert!(
                why.len() >= 40,
                "{list_name} entry {file}/{expr} needs a real justification, got {why:?}"
            );
        }
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

    // …and neither is anything that CONTINUES after the sanitizer call. The escape is
    // only worth something if it is the last thing that happens to the value, so the
    // suffix direction must be rejected exactly like the prefix direction above.
    assert!(
        !is_sanitizer_call("safe_path(p) + &evil"),
        "concatenating onto a sanitized value must not be accepted"
    );
    assert!(
        !is_sanitizer_call("safe_path(p).replace(\"a\", &evil)"),
        "a postfix method on a sanitized value must not be accepted"
    );
    assert!(
        !is_sanitizer_call("safe_path(p).to_string() + evil"),
        "a postfix method plus concatenation must not be accepted"
    );
    // A trailing `?`, `.as_str()` or index is the same hazard shape.
    assert!(!is_sanitizer_call("safe_inline(x)[1..]"));
    // The bare call, with and without a leading `&`, is still accepted — the tightened
    // check must not have closed the legitimate form.
    assert!(is_sanitizer_call("safe_path(p)"));
    assert!(is_sanitizer_call("&crate::output::safe_inline(&e)"));
    // A `)` inside a string argument must not be mistaken for the closing paren.
    assert!(is_sanitizer_call("safe_inline(\"a)b\")"));
}

#[test]
fn the_guard_follows_a_hoisted_format_binding() {
    // B1: hoisting the message into a local is completely idiomatic, and it used to make
    // the whole interpolation invisible — `collect_sites` only looked for `format!`
    // lexically INSIDE the `eprint_warning(…)` parens. This is M2 reintroduced verbatim.
    let src = r#"
        fn f(name: &str) {
            let msg = format!("warning: unknown lint rule '{name}'; ignoring");
            eprint_warning(&msg);
        }
    "#;
    let sites = collect_sites(src);
    assert_eq!(sites.len(), 1, "the hoisted format! must still be a site");
    assert_eq!(sites[0].exprs, vec!["name".to_string()]);
    assert!(
        sites[0].kind.contains("let msg"),
        "the report must name the binding it traced; got {:?}",
        sites[0].kind
    );

    // …and it passes once the identifier is WIRE-escaped, exactly as the inline form does.
    let fixed = r#"
        fn f(name: &str) {
            let msg = format!("warning: unknown lint rule '{}'", safe_inline(name));
            eprint_warning(&msg);
        }
    "#;
    let sites = collect_sites(fixed);
    assert_eq!(sites[0].exprs, vec!["safe_inline(name)".to_string()]);
    assert!(is_sanitizer_call(&sites[0].exprs[0]));

    // A binding that is itself a whole sanitizer call needs no further checking.
    assert!(
        collect_sites(r#"fn f(p: &Path) { let m = safe_path(p); eprint_warning(&m); }"#).is_empty(),
        "a binding that IS a sanitizer call must be accepted outright"
    );
}

#[test]
fn the_guard_reports_an_untraceable_helper_argument() {
    // B2: `eprint_warning(<bare identifier>)` used to produce zero sites, so the argument
    // was trusted without anything checking it. It must fail closed instead.
    let src = r#"
        fn f(warnings: &[String]) {
            for w in warnings {
                eprint_warning(w);
            }
        }
    "#;
    let sites = collect_sites(src);
    assert_eq!(sites.len(), 1, "the loop variable must be reported");
    assert_eq!(sites[0].exprs, vec!["w".to_string()]);
    assert!(
        sites[0].kind.ends_with("(untraced)"),
        "an unresolved argument must be reported as untraced so it is judged against \
         ALLOWED_UNTRACED_HELPER_ARGS, not the general allowlist; got {:?}",
        sites[0].kind
    );

    // A binding the trace CAN reach but does not recognise is reported too — the trace
    // never falls back to trusting the value.
    let opaque = r#"
        fn f(name: &str) {
            let msg = mk_msg(name);
            eprint_warning(&msg);
        }
    "#;
    let sites = collect_sites(opaque);
    assert_eq!(sites.len(), 1);
    assert!(sites[0].kind.ends_with("(untraced)"));

    // Two bindings of one name, only one of them safe: the unsafe one poisons the trace.
    let mixed = r#"
        fn a(p: &Path) { let m = safe_path(p); eprint_warning(&m); }
        fn b(p: &Path) { let m = mk_msg(p); }
    "#;
    let sites = collect_sites(mixed);
    assert_eq!(sites.len(), 1);
    assert!(
        sites[0].kind.ends_with("(untraced)"),
        "a name bound unsafely anywhere in the file must not be accepted; got {:?}",
        sites[0].kind
    );

    // A literal argument is safe by construction and produces no site at all.
    assert!(collect_sites(r#"fn f() { eprint_warning("done."); }"#).is_empty());

    // The helper's own DEFINITION is not one of its call sites.
    assert!(
        collect_sites(r#"pub(crate) fn eprint_warning(w: &str) { let _ = w; }"#).is_empty(),
        "`fn eprint_warning(w: &str)` is a definition, not a call"
    );
}

#[test]
fn the_guard_refuses_to_resolve_a_non_let_binder() {
    // The bypass this closes: `let` bindings are matched file-wide, so a name introduced
    // by a `for` variable / parameter / closure param used to be judged by whatever
    // unrelated `let`s of that name the file contained — and accepted if all of them were
    // safe. This is the exact construct, against the exact shape `lint.rs` carries
    // (`let label = safe_path(…)`, three times). Before `collect_non_let_binders` it
    // produced ZERO sites.
    let for_var = r#"
        fn render(p: &Path, source: &str, fixed: &str) -> String {
            let label = safe_path(p);
            render_unified_diff(source, fixed, &label)
        }
        fn atk_v12(rules: &[String]) {
            for label in rules {
                eprint_warning(label);
            }
        }
    "#;
    let sites = collect_sites(for_var);
    assert_eq!(
        sites.len(),
        1,
        "the `for label in rules` binder must be reported even though every `let label` \
         in the file is safe; got {sites:?}"
    );
    assert_eq!(sites[0].exprs, vec!["label".to_string()]);
    assert!(
        sites[0].kind.ends_with("(untraced)"),
        "it must land in the untraced position so ALLOWED_UNTRACED_HELPER_ARGS is what \
         exempts it, not the general allowlist; got {:?}",
        sites[0].kind
    );

    // Same hole through a function parameter and through a closure parameter.
    for src in [
        r#"
            fn render(p: &Path) -> String { let note = safe_path(p); wrap(note) }
            fn atk(note: &str) { eprint_warning(note); }
        "#,
        r#"
            fn render(p: &Path) -> String { let note = safe_path(p); wrap(note) }
            fn atk(v: &[String]) { v.iter().for_each(|note| eprint_warning(note)); }
        "#,
    ] {
        let sites = collect_sites(src);
        assert_eq!(
            sites.len(),
            1,
            "a parameter / closure param must not be resolved through an unrelated \
             `let` of the same name; got {sites:?}"
        );
        assert!(sites[0].kind.ends_with("(untraced)"));
    }

    // The collector must find each shape it claims to model.
    let binders = collect_non_let_binders(
        "fn f(alpha: &str, beta: usize) { for gamma in xs { xs.map(|delta| delta); } }",
    );
    for want in ["alpha", "beta", "gamma", "delta"] {
        assert!(
            binders.iter().any(|b| b == want),
            "`{want}` must be collected as a non-`let` binder; got {binders:?}"
        );
    }

    // …and must not read a bitwise / logical `|` as a closure, which would poison the
    // names of arbitrary operands and turn the guard into noise.
    let bitwise = collect_non_let_binders("fn f() { let m = flag_a | flag_b; let n = x || y; }");
    assert!(
        !bitwise
            .iter()
            .any(|b| b == "flag_a" || b == "flag_b" || b == "x" || b == "y"),
        "an operand of `|` / `||` is not a closure parameter; got {bitwise:?}"
    );

    // A name that is ONLY `let`-bound is still resolved — the fix must not have made the
    // trace useless.
    assert!(
        collect_sites(
            r#"fn f(p: &Path) { let only_let = safe_path(p); eprint_warning(&only_let); }"#
        )
        .is_empty(),
        "a purely `let`-bound safe local must still be accepted"
    );
}

#[test]
fn the_guard_scans_writes_to_a_stream_but_not_to_a_buffer() {
    // B4: `writeln!(std::io::stderr(), …)` reaches a terminal exactly like `eprintln!`.
    // There are none in the crate today; this pins the rule before the first one lands.
    let sites = collect_sites(
        r#"fn f(p: &Path) { writeln!(std::io::stderr(), "warning: {}", p.display()); }"#,
    );
    assert_eq!(sites.len(), 1, "a write to stderr must be a print site");
    assert_eq!(sites[0].exprs, vec!["p.display()".to_string()]);

    // A handle bound to a local is followed through its `let`.
    let via_local = r#"
        fn f(p: &Path) {
            let out = std::io::stdout();
            writeln!(out, "Clean: {}", p.display());
        }
    "#;
    assert_eq!(
        collect_sites(via_local)[0].exprs,
        vec!["p.display()".to_string()]
    );

    // A write into an in-memory buffer is NOT a print — compiled output and assembled
    // strings must stay byte-faithful, and scanning them would be a false positive.
    let to_buffer = r#"
        fn f(p: &Path) {
            let mut buf = String::new();
            write!(buf, "{}", p.display());
        }
    "#;
    assert!(
        collect_sites(to_buffer).is_empty(),
        "a write into a String buffer must not be scanned; got {:?}",
        collect_sites(to_buffer)
    );

    // A sanitized write to a stream passes, so the rule is satisfiable.
    assert!(
        collect_sites(r#"fn f(p: &Path) { writeln!(std::io::stderr(), "{}", safe_path(p)); }"#)[0]
            .exprs
            .iter()
            .all(|e| is_sanitizer_call(e)),
        "a WIRE-escaped write to stderr must pass"
    );
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
    let bindings = collect_let_bindings(&masked);
    let non_let = collect_non_let_binders(&masked);
    let mut sites = Vec::new();

    for inv in find_invocations(&masked, PRINT_MACROS) {
        sites.push(Site {
            line: inv.line,
            kind: inv.name.clone(),
            exprs: interpolated_exprs(&inv.body),
        });
    }

    // `write!` / `writeln!` are only prints when their sink is a terminal.
    for inv in find_invocations(&masked, STREAM_WRITE_MACROS) {
        let Some((target, rest)) = split_first_arg(&inv.body) else {
            continue;
        };
        if !is_stream_target(target, &bindings) {
            continue;
        }
        sites.push(Site {
            line: inv.line,
            kind: format!("{}(<stream>)", inv.name),
            exprs: interpolated_exprs(rest),
        });
    }

    // `eprint_warning` escapes its argument in HUMAN mode, which preserves `\n`. Any
    // value interpolated into the string it is handed must therefore be WIRE-escaped in
    // its own right — and the argument may be a local rather than an inline `format!`,
    // so classify it, tracing one hop through its `let` binding.
    for call in find_invocations(&masked, SANITIZING_PRINT_HELPERS) {
        match classify_helper_arg(&call.body, &bindings, &non_let, TRACE_BUDGET) {
            ArgVerdict::Safe => {}
            // A `format!` with nothing interpolated, or a binding that resolved wholly to
            // sanitizer calls, has nothing left to judge — do not record an empty site.
            ArgVerdict::Checked { exprs, .. } if exprs.is_empty() => {}
            ArgVerdict::Checked { via, exprs } => sites.push(Site {
                line: call.line,
                kind: format!("{}({via})", call.name),
                exprs,
            }),
            // Fail closed: an argument shape the trace cannot resolve is reported
            // verbatim, so it must be fixed or justified rather than silently trusted.
            ArgVerdict::Unchecked => sites.push(Site {
                line: call.line,
                kind: format!("{}(untraced)", call.name),
                exprs: vec![normalize(&call.body)],
            }),
        }
    }

    sites
}

/// One `let` binding: the name it introduces and the text of its initialiser.
#[derive(Debug)]
struct Binding {
    name: String,
    init: String,
}

/// How the argument handed to a sanitizing print helper is judged.
#[derive(Debug)]
enum ArgVerdict {
    /// A string literal, or a whole-expression sanitizer call. Nothing further to check.
    Safe,
    /// Resolved to one or more `format!`s; `exprs` is everything they interpolate.
    Checked { via: String, exprs: Vec<String> },
    /// Not a recognised shape. Report it.
    Unchecked,
}

/// How many `let` hops `classify_helper_arg` will follow.
///
/// One. A local initialised from another local is reported rather than followed — an
/// explicit bound, so the trace cannot loop on `let a = b; let b = a;`.
const TRACE_BUDGET: u8 = 1;

/// Collect every `let <name> = <init>;` binding in already-masked source.
///
/// Destructuring patterns (`let Some(x) = …`, `let (a, b) = …`) are skipped: the name is
/// required to be a plain identifier followed by `=` or a `:` type annotation. Names are
/// collected file-wide rather than per-function, which is why `classify_helper_arg`
/// requires *every* binding of a name to be acceptable.
fn collect_let_bindings(text: &str) -> Vec<Binding> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        if let Some(next) = skip_literal(text, b, i) {
            i = next;
            continue;
        }
        if !(text[i..].starts_with("let")
            && !prev_is_ident(b, i)
            && b.get(i + 3).is_some_and(u8::is_ascii_whitespace))
        {
            i += 1;
            continue;
        }
        let mut j = skip_ws(b, i + 3);
        if text[j..].starts_with("mut") && b.get(j + 3).is_some_and(u8::is_ascii_whitespace) {
            j = skip_ws(b, j + 3);
        }
        let start = j;
        while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
            j += 1;
        }
        let name = text[start..j].to_string();
        let after = skip_ws(b, j);
        // A plain binding is followed by `=` (or `: Type =`); anything else is a pattern.
        if !is_ident(&name) || !matches!(b.get(after), Some(b'=') | Some(b':')) {
            i += 3;
            continue;
        }
        let Some((eq, semi)) = find_init_bounds(text, b, after) else {
            i += 3;
            continue;
        };
        out.push(Binding {
            name,
            init: text[eq + 1..semi].trim().to_string(),
        });
        i = semi;
    }
    out
}

/// Collect every name in already-masked source that is introduced by something *other*
/// than a `let` — a `for`-loop variable, a function parameter, or a closure parameter.
///
/// # Why
///
/// `collect_let_bindings` matches names file-wide, not per scope. Without this set, a
/// name bound by one of the shapes above was resolved against whatever unrelated `let`s
/// of the same name the file happened to contain, and was accepted if all of them were
/// safe. On real source that was a live bypass:
///
/// ```ignore
/// // in lint.rs, which has three unrelated `let label = safe_path(…);` bindings
/// fn atk(rules: &[String]) { for label in rules { eprint_warning(label); } }
/// ```
///
/// Every name returned here **poisons** itself for [`classify_helper_arg`]: a bare
/// argument with that name is reported instead of resolved, whatever its `let`s say. A
/// name collected here that is genuinely safe costs one allowlist entry; the opposite
/// mistake costs a review round.
///
/// Over-collection is the safe direction, so the shapes are matched loosely: for a `for`
/// pattern and a parameter pattern, *every* identifier-shaped token in the pattern is
/// taken, keywords aside. `if let` / `while let` / `match`-arm binders are **not**
/// modelled — limit 5 in the module doc.
fn collect_non_let_binders(text: &str) -> Vec<String> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        if let Some(next) = skip_literal(text, b, i) {
            i = next;
            continue;
        }
        // `for <pattern> in …` — the pattern ends at the ` in ` that follows it.
        if text[i..].starts_with("for")
            && !prev_is_ident(b, i)
            && b.get(i + 3).is_some_and(u8::is_ascii_whitespace)
        {
            let tail = &text[i + 3..];
            // Bound the search: a `for` header never runs past its opening brace.
            let head = &tail[..tail.find('{').unwrap_or(tail.len()).min(400)];
            if let Some(kw) = head.find(" in ") {
                push_pattern_idents(&head[..kw], &mut out);
            }
            i += 3;
            continue;
        }
        // `fn name(<params>)` — one entry per parameter.
        if text[i..].starts_with("fn")
            && !prev_is_ident(b, i)
            && b.get(i + 2).is_some_and(u8::is_ascii_whitespace)
        {
            if let Some(rel) = text[i..].find('(') {
                let open = i + rel;
                if let Some(close) = matching_paren(text, b, open) {
                    for param in split_top_level(&text[open + 1..close]) {
                        // `name: Type` — the pattern is everything before the top-level `:`.
                        let pat = param.split(':').next().unwrap_or(&param);
                        push_pattern_idents(pat, &mut out);
                    }
                    i = close;
                    continue;
                }
            }
            i += 2;
            continue;
        }
        // Closure parameters, `|a, b|` / `|a: &T|`. A `|` is only read as the opening
        // delimiter when what follows, up to the next `|`, is parameter-shaped: nothing
        // but identifiers, commas, `&`, `mut`, `ref` and type annotations. That excludes
        // `a | b` (bitwise or) and `a || b`, whose operands are arbitrary expressions.
        if b[i] == b'|' && b.get(i + 1) != Some(&b'|') {
            let tail = &text[i + 1..];
            if let Some(rel) = tail.find('|') {
                let params = &tail[..rel];
                if is_closure_param_list(params) {
                    for param in split_top_level(params) {
                        let pat = param.split(':').next().unwrap_or(&param);
                        push_pattern_idents(pat, &mut out);
                    }
                    // Resume *past* the closing `|`, so the text after a closure is never
                    // read as the parameter list of the next one.
                    i += rel + 2;
                    continue;
                }
            }
        }
        i += 1;
    }
    out.sort();
    out.dedup();
    out
}

/// Binding-position keywords and the receiver, none of which name a value a caller
/// controls.
const PATTERN_KEYWORDS: &[&str] = &["mut", "ref", "self", "impl", "dyn", "in"];

/// Push every identifier-shaped token in a binding pattern.
fn push_pattern_idents(pattern: &str, out: &mut Vec<String>) {
    for tok in pattern.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        if is_ident(tok) && !PATTERN_KEYWORDS.contains(&tok) {
            out.push(tok.to_string());
        }
    }
}

/// Does `s` look like the inside of a closure's `|…|`, rather than the right-hand side
/// of a bitwise `|`? Empty is a closure (`||` is handled by the caller as an early-out,
/// so this only sees `| |`); otherwise every character must be pattern-shaped.
fn is_closure_param_list(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || c.is_ascii_whitespace()
                || matches!(c, '_' | ',' | ':' | '&' | '<' | '>' | '\'' | '[' | ']')
        })
}

/// From the start of a binding's `: Type = init;` tail, find the top-level `=` and the
/// top-level `;` that closes it.
fn find_init_bounds(text: &str, b: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut depth = 0i32;
    let mut eq: Option<usize> = None;
    let mut i = from;
    while i < b.len() {
        if let Some(next) = skip_literal(text, b, i) {
            i = next;
            continue;
        }
        match b[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
            }
            // `=` but not `==` / `=>` / `!=` / `<=` / `>=`.
            b'=' if depth == 0
                && eq.is_none()
                && b.get(i + 1) != Some(&b'=')
                && b.get(i + 1) != Some(&b'>')
                && !matches!(
                    b.get(i.wrapping_sub(1)),
                    Some(b'=' | b'!' | b'<' | b'>' | b'+' | b'-' | b'*' | b'/' | b'%')
                ) =>
            {
                eq = Some(i);
            }
            b';' if depth == 0 => return eq.map(|e| (e, i)),
            _ => {}
        }
        i += 1;
    }
    None
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Judge the single argument handed to a sanitizing print helper.
///
/// Accepts a string literal, a whole-expression sanitizer call, or a whole-expression
/// `format!` (whose interpolations are returned for the caller to check). A bare
/// identifier is resolved through its `let` bindings, up to `budget` hops; a name with no
/// visible binding, with any binding that is itself unrecognised, or that appears in
/// `non_let` (see [`collect_non_let_binders`]), is `Unchecked`.
fn classify_helper_arg(
    arg: &str,
    bindings: &[Binding],
    non_let: &[String],
    budget: u8,
) -> ArgVerdict {
    let e = arg.trim().trim_start_matches(['&', ' ']).trim();
    if e.is_empty() {
        return ArgVerdict::Unchecked;
    }

    // A whole string literal — `eprint_warning("Stopped watching.")`.
    if let Some((_, end)) = parse_string_literal(e) {
        if e[end..].trim().is_empty() {
            return ArgVerdict::Safe;
        }
    }

    // A whole-expression sanitizer call.
    if is_sanitizer_call(e) {
        return ArgVerdict::Safe;
    }

    // A whole-expression `format!(…)` — check what it interpolates.
    if let Some(body) = whole_invocation_body(e, "format!") {
        return ArgVerdict::Checked {
            via: "format!".to_string(),
            exprs: interpolated_exprs(&body),
        };
    }

    // A bare local: follow its binding(s) — unless the name is also introduced by a
    // `for` variable, a parameter or a closure param somewhere in the file, in which case
    // the file's `let`s of that name say nothing about this value. Fail closed.
    if is_ident(e) {
        if budget == 0 || non_let.iter().any(|n| n == e) {
            return ArgVerdict::Unchecked;
        }
        let mut matched = false;
        let mut exprs = Vec::new();
        for binding in bindings.iter().filter(|b| b.name == e) {
            matched = true;
            match classify_helper_arg(&binding.init, bindings, non_let, budget - 1) {
                ArgVerdict::Safe => {}
                ArgVerdict::Checked { exprs: mut v, .. } => exprs.append(&mut v),
                // One unrecognised binding of this name poisons the whole trace.
                ArgVerdict::Unchecked => return ArgVerdict::Unchecked,
            }
        }
        if !matched {
            return ArgVerdict::Unchecked;
        }
        return ArgVerdict::Checked {
            via: format!("let {e} = …"),
            exprs,
        };
    }

    ArgVerdict::Unchecked
}

/// Body of `name(…)` when it spans the *entire* expression — nothing may trail the
/// closing paren, or a postfix continuation could undo whatever the call did.
fn whole_invocation_body(expr: &str, name: &str) -> Option<String> {
    let rest = expr.strip_prefix(name)?;
    let open = expr.len() - rest.trim_start().len();
    if expr.as_bytes().get(open) != Some(&b'(') {
        return None;
    }
    let close = matching_paren(expr, expr.as_bytes(), open)?;
    expr[close + 1..]
        .trim()
        .is_empty()
        .then(|| expr[open + 1..close].to_string())
}

/// Does this `write!` / `writeln!` target a terminal stream rather than a buffer?
///
/// True when the target expression names stdout/stderr, or is a local whose `let`
/// initialiser does. See "Accepted limits" in the module doc for what this misses.
fn is_stream_target(target: &str, bindings: &[Binding]) -> bool {
    let t = target.trim();
    if names_stream(t) {
        return true;
    }
    let ident = t.trim_start_matches(['&', ' ']).trim();
    let ident = ident.strip_prefix("mut ").unwrap_or(ident).trim();
    is_ident(ident)
        && bindings
            .iter()
            .any(|b| b.name == ident && names_stream(&b.init))
}

fn names_stream(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.contains("stdout") || lower.contains("stderr")
}

/// Split off the first top-level argument, returning it and the text after its comma.
fn split_first_arg(body: &str) -> Option<(&str, &str)> {
    let b = body.as_bytes();
    let mut depth = 0i32;
    let mut i = 0usize;
    while i < b.len() {
        if let Some(next) = skip_literal(body, b, i) {
            i = next;
            continue;
        }
        match b[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => return Some((&body[..i], &body[i + 1..])),
            _ => {}
        }
        i += 1;
    }
    None
}

/// Is `expr` *exactly* a call to one of [`SANITIZERS`], possibly module-qualified and
/// possibly behind leading `&`?
///
/// Both ends are checked. Nothing may precede the callee but `&` and whitespace, so a
/// sanitizer nested in an outer call (`wrap(safe_path(p))`) is rejected — the outer call
/// could undo the escape. And nothing may follow the closing paren, so a postfix
/// continuation (`safe_path(p) + &evil`, `safe_path(p).replace("a", &evil)`) is rejected
/// for the same reason.
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
    if !segments
        .last()
        .is_some_and(|last| SANITIZERS.contains(last))
    {
        return false;
    }
    // The call must be the whole expression.
    matching_paren(e, e.as_bytes(), open).is_some_and(|close| e[close + 1..].trim().is_empty())
}

/// The written justification exempting `expr` at a site of this `kind`, if any.
///
/// [`ALLOWED_UNTRACED_HELPER_ARGS`] applies *only* to the untraced-helper-argument
/// position; [`ALLOWED_UNSANITIZED`] applies to every other site. The two lists never
/// cover for each other, so exempting a loop variable as a warning body does not also
/// exempt that name in an `eprintln!`.
fn justification(file: &str, expr: &str, kind: &str) -> Option<&'static str> {
    let list = if kind.ends_with("(untraced)") {
        ALLOWED_UNTRACED_HELPER_ARGS
    } else {
        ALLOWED_UNSANITIZED
    };
    list.iter()
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
            if text[i..].starts_with(name) && !prev_is_ident(b, i) && !is_fn_definition(text, i) {
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

/// Is the name at `i` introduced by `fn`, i.e. a definition rather than a call?
///
/// Without this, `fn eprint_warning(w: &str)` in `output.rs` would be scanned as a call
/// to itself whose argument is the parameter list.
fn is_fn_definition(text: &str, i: usize) -> bool {
    let head = text[..i].trim_end();
    let Some(before) = head.strip_suffix("fn") else {
        return false;
    };
    !before.ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
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
