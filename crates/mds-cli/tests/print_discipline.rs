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
//! - `println!` / `eprintln!` / `print!` / `eprint!` in `crates/mds-cli/src/**`, and the
//!   CLI's own stderr writer macros `ewriteln!` / `ewrite!` (`output.rs`, #157), for the
//!   union of their inline captures (`{name}`) and their positional arguments. The guard
//!   matches a macro by name, so a stderr writer is covered only while it is listed in
//!   [`PRINT_MACROS`]: every site that moves to an unlisted writer leaves the guard.
//!   [`SITE_FLOORS`] fails when a listed file's site count drops, which is how such a
//!   move shows up. Every file with a print site has a floor, or a written reason in
//!   [`FLOORS_PENDING`] — `lint.rs`, until the lint pipeline refactor (#309) settles its
//!   count ([`every_printing_file_has_a_site_floor`]).
//! - **No std print macro at all.** `println!` / `print!` / `eprintln!` / `eprint!` /
//!   `dbg!` panic when their write fails, so none may be named anywhere in
//!   `crates/mds-cli/src/**`: not called, not renamed on import, not wrapped in another
//!   macro — test modules and the writer macros' own bodies included, with no exemption
//!   ([`no_raw_print_macro_outside_the_writer`], #157). Status lines go through the
//!   writer macros; a command's product goes through `write_stdout`.
//! - **One way out of the process.** Only the exit funnel `output::exit` calls
//!   `std::process::exit` ([`EXIT_FUNNELS`]); `std::process::abort` appears nowhere, and
//!   a `use` that would let a call skip naming `process::exit` — renamed, braced,
//!   globbed, or through a renamed `process` — is reported
//!   ([`process_exit_only_in_the_funnel`]).
//! - Both of those scans read every module the crate compiles: `crate_sources` resolves
//!   each `mod name;` from `main.rs` as rustc does and fails on one it did not read.
//! - **Every mention of `write_stderr_fmt`**, the function the writer macros expand to.
//!   Its argument is a `format_args!`, which is not a print site, so a direct call would
//!   reach stderr with nothing it interpolates scanned — and so would every site of a new
//!   macro built on it under a name [`PRINT_MACROS`] does not list.
//!   [`the_stderr_writer_fn_is_called_only_by_the_writer_macros`] fails on any mention
//!   outside its `fn` definition and the bodies of the `macro_rules!` definitions
//!   [`PRINT_MACROS`] names, a renamed `use` of it included.
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
//! - `write!` / `writeln!` into an in-memory `String`, and stdout writes through a
//!   `write_stdout` byte sink. Compiled template output is the command's *product* and
//!   must stay byte-faithful.
//! - clap's own help, version and usage output. `main.rs` prints it with clap's
//!   `err.print()`, which is neither a print macro nor `write_stderr_fmt`, so it is
//!   outside every scan here by construction (the raw-print ban skips a method of that
//!   name on purpose). `main.rs` applies the stream rule to its result (#157); the text,
//!   an argument clap echoes back included, is clap's to render.
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
//!     key is a filesystem path. Since #265 the built-in backends refuse a forbidden
//!     path character in every key they resolve, so only a custom `FileSystem` backend
//!     that rewrites keys can still deliver one. **Pinned by a test**:
//!     `crates/mds-cli/tests/producer_discipline.rs`, which drives a module key carrying
//!     a real ESC byte through such a backend and asserts the warning that reaches this
//!     crate is WIRE-escaped, and pins the refusal on the built-in backend.
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
//!    name in the same file (`empty_count`, `ok_count`, `max_depth`) inherits the exemption
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
//! 6. **Print macros are matched by the name they are invoked under.** Renaming a writer
//!    macro on import — `use crate::output::ewriteln as say;` — takes every `say!(…)`
//!    site out of the sanitizer scan, the same shape as limit 1 for sanitizers. Renaming
//!    the writer *function* on import is caught: it is a mention of `write_stderr_fmt`
//!    outside the writer macros. So is renaming a std print macro: the raw-print ban
//!    reports any mention of its name, `use std::eprintln as say;` included.
//! 7. **The raw-print and exit bans read this crate's text only.** Code in a dependency
//!    that prints through std or ends the process is not a mention here: mds-core's
//!    `emit_warnings`, still an `eprintln!` (#435), and clap's `Error::exit` and
//!    `Parser::parse`, which exit on their own — `main.rs` calls neither.
//!
//! Every one of these requires writing code that looks wrong on purpose. The bar this
//! guard is built to meet is **accidental** reintroduction — the four times #176 was
//! reopened, it was an ordinary `eprintln!` or an ordinary hoisted `format!`, never an
//! alias. Closing the lexical gaps beyond that bar would need a rustc lint or a
//! `syn`-based analysis over expanded HIR, which is a different tool.
//!
//! # The escape helpers are not special-cased
//!
//! `eprint_error` and `eprint_warning` are the two functions that write a whole
//! diagnostic to the stream (each through one `ewriteln!`), and neither gets a blanket
//! exemption. `eprint_error` passes with no allowlist entry at all: its single
//! interpolated argument *is* `render_error_sanitized(report)`.
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
//!   cannot resolve at all, [`the_guard_refuses_to_resolve_a_non_let_binder`] for one
//!   whose name is shadowed by a `for` / parameter / closure binder,
//!   [`the_guard_scans_the_stderr_writer_macros`] for `ewriteln!` / `ewrite!`, and
//!   [`the_writer_fn_guard_flags_direct_calls_aliases_and_unlisted_macros`] for a direct
//!   call to the writer function, a renamed import of it and a macro built on it under
//!   an unlisted name.
//!   [`the_raw_print_guard_flags_every_std_print_macro_and_its_aliases`] flags each std
//!   print macro, a path-qualified call, a renamed import and a wrapping macro;
//!   [`the_exit_guard_flags_every_way_out_but_the_funnel`] flags an exit outside the
//!   funnel, `abort`, and each import shape that hides one; and
//!   [`the_module_walk_finds_every_module_a_crate_declares`] proves the coverage check
//!   resolves flat, directory and nested modules.
//! - **Negative:** [`cli_print_sites_sanitize_every_interpolated_value`],
//!   [`the_stderr_writer_fn_is_called_only_by_the_writer_macros`],
//!   [`no_raw_print_macro_outside_the_writer`] and [`process_exit_only_in_the_funnel`]
//!   prove the real sources are clean.
//! - **Non-vacuity:** the same test asserts the scanner actually found the crate's
//!   modules, its print sites (crate-wide, and per file for the files in
//!   [`SITE_FLOORS`]), its interpolations, its `let` bindings, the non-`let` binders that
//!   poison a name, and its calls into the sanitizing print helpers, so it cannot pass
//!   because the parser silently returned nothing. The two bans read every module the
//!   crate declares, the raw-print ban saw the writer calls, and the exit ban found each
//!   funnel holding exactly its one `process::exit`.
//! - **Allowlist rot:** [`every_allowlist_entry_is_live`] fails if an entry in either
//!   allowlist stops matching anything, so exemptions cannot outlive the code that
//!   needed them.

use std::path::{Path, PathBuf};

// ── Configuration ─────────────────────────────────────────────────────────────

/// Macros that write directly to a terminal stream: std's four, and the CLI's
/// non-panicking stderr writer (`output.rs`, #157).
///
/// A stderr writer missing from this list is invisible to every check in this file.
const PRINT_MACROS: &[&str] = &[
    "eprintln!",
    "println!",
    "eprint!",
    "print!",
    "ewriteln!",
    "ewrite!",
];

/// Minimum print-site count per file, for the files whose prints go through the stderr
/// writer macros.
///
/// The crate-wide floor in [`cli_print_sites_sanitize_every_interpolated_value`] cannot
/// see one file's sites leave the guard: a file whose prints move to a writer macro that
/// [`PRINT_MACROS`] does not list drops out of the scan while the crate-wide count still
/// clears its floor. A per-file floor fails instead. Each floor is the file's site count
/// when its prints moved to the writer; lowering one is a decision made in the commit
/// that removes the print, never a side effect.
const SITE_FLOORS: &[(&str, usize)] = &[
    ("build.rs", 27),
    ("fmt.rs", 11),
    ("main.rs", 11),
    ("output.rs", 6),
    ("watch.rs", 18),
];

/// Files that print but have no [`SITE_FLOORS`] entry yet, each with the reason.
///
/// [`every_printing_file_has_a_site_floor`] fails on a printing file listed in neither
/// place, and on an entry here for a file that has a floor or no print site, so the
/// list cannot outlive its reason.
const FLOORS_PENDING: &[(&str, &str)] = &[(
    "lint.rs",
    "`mds lint`'s status lines are moving out of lint.rs into the result sinks of the \
     lint pipeline refactor (#309), which changes this file's site count on purpose. \
     Its floor is set when that lands, so it pins the final count; until then the \
     crate-wide floor and the sanitizer check still cover every site in it.",
)];

/// std's print macros, and `dbg!`, which prints through `eprintln!` (#157).
///
/// Each panics when its write fails, which is how a closed or failing stream ended a run
/// with exit 101. None may appear anywhere in `crates/mds-cli/src/**` — see
/// [`no_raw_print_macro_outside_the_writer`].
const RAW_PRINT_MACROS: &[&str] = &["println", "print", "eprintln", "eprint", "dbg"];

/// The functions allowed to call `std::process::exit`, by file: the CLI's exit funnel,
/// which applies the output-failure rule to every exit code (#157). A second funnel — a
/// panic path that cannot return to it — is listed here beside it, never exempted by
/// name elsewhere.
const EXIT_FUNNELS: &[(&str, &str)] = &[("output.rs", "exit")];

/// The `std::process` functions that end the process: `exit` only through a funnel,
/// `abort` never.
const PROCESS_ENDERS: &[&str] = &["exit", "abort"];

/// The function the stderr writer macros expand to (`output.rs`, #157).
///
/// Its argument is a `format_args!`, which is not a print site, so a direct call would
/// write to stderr with nothing it interpolates scanned. It may be named only at its
/// definition and inside the bodies of the macros in [`PRINT_MACROS`] — see
/// [`the_stderr_writer_fn_is_called_only_by_the_writer_macros`].
const STDERR_WRITER_FN: &str = "write_stderr_fmt";

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
        "build.rs",
        "empty_count",
        "`usize` tally of successful compilations whose written artifact is zero \
         bytes, in the `mds build <dir>` summary line (R5). An integer counter \
         cannot carry a control byte.",
    ),
    (
        "build.rs",
        "partials_only_count",
        "`usize` count of `.mds` files found in a partials-only tree, bound from \
         `output::partials_only`'s `Some(n)` in the #387 nothing-to-build diagnostic. \
         An integer counter cannot carry a control byte.",
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
        "main.rs",
        "partials_only_count",
        "`usize` count of `.mds` files found in a partials-only tree, bound from \
         `output::partials_only`'s `Some(n)` in the #387 nothing-to-check diagnostic. \
         An integer counter cannot carry a control byte.",
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
    // AD-216-3/5/10: four counters for the `mds lint <dir>` summary line.
    // AD-216-10: names are file-unique (limit 2, :106-110) — none collide with
    // existing lint.rs entries; no future variable silently inherits an exemption
    // by reusing an already-listed name.
    (
        "lint.rs",
        "clean_count",
        "`usize` tally of files with no lint findings in the `mds lint <dir>` summary line.",
    ),
    (
        "lint.rs",
        "warn_file_count",
        "`usize` tally of files with warning-severity findings in the `mds lint <dir>` summary line.",
    ),
    (
        "lint.rs",
        "error_file_count",
        "`usize` tally of files with error-severity findings or analysis failures \
         in the `mds lint <dir>` summary line.",
    ),
    (
        "lint.rs",
        "limit_file_count",
        "`usize` tally of files that aborted with `MdsError::ResourceLimit` \
         in the `mds lint <dir>` summary line.",
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
    let mut sites_per_file: Vec<(String, usize)> = Vec::new();

    for file in &files {
        let name = file_key(file);
        let src = std::fs::read_to_string(file).expect("mds-cli source must be readable");
        let masked = mask_comments(&src);
        total_bindings += collect_let_bindings(&masked).len();
        total_non_let += collect_non_let_binders(&masked).len();
        total_helper_calls += find_invocations(&masked, SANITIZING_PRINT_HELPERS).len();
        let sites = collect_sites(&src);
        sites_per_file.push((name.clone(), sites.len()));
        for site in sites {
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
    for (file, floor) in SITE_FLOORS {
        let found = sites_per_file
            .iter()
            .find(|(name, _)| name == file)
            .map(|(_, n)| *n);
        assert!(
            found.is_some_and(|n| n >= *floor),
            "non-vacuity: expected at least {floor} print sites in {file}, found {found:?}. \
             Either its prints moved to a writer PRINT_MACROS does not list (list it), or a \
             print was removed (lower SITE_FLOORS in the same commit). All files: \
             {sites_per_file:?}"
        );
    }
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

/// The writer macros are print sites because the guard knows their names. The function
/// they expand to is not: a direct call to it, or a macro built on it under a name
/// [`PRINT_MACROS`] does not list, writes to stderr with nothing it interpolates scanned.
/// So the function may be named only at its definition and inside the writer macros.
#[test]
fn the_stderr_writer_fn_is_called_only_by_the_writer_macros() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut stray: Vec<String> = Vec::new();
    let mut allowed_in_output_rs = 0usize;
    for file in rust_files(&src_dir) {
        let name = file_key(&file);
        let src = std::fs::read_to_string(&file).expect("mds-cli source must be readable");
        let mentions = writer_fn_mentions(&src);
        if name == "output.rs" {
            allowed_in_output_rs += mentions.allowed;
        }
        stray.extend(
            mentions
                .stray
                .into_iter()
                .map(|line| format!("  {name}:{line}")),
        );
    }

    // Non-vacuity: the scan found the definition and the `ewrite!` / `ewriteln!` bodies,
    // so an empty `stray` means it looked where the writer lives.
    assert!(
        allowed_in_output_rs >= 3,
        "non-vacuity: expected `{STDERR_WRITER_FN}`'s definition and the writer macros' \
         bodies in output.rs (at least 3 mentions), found {allowed_in_output_rs}"
    );
    assert!(
        stray.is_empty(),
        "print-discipline violation: `{STDERR_WRITER_FN}` is named outside its definition \
         and the bodies of the writer macros in {PRINT_MACROS:?}:\n{}\n\n\
         Write through `ewriteln!` / `ewrite!` so every interpolated value is scanned; a \
         new stderr writer macro goes into PRINT_MACROS.",
        stray.join("\n")
    );
}

/// A per-file floor is only a guard for the files that have one. Every file with a print
/// site has a [`SITE_FLOORS`] entry or a written reason in [`FLOORS_PENDING`], and each
/// pending entry is still a printing file without a floor.
#[test]
fn every_printing_file_has_a_site_floor() {
    let mut unfloored: Vec<String> = Vec::new();
    let mut printing = 0usize;
    for (name, src) in crate_sources() {
        let sites = collect_sites(&src).len();
        let floored = SITE_FLOORS.iter().any(|(file, _)| *file == name);
        let pending = FLOORS_PENDING.iter().any(|(file, _)| *file == name);
        if sites > 0 {
            printing += 1;
        }
        match (sites > 0, floored, pending) {
            (true, false, false) => {
                unfloored.push(format!("  {name}: {sites} print sites, no floor"));
            }
            (_, true, true) => unfloored.push(format!(
                "  {name}: has a floor, so its FLOORS_PENDING entry must go"
            )),
            (false, _, true) => unfloored.push(format!(
                "  {name}: prints nothing, so its FLOORS_PENDING entry must go"
            )),
            _ => {}
        }
    }
    assert!(
        printing >= SITE_FLOORS.len() + FLOORS_PENDING.len(),
        "non-vacuity: expected at least {} printing files, found {printing}",
        SITE_FLOORS.len() + FLOORS_PENDING.len()
    );
    for (file, why) in FLOORS_PENDING {
        assert!(
            why.len() >= 40,
            "FLOORS_PENDING entry {file} needs a real reason, got {why:?}"
        );
    }
    assert!(
        unfloored.is_empty(),
        "print-discipline violation: a file's print sites are unguarded by a floor:\n{}\n\n\
         Add the file to SITE_FLOORS at its current site count, or to FLOORS_PENDING with \
         the reason its count is about to change.",
        unfloored.join("\n")
    );
}

/// std's print macros panic when their write fails, which is how a closed or failing
/// stream ended a run with exit 101 (#157). The CLI prints through `ewriteln!` /
/// `ewrite!`, which never panic, and writes its product through `write_stdout`, so no
/// [`RAW_PRINT_MACROS`] name may appear in `crates/mds-cli/src/**`: not as a call, not
/// in a `use` that renames one, not wrapped in another macro.
///
/// No exemption, test modules included. The writer macros need none (they expand to
/// `write_stderr_fmt`), and a writer body that reached for `eprintln!` would bring the
/// panic back. A unit test's skip notice goes through the writer too — it then reaches
/// the real stderr uncaptured, which is where a skip notice belongs.
#[test]
fn no_raw_print_macro_outside_the_writer() {
    let sources = crate_sources();
    let mut found: Vec<String> = Vec::new();
    let mut writer_calls = 0usize;
    for (name, src) in &sources {
        found.extend(
            raw_print_mentions(src)
                .into_iter()
                .map(|line| format!("  {name}:{line}")),
        );
        writer_calls += find_invocations(&mask_comments(src), &["ewriteln!", "ewrite!"]).len();
    }

    // Non-vacuity: the scan read the code the CLI prints from.
    assert!(
        writer_calls >= 80,
        "non-vacuity: expected at least 80 `ewriteln!` / `ewrite!` calls across \
         mds-cli/src, found {writer_calls}"
    );
    assert!(
        found.is_empty(),
        "print-discipline violation: a std print macro ({RAW_PRINT_MACROS:?}) is named in \
         mds-cli/src:\n{}\n\nWrite status lines through `crate::output::ewriteln!` / \
         `ewrite!` and a command's product through `crate::output::write_stdout`; neither \
         panics when its stream fails.",
        found.join("\n")
    );
}

/// Only the exit funnel ends the process: it applies the rule that an output failure
/// lifts the exit code to at least 2 (#157), and any other `std::process::exit` would
/// skip it. `std::process::abort` never appears. Renaming either on import, a glob import
/// of `std::process`, or renaming `process` itself is reported as well, since the call
/// it enables would no longer name `process::exit`.
#[test]
fn process_exit_only_in_the_funnel() {
    let mut stray: Vec<String> = Vec::new();
    let mut in_funnel = 0usize;
    for (name, src) in crate_sources() {
        let funnels: Vec<&str> = EXIT_FUNNELS
            .iter()
            .filter(|(file, _)| *file == name)
            .map(|(_, function)| *function)
            .collect();
        let ends = process_end_mentions(&src, &funnels);
        in_funnel += ends.in_funnel;
        stray.extend(
            ends.stray
                .into_iter()
                .map(|line| format!("  {name}:{line}")),
        );
    }

    // Non-vacuity: each funnel was found, and holds its one `process::exit`.
    assert_eq!(
        in_funnel,
        EXIT_FUNNELS.len(),
        "non-vacuity: each of {EXIT_FUNNELS:?} must hold exactly one `std::process::exit`"
    );
    assert!(
        stray.is_empty(),
        "exit-funnel violation: the process is ended, or can be, outside {EXIT_FUNNELS:?}:\n\
         {}\n\nEnd the run through `crate::output::exit(code)`.",
        stray.join("\n")
    );
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
fn the_guard_scans_the_stderr_writer_macros() {
    // `ewriteln!` / `ewrite!` are the CLI's stderr writer (#157). Every site that uses
    // them is a print site exactly like `eprintln!`, bare or path-qualified.
    let src = r#"
        fn f(path: &Path, e: &str) {
            output::ewriteln!("warning: {}", path.display());
            crate::output::ewrite!("{e}");
            ewriteln!("Clean: {}", safe_path(path));
            ewriteln!("Stopped watching.");
        }
    "#;
    let sites = collect_sites(src);
    let kinds: Vec<&str> = sites.iter().map(|s| s.kind.as_str()).collect();
    assert_eq!(
        kinds,
        ["ewriteln!", "ewrite!", "ewriteln!", "ewriteln!"],
        "every writer invocation must be a site; got {sites:?}"
    );
    assert_eq!(sites[0].exprs, vec!["path.display()".to_string()]);
    assert_eq!(sites[1].exprs, vec!["e".to_string()]);
    for expr in sites[0].exprs.iter().chain(&sites[1].exprs) {
        assert!(
            !is_sanitizer_call(expr),
            "`{expr}` must be reported, not accepted"
        );
    }
    assert_eq!(sites[2].exprs, vec!["safe_path(path)".to_string()]);
    assert!(
        is_sanitizer_call(&sites[2].exprs[0]),
        "a WIRE-escaped writer site must pass"
    );
    assert!(sites[3].exprs.is_empty(), "a literal interpolates nothing");

    // The macros' own definitions and re-exports are not call sites.
    let definition = r#"
        macro_rules! ewriteln {
            ($($arg:tt)+) => {
                $crate::output::write_stderr_fmt(::std::format_args!($($arg)+))
            };
        }
        pub(crate) use ewriteln;
    "#;
    assert!(
        collect_sites(definition).is_empty(),
        "a macro definition is not a print site; got {:?}",
        collect_sites(definition)
    );
}

#[test]
fn the_writer_fn_guard_flags_direct_calls_aliases_and_unlisted_macros() {
    // Accepted: the definition and the bodies of macros PRINT_MACROS lists.
    let writer = r#"
        macro_rules! ewrite {
            ($($arg:tt)*) => {
                $crate::output::write_stderr_fmt(::std::format_args!($($arg)*))
            };
        }
        macro_rules! ewriteln {
            () => {
                $crate::output::write_stderr_fmt(::std::format_args!("\n"))
            };
        }
        pub(crate) fn write_stderr_fmt(args: std::fmt::Arguments<'_>) {
            write_stderr_to(&OUTPUT_STATE, &mut std::io::stderr().lock(), args);
        }
    "#;
    assert_eq!(
        writer_fn_mentions(writer),
        WriterFnMentions {
            stray: Vec::new(),
            allowed: 3
        },
        "the definition and both writer macro bodies must be accepted"
    );

    // Reported: a direct call, a renamed import, and a macro built on it whose name
    // PRINT_MACROS does not list — each on its own line.
    let bypasses = r#"
        fn f(path: &Path) {
            output::write_stderr_fmt(format_args!("{}\n", path.display()));
        }
        use crate::output::write_stderr_fmt as say;
        macro_rules! ewarn {
            ($($arg:tt)*) => {
                $crate::output::write_stderr_fmt(::std::format_args!($($arg)*))
            };
        }
    "#;
    assert_eq!(
        writer_fn_mentions(bypasses),
        WriterFnMentions {
            stray: vec![3, 5, 8],
            allowed: 0
        },
        "a direct call, a renamed import and an unlisted macro must each be reported"
    );

    // Neither: a comment, a doc link, a string, and a longer identifier.
    let prose = r#"
        // Never call write_stderr_fmt directly.
        /// See [`write_stderr_fmt`].
        fn g() -> &'static str { "write_stderr_fmt(" }
        fn write_stderr_fmt_len() -> usize { 0 }
    "#;
    assert_eq!(
        writer_fn_mentions(prose),
        WriterFnMentions {
            stray: Vec::new(),
            allowed: 0
        },
        "comments, strings and other identifiers are not mentions"
    );
}

#[test]
fn the_raw_print_guard_flags_every_std_print_macro_and_its_aliases() {
    // Reported, each on its own line: all five macros, a path-qualified call, a renamed
    // import, a macro that shadows one and a macro that wraps one — the writer's own
    // body included, since the ban has no exemption.
    let raw = r#"
        fn f(p: &Path) {
            eprintln!("{}", safe_path(p));
            std::println!("x");
            print!("y");
            eprint!("z");
            let _v = dbg!(1);
        }
        use std::eprintln as say;
        macro_rules! eprint { () => {}; }
        macro_rules! ewriteln { ($($t:tt)*) => { eprintln!($($t)*) }; }
    "#;
    assert_eq!(raw_print_mentions(raw), vec![3, 4, 5, 6, 7, 9, 10, 11]);

    // Not mentions: the writer, a method or field of that name (clap's `err.print()`),
    // a function definition, longer identifiers, comments and literals.
    let clean = r#"
        fn f(err: &clap::Error, p: &Path) {
            crate::output::ewriteln!("Clean: {}", safe_path(p));
            ewrite!("{}", "eprintln!(\"no\")");
            let _ = err.print();
            let _ = err
                .print();
            eprint_warning("print");
            print_diff(p);
            // eprintln!("a comment");
            /* dbg!(x) */
        }
        fn print() {}
    "#;
    assert_eq!(raw_print_mentions(clean), Vec::<usize>::new());
}

#[test]
fn the_exit_guard_flags_every_way_out_but_the_funnel() {
    // Accepted: the funnel's own `process::exit`, however qualified.
    let funnel = r#"
        /// Ends the run; `std::process::exit` in a doc comment is not a call.
        pub(crate) fn exit(verdict: i32) -> ! {
            std::process::exit(final_exit_code(verdict))
        }
    "#;
    assert_eq!(
        process_end_mentions(funnel, &["exit"]),
        ProcessEnds {
            stray: Vec::new(),
            in_funnel: 1
        }
    );

    // Reported, each on its own line: an exit outside the funnel (the same funnel body
    // in a file that lists no funnel included), both qualifications, `abort`, a renamed
    // import, a braced import, a glob, and a renamed `process`.
    let bypasses = r#"
        fn bail() { std::process::exit(1); }
        fn out() { process::exit(2) }
        fn crash() { ::std::process::abort() }
        use std::process::exit as quit;
        use std::process::{self, abort};
        use std::process::*;
        use std::process as p;
        use std::{process::exit};
    "#;
    assert_eq!(
        process_end_mentions(bypasses, &["exit"]),
        ProcessEnds {
            stray: vec![2, 3, 4, 5, 6, 7, 8, 9],
            in_funnel: 0
        }
    );
    assert_eq!(
        process_end_mentions(funnel, &[]).stray,
        vec![4],
        "the funnel body counts as the funnel only in the file that lists it"
    );

    // Neither: the funnel's callers, other `std::process` items, a plain module import,
    // names that merely contain the words, comments and literals.
    let prose = r#"
        use std::process;
        use std::process::{Command, Stdio};
        fn f() {
            output::exit(2);
            let code = exit_code(&e);
            let _ = process::Command::new("mds");
            let _ = "std::process::exit(1)";
            // process::abort();
        }
    "#;
    assert_eq!(
        process_end_mentions(prose, &["exit"]),
        ProcessEnds {
            stray: Vec::new(),
            in_funnel: 0
        }
    );
}

#[test]
fn the_module_walk_finds_every_module_a_crate_declares() {
    // A crate on disk: `main.rs` declares a flat module, a directory module and a nested
    // one; an inline `mod tests { … }` and a commented-out declaration are no files.
    let root = tempfile::tempdir().expect("create a temporary crate");
    let src = root.path();
    let write = |rel: &str, text: &str| {
        let path = src.join(rel);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("create a dir");
        std::fs::write(path, text).expect("write a module");
    };
    write(
        "main.rs",
        "mod flat;\npub(crate) mod dir;\n// mod ghost;\nmod tests {}\n",
    );
    write("flat.rs", "#[cfg(test)]\nmod inner;\n");
    write("flat/inner.rs", "");
    write("dir/mod.rs", "");
    let mut found = declared_module_files(&src.join("main.rs"));
    found.sort();
    let mut want: Vec<PathBuf> = ["main.rs", "flat.rs", "flat/inner.rs", "dir/mod.rs"]
        .iter()
        .map(|rel| src.join(rel))
        .collect();
    want.sort();
    assert_eq!(found, want);
    assert_eq!(
        module_declarations("mod a;\nmod b { }\n  pub mod c ;\nlet s = \"mod d;\";\n"),
        vec!["a".to_string(), "c".to_string()]
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

/// Where [`STDERR_WRITER_FN`] is named in one source file.
#[derive(Debug, PartialEq, Eq)]
struct WriterFnMentions {
    /// 1-based line of every mention outside its definition and the writer macros'
    /// bodies: a direct call, a `use` of it, a macro built on it under an unlisted name.
    stray: Vec<usize>,
    /// Mentions at its `fn` definition or inside the body of a `macro_rules!` named in
    /// [`PRINT_MACROS`].
    allowed: usize,
}

/// Find every mention of [`STDERR_WRITER_FN`] in `src` as a whole identifier, outside
/// comments and literals, and sort it into [`WriterFnMentions`].
fn writer_fn_mentions(src: &str) -> WriterFnMentions {
    let masked = mask_comments(src);
    let b = masked.as_bytes();
    let bodies = writer_macro_bodies(&masked);
    let name = STDERR_WRITER_FN.as_bytes();
    let mut mentions = WriterFnMentions {
        stray: Vec::new(),
        allowed: 0,
    };
    let mut i = 0usize;
    while i < b.len() {
        if let Some(next) = skip_literal(&masked, b, i) {
            i = next;
            continue;
        }
        let end = i + name.len();
        let is_mention = b[i..].starts_with(name)
            && !prev_is_ident(b, i)
            && !b
                .get(end)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_');
        if !is_mention {
            i += 1;
            continue;
        }
        if is_fn_definition(&masked, i) || bodies.iter().any(|body| body.contains(&i)) {
            mentions.allowed += 1;
        } else {
            mentions.stray.push(masked[..i].matches('\n').count() + 1);
        }
        i = end;
    }
    mentions
}

/// Byte ranges of the bodies of every `macro_rules!` definition in `masked` whose name is
/// in [`PRINT_MACROS`]. A macro of any other name is not scanned at its call sites, so its
/// body gets no range.
fn writer_macro_bodies(masked: &str) -> Vec<std::ops::RangeInclusive<usize>> {
    const MACRO_RULES: &str = "macro_rules!";
    let b = masked.as_bytes();
    let mut bodies = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        if let Some(next) = skip_literal(masked, b, i) {
            i = next;
            continue;
        }
        if !b[i..].starts_with(MACRO_RULES.as_bytes()) || prev_is_ident(b, i) {
            i += 1;
            continue;
        }
        let name_start = skip_ws(b, i + MACRO_RULES.len());
        let mut name_end = name_start;
        while name_end < b.len() && (b[name_end].is_ascii_alphanumeric() || b[name_end] == b'_') {
            name_end += 1;
        }
        let open = skip_ws(b, name_end);
        let Some(close) = matching_delim(masked, b, open) else {
            i = name_end;
            continue;
        };
        let invoked_as = format!("{}!", &masked[name_start..name_end]);
        if PRINT_MACROS.contains(&invoked_as.as_str()) {
            bodies.push(open..=close);
        }
        i = close + 1;
    }
    bodies
}

/// Every Rust source under `crates/mds-cli/src`, as `(file key, contents)`.
///
/// Fails unless the walk read every module the crate compiles — each `mod name;` resolved
/// from `main.rs` the way rustc resolves it — so a scan over these sources cannot pass by
/// missing a file. A `#[path]` attribute, which that resolution does not model, fails it
/// too.
fn crate_sources() -> Vec<(String, String)> {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = rust_files(&src_dir);
    let sources: Vec<(String, String)> = files
        .iter()
        .map(|file| {
            let src = std::fs::read_to_string(file).expect("mds-cli source must be readable");
            (file_key(file), src)
        })
        .collect();

    let declared = declared_module_files(&src_dir.join("main.rs"));
    let unread: Vec<&PathBuf> = declared.iter().filter(|f| !files.contains(f)).collect();
    assert!(
        declared.len() >= 7 && unread.is_empty(),
        "non-vacuity: the scan must read every module the crate declares (main.rs and at \
         least its 6 modules); declared {declared:?}, not read {unread:?}"
    );
    let with_path_attr: Vec<&str> = sources
        .iter()
        .filter(|(_, src)| mask_comments(src).contains("#[path"))
        .map(|(name, _)| name.as_str())
        .collect();
    assert!(
        with_path_attr.is_empty(),
        "a `#[path]` module in {with_path_attr:?} is not modelled by the module walk; \
         teach `declared_module_files` to resolve it"
    );
    sources
}

/// `root` and every file module it declares, transitively: `mod name;` resolves to
/// `name.rs` or `name/mod.rs` beside a `main.rs`, `lib.rs` or `mod.rs`, and below
/// `<stem>/` for any other file.
fn declared_module_files(root: &Path) -> Vec<PathBuf> {
    let mut found = vec![root.to_path_buf()];
    let mut next = 0usize;
    // Bounded: each file is queued at most once, and a crate declares finitely many.
    while next < found.len() {
        let file = found[next].clone();
        next += 1;
        // A declared file that does not exist is reported by the caller as not read.
        let Ok(src) = std::fs::read_to_string(&file) else {
            continue;
        };
        let dir = match file.file_name().and_then(|n| n.to_str()) {
            Some("main.rs" | "lib.rs" | "mod.rs") => file.parent().map(Path::to_path_buf),
            _ => file
                .parent()
                .zip(file.file_stem())
                .map(|(parent, stem)| parent.join(stem)),
        }
        .expect("a module file has a parent directory");
        for name in module_declarations(&src) {
            let nested = dir.join(&name).join("mod.rs");
            let child = if nested.is_file() {
                nested
            } else {
                dir.join(format!("{name}.rs"))
            };
            if !found.contains(&child) {
                found.push(child);
            }
        }
    }
    found
}

/// The names of the file modules `src` declares: `mod name;`, not an inline
/// `mod name { … }`, and nothing in a comment or a literal.
fn module_declarations(src: &str) -> Vec<String> {
    let masked = mask_comments(src);
    let b = masked.as_bytes();
    let mut names = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        if let Some(next) = skip_literal(&masked, b, i) {
            i = next;
            continue;
        }
        let is_mod_keyword = b[i..].starts_with(b"mod")
            && !prev_is_ident(b, i)
            && b.get(i + 3).is_some_and(u8::is_ascii_whitespace);
        if !is_mod_keyword {
            i += 1;
            continue;
        }
        let start = skip_ws(b, i + 3);
        let mut end = start;
        while end < b.len() && (b[end].is_ascii_alphanumeric() || b[end] == b'_') {
            end += 1;
        }
        if end > start && b.get(skip_ws(b, end)) == Some(&b';') {
            names.push(masked[start..end].to_string());
        }
        i = end.max(i + 3);
    }
    names
}

/// 1-based lines of every mention of a [`RAW_PRINT_MACROS`] name in `src` as code: a call
/// (`eprintln!(…)`, `std::println!(…)`), a `use` that brings one in (`use std::eprintln as
/// say;`), a macro that shadows or wraps one. Not a mention: a method or field of that
/// name (`err.print()`), a function definition, a longer identifier (`eprint_warning`),
/// and anything in a comment or a literal.
fn raw_print_mentions(src: &str) -> Vec<usize> {
    let masked = mask_comments(src);
    let b = masked.as_bytes();
    let mut lines = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        if let Some(next) = skip_literal(&masked, b, i) {
            i = next;
            continue;
        }
        if !(b[i].is_ascii_alphabetic() || b[i] == b'_') || prev_is_ident(b, i) {
            i += 1;
            continue;
        }
        let mut end = i;
        while end < b.len() && (b[end].is_ascii_alphanumeric() || b[end] == b'_') {
            end += 1;
        }
        if RAW_PRINT_MACROS.contains(&&masked[i..end])
            && !follows_a_dot(b, i)
            && !is_fn_definition(&masked, i)
        {
            lines.push(masked[..i].matches('\n').count() + 1);
        }
        i = end;
    }
    lines
}

/// Is the identifier at `i` a method or field (`value.name`, the dot perhaps on the line
/// above)?
fn follows_a_dot(b: &[u8], i: usize) -> bool {
    let mut j = i;
    while j > 0 && b[j - 1].is_ascii_whitespace() {
        j -= 1;
    }
    j > 0 && b[j - 1] == b'.'
}

/// Where one source file ends the process, or could (see [`process_end_mentions`]).
#[derive(Debug, PartialEq, Eq)]
struct ProcessEnds {
    /// 1-based lines of every way out outside a funnel: a call path naming
    /// `process::exit` or `process::abort`, and a `use` that brings either in under a
    /// name that path would not show — braced, globbed, or through a renamed `process`.
    stray: Vec<usize>,
    /// `process::exit` mentions inside the body of one of the file's funnels.
    in_funnel: usize,
}

/// Find every way `src` ends the process ([`PROCESS_ENDERS`]), allowing a
/// `process::exit` only inside the body of a function named in `funnels`.
fn process_end_mentions(src: &str, funnels: &[&str]) -> ProcessEnds {
    let masked = mask_comments(src);
    let b = masked.as_bytes();
    let bodies: Vec<std::ops::RangeInclusive<usize>> = funnels
        .iter()
        .filter_map(|name| fn_body(&masked, name))
        .collect();
    let line_of = |at: usize| masked[..at].matches('\n').count() + 1;
    let mut ends = ProcessEnds {
        stray: Vec::new(),
        in_funnel: 0,
    };

    let mut i = 0usize;
    while i < b.len() {
        if let Some(next) = skip_literal(&masked, b, i) {
            i = next;
            continue;
        }
        if !b[i..].starts_with(b"process") || prev_is_ident(b, i) {
            i += 1;
            continue;
        }
        let after = i + "process".len();
        let sep = skip_ws(b, after);
        if b[sep..].starts_with(b"::") {
            let start = skip_ws(b, sep + 2);
            let mut end = start;
            while end < b.len() && (b[end].is_ascii_alphanumeric() || b[end] == b'_') {
                end += 1;
            }
            let item = &masked[start..end];
            if item == "exit" && bodies.iter().any(|body| body.contains(&i)) {
                ends.in_funnel += 1;
            } else if PROCESS_ENDERS.contains(&item) {
                ends.stray.push(line_of(i));
            }
        }
        i = after;
    }

    for (at, item) in use_items(&masked) {
        if use_reaches_a_process_ender(&item) {
            ends.stray.push(line_of(at));
        }
    }
    ends.stray.sort_unstable();
    ends.stray.dedup();
    ends
}

/// The byte range of the body of `fn name` in `masked`, braces included.
fn fn_body(masked: &str, name: &str) -> Option<std::ops::RangeInclusive<usize>> {
    let b = masked.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        if let Some(next) = skip_literal(masked, b, i) {
            i = next;
            continue;
        }
        let is_fn_keyword = b[i..].starts_with(b"fn")
            && !prev_is_ident(b, i)
            && b.get(i + 2).is_some_and(u8::is_ascii_whitespace);
        if is_fn_keyword {
            let start = skip_ws(b, i + 2);
            let end = start + name.len();
            let named = b[start..].starts_with(name.as_bytes())
                && !b
                    .get(end)
                    .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_');
            if named {
                let open = end + masked[end..].find('{')?;
                let close = matching_delim(masked, b, open)?;
                return Some(open..=close);
            }
        }
        i += 1;
    }
    None
}

/// Every `use` item in `masked`: the byte offset of its `use` keyword and its text up to
/// the `;`.
fn use_items(masked: &str) -> Vec<(usize, String)> {
    let b = masked.as_bytes();
    let mut items = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        if let Some(next) = skip_literal(masked, b, i) {
            i = next;
            continue;
        }
        let is_use_keyword = b[i..].starts_with(b"use")
            && !prev_is_ident(b, i)
            && b.get(i + 3).is_some_and(u8::is_ascii_whitespace);
        if !is_use_keyword {
            i += 1;
            continue;
        }
        let end = masked[i..].find(';').map_or(b.len(), |rel| i + rel);
        items.push((i, masked[i + 3..end].to_string()));
        i = end;
    }
    items
}

/// Does this `use` item (the text after `use`) bring a [`PROCESS_ENDERS`] function into
/// scope, or rename `process` so that a call to one would not name `process::`?
fn use_reaches_a_process_ender(item: &str) -> bool {
    const PROCESS: &str = "process";
    let b = item.as_bytes();
    let whole_word = |at: &usize| {
        !prev_is_ident(b, *at)
            && !b
                .get(at + PROCESS.len())
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
    };
    let Some(at) = item
        .match_indices(PROCESS)
        .map(|(at, _)| at)
        .find(whole_word)
    else {
        return false;
    };
    let words = |text: &str| -> Vec<String> {
        text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .filter(|w| !w.is_empty())
            .map(str::to_string)
            .collect()
    };
    let rest = item[at + PROCESS.len()..].trim_start();
    match rest.strip_prefix("::") {
        // `process::exit`, `process::{self, abort}`, `process::*`.
        Some(path) => {
            path.trim_start().starts_with('*')
                || words(path)
                    .iter()
                    .any(|w| PROCESS_ENDERS.contains(&w.as_str()))
        }
        // `process as p`: `p::exit(…)` would not name `process::`.
        None => words(rest).first().is_some_and(|w| w == "as"),
    }
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
    matching_delim(src, b, open)
}

/// Index of the delimiter closing the `(`, `[` or `{` at `open`; `None` when `open`
/// holds none of them or it is never closed.
fn matching_delim(src: &str, b: &[u8], open: usize) -> Option<usize> {
    let (open_byte, close_byte) = match b.get(open)? {
        b'(' => (b'(', b')'),
        b'[' => (b'[', b']'),
        b'{' => (b'{', b'}'),
        _ => return None,
    };
    let mut depth = 0i32;
    let mut i = open;
    while i < b.len() {
        if let Some(next) = skip_literal(src, b, i) {
            i = next;
            continue;
        }
        if b[i] == open_byte {
            depth += 1;
        } else if b[i] == close_byte {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}
