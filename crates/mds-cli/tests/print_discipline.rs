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
//!   [`FLOORS_PENDING`] ([`every_printing_file_has_a_site_floor`]).
//! - **No std print macro at all.** `println!` / `print!` / `eprintln!` / `eprint!` /
//!   `dbg!` panic when their write fails, so none may be named anywhere in
//!   `crates/mds-cli/src/**`: not called, not renamed on import, not wrapped in another
//!   macro — test modules and the writer macros' own bodies included, with no exemption
//!   ([`no_raw_print_macro_outside_the_writer`], #157). Status lines go through the
//!   writer macros; a command's product goes through `write_stdout`.
//! - **One way out of the process.** Only the exit funnel `output::exit`, and the panic
//!   path's `output::exit_after_panic` (#389), call `std::process::exit`
//!   ([`EXIT_FUNNELS`]); `std::process::abort` appears nowhere, and a `use` that would let
//!   a call skip naming `process::exit` — renamed, braced, globbed, or through a renamed
//!   `process` — is reported ([`process_exit_only_in_the_funnel`]).
//! - **Stream handles only in the writers.** `stdout` / `stderr`, the std functions that
//!   hand out a terminal stream, are named only inside the functions in
//!   [`STREAM_HANDLE_OWNERS`], or to ask `.is_terminal()`
//!   ([`terminal_streams_are_opened_only_by_the_writers`]). A second writer holding its
//!   own handle would not see a stdout closed for good or a failure already reported;
//!   `mds lint` had one (#157). The panic hook is listed on purpose: it writes one fixed
//!   text, and `tests/panic_hook.rs` pins that it writes nothing else (#389). So is one
//!   unit test of the hook, which only holds stderr's lock.
//! - These three scans read every module the crate compiles: `crate_sources` resolves
//!   each `mod name;` from `main.rs` as rustc does and fails on one it did not read.
//! - **`mds lint` shows its results through its result sink** (#309): lint's writer-macro
//!   calls, stdout writes and error renderers sit in `lint_sink.rs`, never in `lint.rs`,
//!   and neither file ends the process — `main` exits with the code `lint::run_lint`
//!   returns ([`lint_output_goes_through_the_sink`]).
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
//! - **No raw path and no raw error cause in a message** (#390). These three rules read
//!   the crate's code with its `#[cfg(test)]` items left out, every module included:
//!   - `.display()`, `to_string_lossy()` and `to_str()`, std's ways of turning a path
//!     into text, are called only inside the functions [`PATH_TEXT_HELPERS`] lists, as
//!     often as it says, and no message's format string asks for `Debug` (`{:?}`)
//!     ([`paths_become_text_only_in_the_listed_functions`]). Any other path reaches a
//!     message through `safe_path` / `safe_file_display`, as typed and escaped.
//!   - An error value reaches a message — a `format!`, `miette!`, writer macro or
//!     `write!` argument, an `eprint_warning` or `io_error` argument, a `message:` field —
//!     only as its cause, `safe_inline(io_cause(&e))` / `safe_inline(notify_cause(&e))`,
//!     or bare where `io_error` escapes it ([`messages_interpolate_an_error_only_as_its_cause`]).
//!     io, notify and tempfile errors name paths in their own text; the cause helpers
//!     drop them. An error value is a name an `Err(…)` pattern, a `map_err`-style closure
//!     or an error-typed parameter binds, `e` / `err` / `error`, or a `let` one hop from
//!     one. [`PATH_FREE_ERRORS`] lists the error values shown another way, each with its
//!     site count and why its text names no path.
//!   - Every `failed to watch …` message names its directory through
//!     `safe_path(&shown_watched_dir(…))`, at [`WATCH_FAILURE_SITES`] sites
//!     ([`every_watch_failure_names_its_directory_as_shown`]). The rule pins the call, not
//!     what it returns; `shown_watched_dir`'s unit test pins that, including the
//!     directory outside the entry's directory and the root that it names canonically.
//!
//!   The carve-outs are the entries of those lists that are no escape helper, cause or
//!   comparison with a fixed value, each with its reason: the source map's `file` key
//!   and the sidecar name compared with it; the debug-build panic trigger; and the
//!   errors in [`PATH_FREE_ERRORS`].
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
//! 7. **The raw-print, exit and stream-handle bans read this crate's text only, by
//!    name.** Code in a dependency that prints through std or ends the process is not a
//!    mention here: mds-core's `emit_warnings`, still an `eprintln!` (#435), and clap's
//!    `Error::exit` and `Parser::parse`, which exit on their own — `main.rs` calls
//!    neither. Nor is a way out or a stream reached without its std name: `libc::exit`
//!    (a unix dev-dependency, so only a test module could reach it), a descriptor opened
//!    as a file, `/dev/stdout`, or a `main` that returns without calling the funnel.
//! 8. **The path-sink rules read names, not types.** A path turned into text another
//!    way — `into_string()`, `String::from_utf8_lossy` of its bytes, a `{:?}` outside
//!    [`MESSAGE_MACROS`] or in a format string that is not a literal — is not a
//!    mention, and the text a listed function takes is trusted to go where its reason
//!    says: the count does not change when it is shown raw. An error held under a name
//!    none of the binders above gives it (a struct field, a value two `let`s away, a
//!    pattern binder other than `Err(…)`), or a cause hoisted into a `let` and
//!    interpolated unescaped, is not seen. Nor is a message built by a function this
//!    crate does not define. An io error cannot become an `MdsError` through `?`:
//!    mds-core defines no conversion from `io::Error`, which is the type system's
//!    guarantee, not this guard's.
//! 9. **Only `#[cfg(test)]` items are skipped by the path-sink rules.** Code under
//!    another test-only `cfg` is read as product code, which can only add findings.
//!
//! Each of limits 1–7 requires writing code that looks wrong on purpose; limit 8 does
//! not (a listed name shown raw looks ordinary), and review remains its check. The bar
//! this guard is built to meet is **accidental** reintroduction — the four times #176 was
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
//!   funnel, `abort`, and each import shape that hides one;
//!   [`the_stream_handle_guard_flags_a_second_writer`] flags a stream handle outside its
//!   owners, each import shape, and one bound before it is queried;
//!   [`the_module_walk_finds_every_module_a_crate_declares`] proves the coverage check
//!   resolves flat, directory and nested modules; and
//!   [`lint_output_goes_through_the_sink`] reports a writer call, a stdout write, an error
//!   renderer and an exit planted in `lint.rs`, and an exit planted in `lint_sink.rs`.
//!   For the path-sink rules, [`the_path_text_guard_flags_a_path_shown_outside_the_helpers`]
//!   flags `.display()`, `to_string_lossy()`, `to_str()` and a path to each outside the
//!   helpers, and a message's `Debug` capture, named or positional; it skips test items
//!   and escaped braces, and reads the code after them;
//!   [`the_cause_guard_flags_an_error_shown_with_its_paths`] flags a raw, a stringified, a
//!   Debug-captured and a `safe_inline`-escaped error, one hoisted into a `let`, one in a
//!   `message:` field, a warning, a nested `format!`, a `write!` and `io_error`, each binder
//!   shape, and an unescaped cause; and
//!   [`the_watch_label_guard_flags_a_directory_named_any_other_way`] flags a watched
//!   directory escaped as it is, captured, displayed or relabelled.
//! - **Negative:** [`cli_print_sites_sanitize_every_interpolated_value`],
//!   [`the_stderr_writer_fn_is_called_only_by_the_writer_macros`],
//!   [`no_raw_print_macro_outside_the_writer`], [`process_exit_only_in_the_funnel`],
//!   [`terminal_streams_are_opened_only_by_the_writers`] and the three path-sink rules
//!   prove the real sources are clean. The path-sink rules also count what they found:
//!   each listed helper's exact calls, at least [`MESSAGE_SINK_FLOOR`] message sinks and
//!   [`CAUSE_FLOOR`] causes, and exactly [`WATCH_FAILURE_SITES`] watch-failure messages.
//! - **Non-vacuity:** the same test asserts the scanner actually found the crate's
//!   modules, its print sites (crate-wide, and per file for the files in
//!   [`SITE_FLOORS`]), its interpolations, its `let` bindings, the non-`let` binders that
//!   poison a name, and its calls into the sanitizing print helpers, so it cannot pass
//!   because the parser silently returned nothing. The three bans read every module the
//!   crate declares, the raw-print ban saw the writer calls, the exit ban found each
//!   funnel holding exactly its one `process::exit`, and the stream-handle ban found each
//!   owner holding its handle.
//! - **Allowlist rot:** [`every_allowlist_entry_is_live`] fails if an entry in either
//!   allowlist stops matching anything, so exemptions cannot outlive the code that
//!   needed them. [`PATH_TEXT_HELPERS`] and [`PATH_FREE_ERRORS`] carry exact counts,
//!   checked by their rules in both directions.

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
    ("lint.rs", 1),
    ("lint_sink.rs", 12),
    ("main.rs", 11),
    // 4 since the write primitive and its tests moved to `write.rs` (#160).
    ("output.rs", 4),
    // 16 since one function removes a deleted source's output, and reports it, for both
    // kinds of directory batch (#160).
    ("watch.rs", 16),
    // The skip notices of the write primitive's tests, moved from `output.rs` (#160).
    ("write.rs", 3),
];

/// Files that print but have no [`SITE_FLOORS`] entry yet, each with the reason.
///
/// [`every_printing_file_has_a_site_floor`] fails on a printing file listed in neither
/// place, and on an entry here for a file that has a floor or no print site, so the
/// list cannot outlive its reason.
const FLOORS_PENDING: &[(&str, &str)] = &[];

/// std's print macros, and `dbg!`, which prints through `eprintln!` (#157).
///
/// Each panics when its write fails, which is how a closed or failing stream ended a run
/// with exit 101. None may appear anywhere in `crates/mds-cli/src/**` — see
/// [`no_raw_print_macro_outside_the_writer`].
const RAW_PRINT_MACROS: &[&str] = &["println", "print", "eprintln", "eprint", "dbg"];

/// The functions allowed to call `std::process::exit`, by file: the CLI's exit funnel,
/// which applies the output-failure rule to every exit code (#157), and the panic path's
/// own way out, for a panic that cannot return to it — nothing catches it, or it comes
/// while its thread unwinds from another — which exits 101 at once (#389).
const EXIT_FUNNELS: &[(&str, &str)] = &[("output.rs", "exit"), ("output.rs", "exit_after_panic")];

/// The `std::process` functions that end the process: `exit` only through a funnel,
/// `abort` never.
const PROCESS_ENDERS: &[&str] = &["exit", "abort"];

/// The std functions that hand out a terminal stream.
const STREAM_HANDLES: &[&str] = &["stdout", "stderr"];

/// The functions allowed to name a [`STREAM_HANDLES`] function for anything but an
/// `.is_terminal()` query, by file: the two writers, which keep the state every write
/// consults — a pipe closed for good, a failure already reported (#157) — the exit
/// after clap's own output, which flushes what clap printed, and the panic hook, which
/// writes its one fixed text with a `write_all` of its own: a panic may come from inside
/// the writer, and the text is written whatever the writer's state says (#389). One unit
/// test is listed as well: it holds stderr's lock, writing nothing, so that the hook's
/// write waits, and checks that the panic is recorded all the same (#389).
const STREAM_HANDLE_OWNERS: &[(&str, &str)] = &[
    ("output.rs", "write_stderr_fmt"),
    ("output.rs", "write_stdout"),
    ("main.rs", "exit_after_clap_output"),
    ("output.rs", "on_panic"),
    ("output.rs", "a_panic_is_recorded_before_the_hook_writes"),
];

/// The calls through which `mds lint` shows a result, by name: the writer macros, the
/// stdout path, and the error renderers that write a whole diagnostic to stderr. They
/// belong in its result sink, `lint_sink.rs`, never in `lint.rs` — see
/// [`lint_output_goes_through_the_sink`].
const LINT_OUTPUT_CALLS: &[&str] = &[
    "ewriteln!",
    "ewrite!",
    "write_stdout",
    "eprint_error",
    "eprint_io_failure",
];

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
        "lint_sink.rs",
        "STDIN_DISPLAY_LABEL",
        "`&'static str` compile-time constant defined in `output.rs` as `\"<stdin>\"`. \
         It is the uniform stdin source-identity sentinel (AD-211-3 / issue #211); \
         it contains only ASCII printable characters and cannot carry hostile bytes.",
    ),
    (
        "fmt.rs",
        "STDIN_DISPLAY_LABEL",
        "Same `output.rs` constant as the `lint_sink.rs` entry above — `mds fmt -`'s \
         `Would reformat:` status line names the source with the shared sentinel \
         instead of its own literal (AD-211-3).",
    ),
    (
        "main.rs",
        "STDIN_DISPLAY_LABEL",
        "Same `output.rs` constant as the `lint_sink.rs` entry above — `mds check -`'s \
         `OK:` status line names the source with the shared sentinel instead of its \
         own literal (AD-211-3).",
    ),
    (
        "lint_sink.rs",
        "applied_count",
        "`usize` tally of lint fixes actually applied, in the `Partially fixed:` line.",
    ),
    (
        "lint_sink.rs",
        "total_count",
        "`usize` tally of lint fixes planned, in the `Partially fixed:` line.",
    ),
    (
        "lint_sink.rs",
        "mds::MAX_DIAGNOSTICS",
        "`usize` compile-time constant `mds::MAX_DIAGNOSTICS` (the per-file diagnostic cap).",
    ),
    (
        "lint_sink.rs",
        "cap_advice",
        "`&'static str` bound in `cap_reached` from a `match` over `CapNotice` whose two \
         arms are string literals: empty, or the fixed ASCII-and-em-dash advice to re-run \
         `--fix` (#309). It carries no input text.",
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
    // Four counters for the `mds lint <dir>` summary line. Their names are file-unique
    // (limit 2) — none collide with the other lint_sink.rs entries; no future variable
    // silently inherits an exemption by reusing an already-listed name.
    (
        "lint_sink.rs",
        "clean_count",
        "`usize` tally of files with no lint findings in the `mds lint <dir>` summary line.",
    ),
    (
        "lint_sink.rs",
        "warn_file_count",
        "`usize` tally of files with warning-severity findings in the `mds lint <dir>` summary line.",
    ),
    (
        "lint_sink.rs",
        "error_file_count",
        "`usize` tally of files with error-severity findings or analysis failures \
         in the `mds lint <dir>` summary line.",
    ),
    (
        "lint_sink.rs",
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

// ── Path sinks (#390) ─────────────────────────────────────────────────────────

/// The ways std turns a path into text: `display` and `to_string_lossy` show it as
/// `Display` prints it, and `to_str` gives its text when it is UTF-8. None of them is the
/// form the user typed, escaped.
const PATH_TEXT_METHODS: &[&str] = &["display", "to_string_lossy", "to_str"];

/// The functions allowed to call a [`PATH_TEXT_METHODS`] method (#390), by file, with
/// the exact number of calls in the function's body and the reason.
///
/// Outside these bodies no such method may appear in the crate's code, its
/// `#[cfg(test)]` items aside ([`paths_become_text_only_in_the_listed_functions`]). The
/// count is exact, so a new call inside a listed function fails the guard just as one
/// elsewhere does, and an entry whose function lost its calls (or was renamed) fails it
/// too. Most entries are escape helpers: the lossy text goes straight into
/// `mds::escape_path_for_message`, or the path through `safe_path`. Others compare a
/// name or an extension, as text, with a fixed value, and show it nowhere. The rest are
/// the carve-outs, each saying why it shows or keeps a path some other way.
const PATH_TEXT_HELPERS: &[(&str, &str, usize, &str)] = &[
    (
        "output.rs",
        "safe_path",
        1,
        "The escape helper every path in a message goes through: it strips a Windows \
         verbatim prefix, then escapes every forbidden path character.",
    ),
    (
        "output.rs",
        "reject_forbidden_output_path",
        1,
        "Scans the typed value lossily for a forbidden path character, as mds-core does; \
         its refusal names the value through core's escape.",
    ),
    (
        "output.rs",
        "reject_non_utf8_output_path",
        2,
        "`to_str` is the UTF-8 test itself; a value that fails it is named through \
         `mds::escape_path_for_message` of its lossy text, the form a message can carry.",
    ),
    (
        "output.rs",
        "reject_forbidden_resolved_output_path",
        1,
        "Hands the typed value, lossy, to `mds::reject_forbidden_path`, which escapes it \
         in its refusal; the resolved path is scanned, never shown.",
    ),
    (
        "output.rs",
        "payload",
        1,
        "Carve-out: the debug-build test trigger of the panic hook builds a payload that \
         names the working directory on purpose, and `tests/panic_hook.rs` pins that the \
         hook never shows it.",
    ),
    (
        "output.rs",
        "panic_on_compile",
        1,
        "Compares a file stem with the debug-build panic trigger's value; the stem \
         reaches no message.",
    ),
    (
        "output.rs",
        "is_within_default_excluded_dir",
        1,
        "Compares each directory name above a file with the default exclusions; the \
         name reaches no message.",
    ),
    (
        "output.rs",
        "collect_mds_files_inner",
        2,
        "Compares a walked directory's name with the default exclusions and a file's \
         extension with `mds`; neither reaches a message.",
    ),
    (
        "output.rs",
        "count_mds_in_excluded_dir",
        1,
        "Compares a file's extension with `mds` to count it; the extension reaches no \
         message.",
    ),
    (
        "output.rs",
        "is_partial",
        1,
        "Checks whether a file name starts with `_`; the name reaches no message.",
    ),
    (
        "build.rs",
        "warn_output_extension_mismatch",
        1,
        "Takes the extension of the `-o` value, which the warning shows through \
         `safe_inline` beside the value itself.",
    ),
    (
        "build.rs",
        "ensure_existing_mds_file",
        2,
        "Binds the lossy typed path and escapes it at once \
         (`mds::escape_path_for_message`); every message in the function names that. \
         `to_str` compares the extension with `mds`.",
    ),
    (
        "build.rs",
        "auto_detect_mds_file",
        1,
        "Compares the extension of each entry of the working directory with `mds`; the \
         extension reaches no message.",
    ),
    (
        "build.rs",
        "several_mds_files",
        1,
        "Takes the file name of each `.mds` file in the working directory and escapes it \
         at once through `safe_file_display`; the message lists those.",
    ),
    (
        "build.rs",
        "refuse_output_over_entry",
        1,
        "Names the typed entry through `mds::escape_path_for_message` of its lossy text.",
    ),
    (
        "build.rs",
        "apply_source_map_file_label",
        1,
        "Carve-out: the source map's `file` key, the output's file name. A source map's \
         paths are the spec's named exception to escaping (not a diagnostic), and this is \
         no message.",
    ),
    (
        "build.rs",
        "file_name_of",
        1,
        "Carve-out: the file name a sidecar map records as `file`, compared with the map \
         on disk before it is replaced; no message shows it.",
    ),
    (
        "build.rs",
        "vars_file_error",
        1,
        "Names the typed `--vars` path through `mds::escape_path_for_message` of its \
         lossy text when it is a symlink, for every command that takes the flag.",
    ),
    (
        "input.rs",
        "refusal",
        1,
        "Names the typed directory argument through `mds::escape_path_for_message` of \
         its lossy text.",
    ),
    (
        "lint.rs",
        "file",
        2,
        "`to_str` takes the file name a typed file argument's diagnostics carry, escaped \
         where they are shown; an argument with none is named through \
         `mds::escape_path_for_message` of its lossy text.",
    ),
    (
        "lint.rs",
        "relative_display",
        1,
        "`to_str` takes each walked name below the root to build the entry's key, escaped \
         where it is shown; both messages name their paths through `safe_path`.",
    ),
    (
        "lint.rs",
        "read_canonical_source",
        2,
        "`to_str` takes the canonical path to read it, and `display` hands its directory, \
         lossless once that passed, to `anchor_base_dir`: arguments, not message text. \
         The message names the path through `safe_path`.",
    ),
    (
        "watch.rs",
        "moved",
        1,
        "Names the typed watched path through `mds::escape_path_for_message` of its \
         lossy text when it resolves somewhere new.",
    ),
    (
        "watch.rs",
        "handle_fs_event_dir",
        1,
        "Compares a changed path's extension with `mds`; the extension reaches no \
         message.",
    ),
];

/// The functions that turn an io, notify or tempfile error into the cause a message shows,
/// with every path it carries dropped (`output.rs`, #390). An error reaches a message
/// only as one of these, escaped: `safe_inline(io_cause(&e))`. Bare, it reaches only
/// [`CAUSE_ESCAPING_CALLS`], which escape the cause themselves.
const CAUSE_PRODUCERS: &[&str] = &["io_cause", "notify_cause"];

/// The fewest message sinks the path-sink rule must find across mds-cli's sources, so it
/// cannot pass because the scan read nothing.
const MESSAGE_SINK_FLOOR: usize = 150;

/// The fewest causes the path-sink rule must find shown through [`CAUSE_PRODUCERS`]: 19
/// since every write failure, and every removal's, is worded once, in one place (#160).
const CAUSE_FLOOR: usize = 19;

/// Names taken to hold an error wherever they appear. [`error_names`] adds every name a
/// file binds to one.
const ERROR_NAMES: &[&str] = &["e", "err", "error"];

/// Macros whose arguments become a message's text. For `write!` / `writeln!` the first
/// argument is the sink, not text.
const MESSAGE_MACROS: &[&str] = &[
    "format!",
    "miette!",
    "ewriteln!",
    "ewrite!",
    "writeln!",
    "write!",
];

/// Calls whose every argument is message text: `eprint_warning`'s warning, and the write
/// primitive's `io_error` (`write.rs`), which takes the path it names and the cause it shows
/// after it.
const MESSAGE_CALLS: &[&str] = &["eprint_warning", "io_error"];

/// The [`MESSAGE_CALLS`] that pass their cause through `safe_inline` themselves, so a
/// bare [`CAUSE_PRODUCERS`] call is their accepted argument.
const CAUSE_ESCAPING_CALLS: &[&str] = &["io_error"];

/// The struct field an error's message is built in (`MdsError::Io { message }`).
const MESSAGE_FIELD: &str = "message";

/// Error values a message interpolates other than as a cause, by file and normalized
/// expression, with the exact number of sites and the reason each names no path.
///
/// [`messages_interpolate_an_error_only_as_its_cause`] fails on a count that differs, in
/// either direction, so an entry cannot outlive its sites or quietly cover a new one.
const PATH_FREE_ERRORS: &[(&str, &str, usize, &str)] = &[
    (
        "build.rs",
        "crate::output::safe_inline(&e)",
        3,
        "Escaped, and none names a path: a `FromUtf8Error` reading `mds.json` (a byte \
         count and an offset), a `serde_json` error parsing it (a message, a line and a \
         column), and one serializing `messages` output (fixed text).",
    ),
    (
        "lint.rs",
        "e",
        3,
        "A `miette::Report` from `build::load_config`, turned into an `MdsError` for the \
         lint sinks: text this crate wrote, which names `mds.json` through `safe_path` \
         and its cause through `io_cause` or a path-free `serde_json` / UTF-8 error.",
    ),
    (
        "lint_sink.rs",
        "report.to_string()",
        1,
        "A `miette::Report` that lint's setup failed with and that is not an `MdsError`, \
         turned into one for the JSON error document: `auto_detect_mds_file`'s, text this \
         crate wrote — no `.mds` file in the working directory, or the file names found \
         there, each escaped through `safe_file_display` — naming no path.",
    ),
    (
        "output.rs",
        "not_removed.cause()",
        1,
        "A `write::NotRemoved`'s cause, escaped where it is made: an io error through \
         `io_cause`, a fixed refusal, or one naming the refused directory as shown \
         through `safe_path` (#160).",
    ),
    (
        "watch.rs",
        "safe_inline(not_removed.cause())",
        2,
        "A `write::NotRemoved`'s cause, escaped where it is made — an io error through \
         `io_cause`, a fixed refusal, or one naming the refused directory as shown \
         through `safe_path` — and escaped again, which changes nothing: the warning that \
         a deleted source's output, and the one that the other kind's output after a \
         change of kind, could not be removed (#160).",
    ),
];

/// The text a message about a directory the file watcher refused begins with.
const WATCH_FAILURE_TEXT: &str = "failed to watch";

/// The function that names a watched directory as shown in that message (`watch.rs`):
/// below the entry's directory or the root as typed (#390).
const WATCH_FAILURE_LABEL: &str = "shown_watched_dir";

/// The [`WATCH_FAILURE_TEXT`] messages in `watch.rs`: the rebuild-time re-arm, file and
/// directory mode's `failed to watch directory`, `failed to watch vars directory` and
/// `failed to watch external dep dir`. Changing it is a decision made in the commit that
/// adds or removes a site.
const WATCH_FAILURE_SITES: usize = 5;

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

    // Non-vacuity only: the scan found the registered writer macros at all, so the ban
    // below cannot pass by reading nothing (a broken module walk, a masking bug, a
    // renamed macro). It is not a coverage count. Which files print through the writer
    // is guarded file by file, by `SITE_FLOORS` in
    // `cli_print_sites_sanitize_every_interpolated_value`. So this number sits well
    // under the crate's real count, and a refactor that removes duplicated print sites
    // must never have to keep them to satisfy it.
    assert!(
        writer_calls >= 60,
        "non-vacuity: expected at least 60 `ewriteln!` / `ewrite!` calls across \
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

/// A stream handle is a second way to write: a helper holding its own
/// `std::io::stdout()` would not see a stdout closed for good or a failure already
/// reported — the duplicate `write_stdout` `mds lint` had (#157). So only the functions in
/// [`STREAM_HANDLE_OWNERS`] name `stdout` or `stderr`; any other code may only ask
/// `.is_terminal()`.
#[test]
fn terminal_streams_are_opened_only_by_the_writers() {
    let mut stray: Vec<String> = Vec::new();
    let mut owners_found: Vec<String> = Vec::new();
    for (name, src) in crate_sources() {
        let owners: Vec<&str> = STREAM_HANDLE_OWNERS
            .iter()
            .filter(|(file, _)| *file == name)
            .map(|(_, function)| *function)
            .collect();
        let handles = stream_handle_mentions(&src, &owners);
        owners_found.extend(
            owners
                .iter()
                .zip(&handles.in_owner)
                .filter(|(_, count)| **count > 0)
                .map(|(owner, _)| format!("{name}::{owner}")),
        );
        stray.extend(
            handles
                .stray
                .into_iter()
                .map(|line| format!("  {name}:{line}")),
        );
    }

    // Non-vacuity: each owner was found, holding its handle.
    assert_eq!(
        owners_found.len(),
        STREAM_HANDLE_OWNERS.len(),
        "non-vacuity: each of {STREAM_HANDLE_OWNERS:?} must name a stream handle; found \
         {owners_found:?}"
    );
    assert!(
        stray.is_empty(),
        "print-discipline violation: a terminal stream is named outside \
         {STREAM_HANDLE_OWNERS:?} for more than an `.is_terminal()` query:\n{}\n\n\
         Write status lines through `crate::output::ewriteln!` / `ewrite!` and a command's \
         product through `crate::output::write_stdout`.",
        stray.join("\n")
    );
}

/// `mds lint` shows its results only through its result sink (#309): lint's writer-macro
/// calls, stdout writes and error renderers ([`LINT_OUTPUT_CALLS`]) appear in
/// `lint_sink.rs`, never in `lint.rs`, and neither file ends the process — `lint::run_lint`
/// returns the exit code, and `main`, the driver, exits with it. A print or an exit
/// written back into `lint.rs` would bypass the order `lint::render` decides and the exit
/// code the run returns.
///
/// `lint.rs` keeps one print on purpose, `mds.json`'s unknown-rule warning, which it shows
/// through `eprint_warning` while the config loads; the sanitizer scan above covers it.
/// Calls are matched by name, as in the other scans here.
#[test]
fn lint_output_goes_through_the_sink() {
    let sources = crate_sources();
    let source = |name: &str| -> &str {
        sources
            .iter()
            .find(|(file, _)| file == name)
            .map(|(_, src)| src.as_str())
            .unwrap_or_else(|| panic!("{name} must be one of the crate's modules"))
    };
    let (lint, sink, main) = (source("lint.rs"), source("lint_sink.rs"), source("main.rs"));

    // Non-vacuity: the scan read lint's module, found the sink's status lines and its
    // stdout path, and found the driver ending the run with lint's code.
    assert!(
        fn_body(&mask_comments(lint), "run_lint").is_some(),
        "non-vacuity: lint.rs must define `run_lint`"
    );
    let funnel = lint_funnel(lint, sink, main);
    assert!(
        funnel.sink_writer_calls >= 10,
        "non-vacuity: expected lint's status lines in lint_sink.rs (at least 10 writer-macro \
         calls), found {}",
        funnel.sink_writer_calls
    );
    assert!(
        funnel.sink_stdout_writes >= 1,
        "non-vacuity: expected lint's stdout path in lint_sink.rs"
    );
    assert_eq!(
        funnel.driver_exits, 1,
        "non-vacuity: main.rs must end an `mds lint` run with `exit(lint::run_lint(…))`"
    );
    assert!(
        funnel.stray.is_empty(),
        "print-discipline violation: `mds lint` prints or exits outside its result sink:\n{}\n\n\
         Show the result through a `ResultSink` method in lint_sink.rs, and return the exit \
         code from `run_lint`.",
        funnel.stray.join("\n")
    );

    // Positive controls: each call planted in lint.rs is reported, and so is an exit
    // planted in lint_sink.rs.
    for plant in [
        "crate::output::ewriteln!(\"planted\");",
        "let _ = crate::output::write_stdout(b\"planted\");",
        "crate::output::eprint_error(miette::miette!(\"planted\"));",
        "crate::output::exit(2);",
    ] {
        let planted = format!("{lint}\nfn planted() {{ {plant} }}\n");
        let found = lint_funnel(&planted, sink, main).stray;
        assert_eq!(
            found.len(),
            1,
            "a `{plant}` planted in lint.rs must be reported; got {found:?}"
        );
    }
    let exit_in_sink = format!("{sink}\nfn planted() {{ crate::output::exit(2); }}\n");
    let found = lint_funnel(lint, &exit_in_sink, main).stray;
    assert_eq!(
        found.len(),
        1,
        "an exit planted in lint_sink.rs must be reported; got {found:?}"
    );
    // …and a driver that stops exiting with lint's code is not counted.
    let unfunnelled = main.replacen("output::exit(lint::run_lint(", "(lint::run_lint(", 1);
    assert_ne!(
        unfunnelled, main,
        "precondition: main.rs exits with `lint::run_lint`'s code"
    );
    assert_eq!(lint_funnel(lint, sink, &unfunnelled).driver_exits, 0);

    // Negative control: a comment and a string literal are not calls.
    let prose = format!(
        "{lint}\n// crate::output::exit(2);\nconst PLANTED: &str = \"ewriteln!(\\\"x\\\")\";\n"
    );
    let found = lint_funnel(&prose, sink, main).stray;
    assert!(
        found.is_empty(),
        "a comment or a literal is not a call; got {found:?}"
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
    // import, a braced import, a glob, and a renamed `process` — directly, as a braced
    // `self`, and after another `process` path in the same braces.
    let bypasses = r#"
        fn bail() { std::process::exit(1); }
        fn out() { process::exit(2) }
        fn crash() { ::std::process::abort() }
        use std::process::exit as quit;
        use std::process::{self, abort};
        use std::process::*;
        use std::process as p;
        use std::{process::exit};
        use std::process::{self as pr, Command};
        use std::{process::Command, process as sp};
    "#;
    assert_eq!(
        process_end_mentions(bypasses, &["exit"]),
        ProcessEnds {
            stray: vec![2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
            in_funnel: 0
        }
    );
    assert_eq!(
        process_end_mentions(funnel, &[]).stray,
        vec![4],
        "the funnel body counts as the funnel only in the file that lists it"
    );

    // Two funnels in one file: each holds its one exit, and an exit in any other function
    // of the file — a name that merely starts with a funnel's included — is still stray.
    let two_funnels = r#"
        pub(crate) fn exit(verdict: i32) -> ! {
            std::process::exit(final_exit_code(verdict))
        }
        fn exit_after_panic() -> ! {
            std::process::exit(PANIC_EXIT)
        }
        fn exit_after_panic_twice() -> ! {
            std::process::exit(PANIC_EXIT)
        }
    "#;
    assert_eq!(
        process_end_mentions(two_funnels, &["exit", "exit_after_panic"]),
        ProcessEnds {
            stray: vec![9],
            in_funnel: 2
        }
    );
    assert_eq!(
        process_end_mentions(two_funnels, &["exit"]),
        ProcessEnds {
            stray: vec![6, 9],
            in_funnel: 1
        },
        "a panic path's exit is stray until it is listed as a funnel"
    );

    // Neither: the funnel's callers, other `std::process` items, a plain module import,
    // names that merely contain the words, comments and literals.
    let prose = r#"
        use std::process;
        use std::process::{Command, Stdio};
        use std::process::{self, Command};
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
fn the_stream_handle_guard_flags_a_second_writer() {
    // Reported, each on its own line: stdout and stderr outside the owners, however
    // qualified, a `use` of either, renamed or braced, a handle bound before it is queried,
    // and an owner's body in a file that lists no owner.
    let stray = r#"
        fn emit(bytes: &[u8]) { let _ = std::io::stdout().write_all(bytes); }
        fn warn(text: &str) { let _ = ::std::io::stderr().lock().write_all(text.as_bytes()); }
        use std::io::stdout as out;
        use std::io::{stderr, Write};
        fn tty() -> bool { let s = io::stdout(); s.is_terminal() }
        fn write_stdout(bytes: &[u8]) { let _ = std::io::stdout().write_all(bytes); }
    "#;
    assert_eq!(
        stream_handle_mentions(stray, &[]),
        StreamHandles {
            stray: vec![2, 3, 4, 5, 6, 7],
            in_owner: Vec::new()
        }
    );

    // Accepted: an owner's own handle, and an `.is_terminal()` query anywhere.
    let owned = r#"
        fn write_stdout(bytes: &[u8]) -> Outcome {
            write_stdout_to(&STATE, &mut std::io::stdout().lock(), bytes)
        }
        fn clear() { if std::io::stderr().is_terminal() { ewrite!("x"); } }
        fn tty() -> bool { std::io::stdout()
            .is_terminal() }
    "#;
    assert_eq!(
        stream_handle_mentions(owned, &["write_stdout"]),
        StreamHandles {
            stray: Vec::new(),
            in_owner: vec![1]
        }
    );

    // The panic hook's handle, bound to a local of the stream's own name: every mention
    // in the owner's body counts for it, and the same body is stray until it is listed.
    let hook = r#"
        fn on_panic(info: &PanicHookInfo<'_>) {
            let mut stderr = std::io::stderr();
            let _ = stderr.write_all(ICE_TEXT.as_bytes());
        }
    "#;
    assert_eq!(
        stream_handle_mentions(hook, &["write_stdout", "on_panic"]),
        StreamHandles {
            stray: Vec::new(),
            in_owner: vec![0, 3]
        }
    );
    assert_eq!(
        stream_handle_mentions(hook, &["write_stdout"]).stray,
        vec![3, 3, 4],
        "the hook's handle is stray until the hook is listed as an owner"
    );

    // Neither: a field or method of that name, a longer identifier, a definition,
    // comments and literals.
    let prose = r#"
        fn f(child: &mut Child, err: &clap::Error, state: &State) {
            let _ = child.stdout.take();
            let _ = err.use_stderr();
            let _ = state.stdout_closed();
            let _ = "std::io::stdout().write_all(b)";
            // std::io::stderr().write_all(b);
        }
        fn stdout() {}
    "#;
    assert_eq!(
        stream_handle_mentions(prose, &[]),
        StreamHandles {
            stray: Vec::new(),
            in_owner: Vec::new()
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

// ── Path sinks (#390) ─────────────────────────────────────────────────────────

/// A path reaches a message as the user typed it, escaped, through the escape helpers
/// (#390). std's text of a path is neither: it shows a canonical or absolute path as
/// such, and a forbidden character raw. So the [`PATH_TEXT_METHODS`] are called only
/// inside the functions [`PATH_TEXT_HELPERS`] lists, exactly as often as it says, and no
/// message's format string asks for `Debug` ([`debug_captures`]), in the crate's code
/// with its `#[cfg(test)]` items left out.
#[test]
fn paths_become_text_only_in_the_listed_functions() {
    let sources = crate_sources();
    let mut stray: Vec<String> = Vec::new();
    let mut found = 0usize;
    for (name, src) in &sources {
        let listed: Vec<&(&str, &str, usize, &str)> = PATH_TEXT_HELPERS
            .iter()
            .filter(|(file, ..)| *file == name.as_str())
            .collect();
        let helpers: Vec<&str> = listed.iter().map(|(_, function, ..)| *function).collect();
        let texts = path_text_mentions(src, &helpers);
        stray.extend(texts.stray.iter().map(|line| format!("  {name}:{line}")));
        stray.extend(
            debug_captures(src)
                .iter()
                .map(|line| format!("  {name}:{line}: a message formats a value with Debug")),
        );
        for ((_, function, count, _), calls) in listed.iter().zip(&texts.in_helper) {
            found += calls.unwrap_or(0);
            if *calls != Some(*count) {
                stray.push(format!(
                    "  {name}::{function}: listed with {count} call(s), found {calls:?} \
                     (None: no such function in the file's code)"
                ));
            }
        }
    }

    for (file, function, _, why) in PATH_TEXT_HELPERS {
        assert!(
            sources.iter().any(|(name, _)| name == file),
            "PATH_TEXT_HELPERS names {file}, which is not a source of the crate"
        );
        assert!(
            why.len() >= 40,
            "PATH_TEXT_HELPERS entry {file}::{function} needs a real reason, got {why:?}"
        );
    }
    // Non-vacuity: the scan found the listed calls, so it cannot pass by reading nothing.
    let listed_total: usize = PATH_TEXT_HELPERS.iter().map(|(_, _, n, _)| n).sum();
    assert!(
        found >= 25 && listed_total >= 25,
        "non-vacuity: expected at least 25 path-to-text calls in the listed functions, \
         found {found} of {listed_total} listed"
    );
    assert!(
        stray.is_empty(),
        "path-sink violation: a path is turned into text ({PATH_TEXT_METHODS:?}) outside \
         the functions PATH_TEXT_HELPERS lists, a listed count no longer holds, or a \
         message formats a value with Debug:\n{}\n\n\
         Name a path in a message through `crate::output::safe_path` (or \
         `safe_file_display`), which shows it as typed and escaped. A function that must \
         turn a path into text for another reason goes in PATH_TEXT_HELPERS with its \
         exact count and why.",
        stray.join("\n")
    );
}

/// io, notify and tempfile errors name paths in their own text, absolute ones included —
/// tempfile's `at path "…"`, notify's ` about [...]` (#390). A message shows such an error
/// only as its cause, with the paths dropped and escaped: `safe_inline(io_cause(&e))`,
/// `safe_inline(notify_cause(&e))`, or bare as the cause the write primitive's `io_error`
/// escapes itself. [`PATH_FREE_ERRORS`] lists the error values a message shows otherwise,
/// each with its exact site count and why it names no path.
#[test]
fn messages_interpolate_an_error_only_as_its_cause() {
    let sources = crate_sources();
    let mut violations: Vec<String> = Vec::new();
    // `(file, expression, line)` of every value a PATH_FREE_ERRORS entry names.
    let mut exempt: Vec<(String, String, usize)> = Vec::new();
    let mut sinks = 0usize;
    let mut causes = 0usize;
    for (name, src) in &sources {
        let values = message_values(src);
        sinks += values.sinks;
        causes += values.causes;
        for (line, sink, expr) in values.raw {
            let listed = PATH_FREE_ERRORS
                .iter()
                .any(|(file, listed, ..)| *file == name.as_str() && *listed == expr);
            if listed {
                exempt.push((name.clone(), expr, line));
            } else {
                violations.push(format!("  {name}:{line}: {sink} interpolates `{expr}`"));
            }
        }
    }
    for (file, expr, count, why) in PATH_FREE_ERRORS {
        assert!(
            why.len() >= 40,
            "PATH_FREE_ERRORS entry {file} `{expr}` needs a real reason, got {why:?}"
        );
        let lines: Vec<usize> = exempt
            .iter()
            .filter(|(name, seen, _)| name == file && seen == expr)
            .map(|(_, _, line)| *line)
            .collect();
        if lines.len() != *count {
            violations.push(format!(
                "  PATH_FREE_ERRORS {file} `{expr}`: listed for {count} site(s), found {} \
                 at lines {lines:?}",
                lines.len()
            ));
        }
    }

    // Non-vacuity: the scan read the crate's messages and recognised the causes in them.
    assert!(
        sinks >= MESSAGE_SINK_FLOOR,
        "non-vacuity: expected at least {MESSAGE_SINK_FLOOR} message sinks across \
         mds-cli/src, found {sinks}"
    );
    assert!(
        causes >= CAUSE_FLOOR,
        "non-vacuity: expected at least {CAUSE_FLOOR} causes shown through \
         {CAUSE_PRODUCERS:?}, found {causes}"
    );
    assert!(
        violations.is_empty(),
        "path-sink violation: a message shows an error with whatever paths it carries:\n\
         {}\n\nShow an io, notify or tempfile error as its cause: \
         `safe_inline(io_cause(&e))` / `safe_inline(notify_cause(&e))` \
         (crates/mds-cli/src/output.rs). An error whose text names no path goes in \
         PATH_FREE_ERRORS with its exact site count and why.",
        violations.join("\n")
    );
}

/// `shown_watched_dir` names a directory the file watcher refused below the entry's
/// directory or the root, as typed (#390). Most of these messages have no local vector —
/// notify must refuse a directory that exists — so a site that named the directory any
/// other way would pass every behavioural test. Every message whose format string says
/// `failed to watch` therefore fills its first placeholder with
/// `safe_path(&shown_watched_dir(…))`; [`WATCH_FAILURE_SITES`] fixes how many there are.
/// What `shown_watched_dir` returns is pinned by its own unit test in `watch.rs`.
#[test]
fn every_watch_failure_names_its_directory_as_shown() {
    let mut sites: Vec<(String, usize, bool)> = Vec::new();
    let mut texts = 0usize;
    for (name, src) in crate_sources() {
        texts += product_code(&src).matches(WATCH_FAILURE_TEXT).count();
        sites.extend(
            watch_failure_sites(&src)
                .into_iter()
                .map(|(line, named)| (name.clone(), line, named)),
        );
    }

    assert_eq!(
        sites.len(),
        WATCH_FAILURE_SITES,
        "non-vacuity: expected {WATCH_FAILURE_SITES} `{WATCH_FAILURE_TEXT}` messages, found \
         {sites:?}. Adding or removing one changes WATCH_FAILURE_SITES in the same commit."
    );
    assert_eq!(
        texts,
        sites.len(),
        "the text `{WATCH_FAILURE_TEXT}` appears in the crate's code outside a message's \
         format string, where this check cannot see which directory it names: {sites:?}"
    );
    let bypassing: Vec<String> = sites
        .iter()
        .filter(|(_, _, named)| !named)
        .map(|(name, line, _)| format!("  {name}:{line}"))
        .collect();
    assert!(
        bypassing.is_empty(),
        "path-sink violation: a `{WATCH_FAILURE_TEXT}` message names its directory other \
         than through `safe_path(&{WATCH_FAILURE_LABEL}(…))`:\n{}",
        bypassing.join("\n")
    );
}

#[test]
fn the_path_text_guard_flags_a_path_shown_outside_the_helpers() {
    // Reported, each on its own line: `.display()` in a message, `to_string_lossy()`, a
    // path to the method called or passed, a call whose dot is on the line above, and
    // `to_str` called or passed. Product code after a test item, a test-only field
    // included, is still read.
    let src = r#"
        fn label(p: &Path) -> String { safe_path(p) }
        fn warn(p: &Path) { eprint_warning(&format!("cannot read {}", p.display())); }
        fn lossy(p: &Path) -> String { p.to_string_lossy().into_owned() }
        fn ufcs(p: &Path) -> String { format!("{}", Path::display(p)) }
        fn mapped(ps: &[PathBuf]) { let _ = ps.iter().map(Path::display); }
        fn chained(p: &Path) -> String { p
            .display()
            .to_string() }
        fn safe_path(p: &Path) -> String { safe_file_display(&p.display().to_string()) }
        fn display(display: &str) -> String { display.to_string() }
        // p.display()
        const NOTE: &str = "p.display()";
        #[cfg(test)]
        mod tests {
            fn t(p: &Path) { let _ = p.display(); }
        }
        #[cfg(test)]
        fn only_in_tests(p: &Path) -> String { p.to_string_lossy().into_owned() }
        fn after(p: &Path) -> String { p.display().to_string() }
        fn utf8(p: &Path) -> &str { p.to_str().unwrap() }
        fn named(p: &Path) -> Option<&str> { p.file_name().and_then(std::ffi::OsStr::to_str) }
        fn debugged(p: &Path) -> String { format!("cannot read {p:?}") }
        fn positional(p: &Path) { ewriteln!("cannot read {:#?}", p); }
        fn braces(p: &Path) -> String { format!("{{:?}} {}", safe_path(p)) }
        #[cfg(test)]
        fn debug_in_tests(p: &Path) -> String { format!("{p:?}") }
        struct Probe {
            #[cfg(test)]
            calls: Vec<String>,
            shown: String,
        }
        fn after_field(p: &Path) -> String { p.display().to_string() }
        struct Last { shown: String, #[cfg(test)] calls: Vec<String> }
        fn after_last(p: &Path) -> String { p.to_string_lossy().into_owned() }
    "#;
    assert_eq!(
        path_text_mentions(src, &["safe_path"]),
        PathTexts {
            stray: vec![3, 4, 5, 6, 8, 20, 21, 22, 33, 35],
            in_helper: vec![Some(1)]
        }
    );

    // The helper's own call is stray until it is listed, and a listed function the code
    // does not define is reported as missing, not as clean.
    assert_eq!(
        path_text_mentions(src, &[]).stray,
        vec![3, 4, 5, 6, 8, 10, 20, 21, 22, 33, 35]
    );
    assert_eq!(
        path_text_mentions(src, &["safe_path", "gone"]).in_helper,
        vec![Some(1), None]
    );

    // A Debug capture in a message, named or positional, is reported; escaped braces and
    // a test item are not.
    assert_eq!(debug_captures(src), vec![23, 24]);
}

#[test]
fn the_cause_guard_flags_an_error_shown_with_its_paths() {
    // Reported, each on its own line: a raw `{e}`, `e.to_string()`, `safe_inline(&e)`, a
    // Debug capture, a `message:` field, a warning, a nested `format!`, a `let` one hop
    // away, names bound by `Err(…)`, by a `map_err` closure and by an error-typed
    // parameter, a cause left unescaped, a `write!`, and `io_error` handed the error.
    let flagged = r#"
        fn read(p: &Path) -> Result<String, MdsError> {
            std::fs::read_to_string(p).map_err(|e| MdsError::Io { message: format!("cannot read: {e}") })
        }
        fn raw_method(e: std::io::Error) -> String { format!("cannot read: {}", e.to_string()) }
        fn escaped(e: std::io::Error) -> miette::Report { miette::miette!("cannot read: {}", safe_inline(&e)) }
        fn debug(err: std::io::Error) { ewriteln!("cannot read: {err:?}"); }
        fn field(e: std::io::Error) -> MdsError { MdsError::Io { message: e.to_string() } }
        fn warning(e: notify::Error) { eprint_warning(&safe_inline(&e)); }
        fn nested(e: std::io::Error) -> String { format!("{}", format!("cannot read: {e}")) }
        fn hoisted(e: std::io::Error) -> String { let msg = e.to_string(); format!("cannot read: {msg}") }
        fn matched(r: Result<(), Failure>) -> String { match r { Err(failure) => format!("{failure}"), Ok(()) => String::new() } }
        fn closure(r: std::io::Result<()>) -> Result<(), String> { r.map_err(|source| format!("cannot read: {source}")) }
        fn param(why: &std::io::Error) -> String { format!("cannot read: {}", safe_inline(why)) }
        fn unescaped(e: std::io::Error) -> String { format!("cannot read: {}", io_cause(&e)) }
        fn written(e: std::io::Error) -> String { let mut s = String::new(); let _ = write!(s, "{}", e); s }
        fn handed(e: std::io::Error) -> MdsError { io_error("cannot write", e.to_string()) }
    "#;
    let mut raw = message_values(flagged).raw;
    raw.sort();
    let expected: Vec<(usize, String, String)> = [
        (3, "format!", "e"),
        (5, "format!", "e.to_string()"),
        (6, "miette!", "safe_inline(&e)"),
        (7, "ewriteln!", "err"),
        (8, "message", "e.to_string()"),
        (9, "eprint_warning", "&safe_inline(&e)"),
        (10, "format!", "e"),
        (11, "format!", "msg"),
        (12, "format!", "failure"),
        (13, "format!", "source"),
        (14, "format!", "safe_inline(why)"),
        (15, "format!", "io_cause(&e)"),
        (16, "write!", "e"),
        (17, "io_error", "e.to_string()"),
    ]
    .into_iter()
    .map(|(line, sink, expr)| (line, sink.to_string(), expr.to_string()))
    .collect();
    assert_eq!(raw, expected);

    // Accepted: the cause, escaped, with or without module paths; the bare cause in
    // `io_error`, which escapes it, whatever the producer is handed; an `io::ErrorKind`.
    // Ignored: a test module.
    let accepted = r#"
        fn a(e: std::io::Error) -> String { format!("cannot read x: {}", safe_inline(io_cause(&e))) }
        fn b(e: std::io::Error) -> MdsError { MdsError::Io { message: format!("cannot read x: {}", crate::output::safe_inline(crate::output::io_cause(&e))) } }
        fn c(e: notify::Error) { eprint_warning(&format!("warning: {}", safe_inline(notify_cause(&e)))); }
        fn d(e: std::io::Error) -> MdsError { io_error("cannot stat", io_cause(&e)) }
        fn f(e: tempfile::PersistError) -> MdsError { io_error("cannot rename", io_cause(&e.error)) }
        fn h(e: std::io::Error) -> String { format!("kind: {}", e.kind()) }
        #[cfg(test)]
        mod tests { fn t(e: std::io::Error) -> String { format!("{e}") } }
    "#;
    let values = message_values(accepted);
    assert_eq!(values.raw, Vec::new());
    assert_eq!(values.causes, 5, "each of a, b, c, d and f shows one cause");
}

#[test]
fn the_watch_label_guard_flags_a_directory_named_any_other_way() {
    // Named as shown: through `safe_path(&shown_watched_dir(…))`, with or without module
    // paths, and through an explicit position. Reported: a directory escaped as it is,
    // captured, displayed, or the label's result changed after it.
    let src = r#"
        fn one(dir: &Path, e: notify::Error) { eprint_warning(&format!("warning: failed to watch {}: {}", safe_path(&shown_watched_dir(dir, root, vars)), safe_inline(notify_cause(&e)))); }
        fn two(dir: &Path) -> miette::Report { miette::miette!("failed to watch directory {}: x", crate::output::safe_path(&crate::watch::shown_watched_dir(dir, root, None))) }
        fn three(dir: &Path) { eprint_warning(&format!("warning: failed to watch {}: x", safe_path(dir))); }
        fn four(dir: &Path) { eprint_warning(&format!("warning: failed to watch {dir:?}")); }
        fn five(dir: &Path) { eprint_warning(&format!("warning: failed to watch {}: x", dir.display())); }
        fn six(dir: &Path) { eprint_warning(&format!("warning: failed to watch {}: x", safe_path(&shown_watched_dir(dir, root, vars).canonicalize()))); }
        fn seven(dir: &Path) { eprint_warning(&format!("warning: failed to watch {1}: {0}", safe_inline(&x), safe_path(&shown_watched_dir(dir, root, vars)))); }
        #[cfg(test)]
        mod tests { fn t(dir: &Path) -> String { format!("failed to watch {}", dir.display()) } }
    "#;
    assert_eq!(
        watch_failure_sites(src),
        vec![
            (2, true),
            (3, true),
            (4, false),
            (5, false),
            (6, false),
            (7, false),
            (8, true)
        ]
    );
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
/// scope, or rename `process` so that a call to one would not name `process::`? Every
/// `process` in the item is judged, so a rename after another `process` path in the same
/// braces is found too.
fn use_reaches_a_process_ender(item: &str) -> bool {
    const PROCESS: &str = "process";
    let b = item.as_bytes();
    let whole_word = |at: &usize| {
        !prev_is_ident(b, *at)
            && !b
                .get(at + PROCESS.len())
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
    };
    let words = |text: &str| -> Vec<String> {
        text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .filter(|w| !w.is_empty())
            .map(str::to_string)
            .collect()
    };
    item.match_indices(PROCESS)
        .map(|(at, _)| at)
        .filter(whole_word)
        .any(|at| {
            let rest = item[at + PROCESS.len()..].trim_start();
            match rest.strip_prefix("::") {
                // `process::exit`, `process::{self, abort}`, `process::*`, and
                // `process::{self as p}`, which renames `process` itself.
                Some(path) => {
                    let path_words = words(path);
                    path.trim_start().starts_with('*')
                        || path_words
                            .iter()
                            .any(|w| PROCESS_ENDERS.contains(&w.as_str()))
                        || path_words
                            .windows(2)
                            .any(|w| w[0] == "self" && w[1] == "as")
                }
                // `process as p`: `p::exit(…)` would not name `process::`.
                None => words(rest).first().is_some_and(|w| w == "as"),
            }
        })
}

/// Where one source file names a [`STREAM_HANDLES`] function (see
/// [`stream_handle_mentions`]).
#[derive(Debug, PartialEq, Eq)]
struct StreamHandles {
    /// 1-based lines of every mention outside the owners that is not an `.is_terminal()`
    /// query: a call, a `use` of one, a handle bound to a local.
    stray: Vec<usize>,
    /// For each owner passed in, in order, the mentions inside its body.
    in_owner: Vec<usize>,
}

/// Find every mention of a [`STREAM_HANDLES`] name in `src` as code — not a method or
/// field (`child.stdout`), a longer identifier, a definition, or anything in a comment or
/// a literal — and sort it into [`StreamHandles`]: inside the body of one of `owners`, a
/// call that only asks `.is_terminal()`, or stray.
fn stream_handle_mentions(src: &str, owners: &[&str]) -> StreamHandles {
    let masked = mask_comments(src);
    let b = masked.as_bytes();
    let bodies: Vec<Option<std::ops::RangeInclusive<usize>>> =
        owners.iter().map(|name| fn_body(&masked, name)).collect();
    let mut handles = StreamHandles {
        stray: Vec::new(),
        in_owner: vec![0; owners.len()],
    };
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
        if STREAM_HANDLES.contains(&&masked[i..end])
            && !follows_a_dot(b, i)
            && !is_fn_definition(&masked, i)
        {
            let owner = bodies
                .iter()
                .position(|body| body.as_ref().is_some_and(|body| body.contains(&i)));
            match owner {
                Some(owner) => handles.in_owner[owner] += 1,
                None if asks_is_terminal(&masked, end) => {}
                None => handles.stray.push(masked[..i].matches('\n').count() + 1),
            }
        }
        i = end;
    }
    handles
}

/// Is the stream handle named just before `end` called only to ask `.is_terminal()`
/// (`stdout().is_terminal()`, whitespace allowed)?
fn asks_is_terminal(masked: &str, end: usize) -> bool {
    const QUERY: &str = "is_terminal";
    let b = masked.as_bytes();
    let mut j = skip_ws(b, end);
    for expected in *b"()." {
        if b.get(j) != Some(&expected) {
            return false;
        }
        j = skip_ws(b, j + 1);
    }
    masked[j..].starts_with(QUERY)
        && !b
            .get(j + QUERY.len())
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

/// Where `mds lint`'s output calls sit (see [`lint_funnel`]).
#[derive(Debug)]
struct LintFunnel {
    /// `file:line: call` for every [`LINT_OUTPUT_CALLS`] call or `exit(…)` in `lint.rs`,
    /// and every `exit(…)` in `lint_sink.rs`.
    stray: Vec<String>,
    /// `ewriteln!` / `ewrite!` calls in `lint_sink.rs`.
    sink_writer_calls: usize,
    /// `write_stdout(…)` calls in `lint_sink.rs`.
    sink_stdout_writes: usize,
    /// `exit(lint::run_lint(…))` calls in `main.rs`: the driver ending the run with lint's
    /// exit code.
    driver_exits: usize,
}

/// Sort the output calls in the sources of `lint.rs`, `lint_sink.rs` and `main.rs` into
/// [`LintFunnel`]. A call is a name from [`LINT_OUTPUT_CALLS`], or `exit`, followed by its
/// parenthesised arguments, outside comments and literals — `output::exit(…)` and
/// `std::process::exit(…)` alike, never `exit_code(…)`.
fn lint_funnel(lint: &str, sink: &str, main: &str) -> LintFunnel {
    let calls = |src: &str, names: &[&str]| find_invocations(&mask_comments(src), names);
    let reported = |file: &str, invocations: Vec<Invocation>| -> Vec<String> {
        invocations
            .into_iter()
            .map(|inv| format!("  {file}:{}: {}", inv.line, inv.name))
            .collect()
    };
    let mut stray = reported("lint.rs", calls(lint, LINT_OUTPUT_CALLS));
    stray.extend(reported("lint.rs", calls(lint, &["exit"])));
    stray.extend(reported("lint_sink.rs", calls(sink, &["exit"])));
    LintFunnel {
        stray,
        sink_writer_calls: calls(sink, &["ewriteln!", "ewrite!"]).len(),
        sink_stdout_writes: calls(sink, &["write_stdout"]).len(),
        driver_exits: calls(main, &["exit"])
            .iter()
            .filter(|inv| inv.body.trim_start().starts_with("lint::run_lint("))
            .count(),
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

// ── Path-sink scanners (#390) ─────────────────────────────────────────────────

/// The code the path-sink rules read: `src` with its comments masked and every
/// `#[cfg(test)]` item blanked, each byte but a newline replaced by a space, so lines and
/// byte offsets stay where they were.
fn product_code(src: &str) -> String {
    blank_cfg_test_items(&mask_comments(src))
}

/// `masked` with every item that follows a `#[cfg(test)]` attribute blanked, the
/// attribute included: a test module (`mod tests { … }` or `mod tests;`), a test-only
/// function or method, a `use`, a field. The item ends at its first top-level `;` or
/// `,`, at the `}` that closes its first top-level `{`, or before the `}` that closes the
/// block around it. An attribute in a literal is not one.
fn blank_cfg_test_items(masked: &str) -> String {
    const ATTR: &[u8] = b"#[cfg(test)]";
    let b = masked.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0usize;
    // Bounded: every step moves `i` forward, past a literal, a byte or a blanked item.
    while i < b.len() {
        if let Some(next) = skip_literal(masked, b, i) {
            i = next;
            continue;
        }
        if !b[i..].starts_with(ATTR) {
            i += 1;
            continue;
        }
        let end = item_end(masked, b, i + ATTR.len()).unwrap_or(b.len() - 1);
        for byte in &mut out[i..=end] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
        i = end + 1;
    }
    String::from_utf8(out).expect("blanking replaces whole items with ASCII spaces")
}

/// Index of the byte that ends the item starting at `from`: its first `;` or `,` outside
/// any parentheses or brackets, the `}` closing its first such `{`, or the byte before a
/// `}` that closes the block the item sits in (a field, the last one). A `,` in generic
/// parameters (`fn f<A, B>`) ends the item early, which leaves test code to be read as
/// product code: more findings, never fewer.
fn item_end(text: &str, b: &[u8], from: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = from;
    while i < b.len() {
        if let Some(next) = skip_literal(text, b, i) {
            i = next;
            continue;
        }
        match b[i] {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b';' | b',' if depth == 0 => return Some(i),
            b'{' if depth == 0 => return matching_delim(text, b, i),
            b'}' if depth == 0 => return Some(i - 1),
            _ => {}
        }
        i += 1;
    }
    None
}

/// Where one source file turns a path into text (see [`path_text_mentions`]).
#[derive(Debug, PartialEq, Eq)]
struct PathTexts {
    /// 1-based lines of every [`PATH_TEXT_METHODS`] call outside the helpers.
    stray: Vec<usize>,
    /// For each helper passed in, in order, the calls inside its body; `None` when the
    /// file's code defines no function of that name.
    in_helper: Vec<Option<usize>>,
}

/// Find every call of a [`PATH_TEXT_METHODS`] method in the product code of `src` — a
/// method call (`path.display()`, the dot perhaps on the line above) or a path to the
/// method (`Path::display(p)`, `.map(Path::display)`), not a local or a parameter of that
/// name, a longer identifier, a definition, a comment or a literal — and sort it into
/// [`PathTexts`]: inside the body of one of `helpers`, or stray.
fn path_text_mentions(src: &str, helpers: &[&str]) -> PathTexts {
    let code = product_code(src);
    let b = code.as_bytes();
    let bodies: Vec<Option<std::ops::RangeInclusive<usize>>> =
        helpers.iter().map(|name| fn_body(&code, name)).collect();
    let mut texts = PathTexts {
        stray: Vec::new(),
        in_helper: bodies.iter().map(|body| body.as_ref().map(|_| 0)).collect(),
    };
    let mut i = 0usize;
    while i < b.len() {
        if let Some(next) = skip_literal(&code, b, i) {
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
        if PATH_TEXT_METHODS.contains(&&code[i..end])
            && (follows_a_dot(b, i) || follows_a_path_separator(b, i))
            && !is_fn_definition(&code, i)
        {
            let helper = bodies
                .iter()
                .position(|body| body.as_ref().is_some_and(|body| body.contains(&i)));
            match helper.and_then(|h| texts.in_helper[h].as_mut()) {
                Some(count) => *count += 1,
                None => texts.stray.push(code[..i].matches('\n').count() + 1),
            }
        }
        i = end;
    }
    texts
}

/// Is the identifier at `i` the last segment of a path (`Path::display`), the `::`
/// perhaps on the line above?
fn follows_a_path_separator(b: &[u8], i: usize) -> bool {
    let mut j = i;
    while j > 0 && b[j - 1].is_ascii_whitespace() {
        j -= 1;
    }
    j >= 2 && b[j - 1] == b':' && b[j - 2] == b':'
}

/// 1-based lines of every [`MESSAGE_MACROS`] invocation in the product code of `src` whose
/// format string formats a value with `Debug` (`{:?}`, `{p:?}`, `{:#?}`), which shows a
/// path quoted and with Rust's escapes: neither as typed nor in the CLI's escape.
fn debug_captures(src: &str) -> Vec<usize> {
    let code = product_code(src);
    let mut lines: Vec<usize> = MESSAGE_MACROS
        .iter()
        .flat_map(|name| nested_invocations(&code, name))
        .filter(|inv| {
            split_top_level(&inv.body).iter().any(|arg| {
                parse_string_literal(arg.trim()).is_some_and(|(fmt, _)| formats_with_debug(&fmt))
            })
        })
        .map(|inv| inv.line)
        .collect();
    lines.sort_unstable();
    lines.dedup();
    lines
}

/// Does the format string `fmt` hold a placeholder whose spec asks for `Debug` (`{:?}`,
/// `{name:#?}`, `{0:x?}`)? `{{` is a literal brace.
fn formats_with_debug(fmt: &str) -> bool {
    let b = fmt.as_bytes();
    let mut i = 0usize;
    // Bounded: every step moves `i` forward, past a literal brace or a placeholder.
    while i < b.len() {
        match b[i] {
            b'{' if b.get(i + 1) == Some(&b'{') => i += 2,
            b'{' => {
                let Some(rel) = fmt[i..].find('}') else {
                    return false;
                };
                let inner = &fmt[i + 1..i + rel];
                if inner
                    .split_once(':')
                    .is_some_and(|(_, spec)| spec.contains('?'))
                {
                    return true;
                }
                i += rel + 1;
            }
            _ => i += 1,
        }
    }
    false
}

/// How one source file's messages interpolate error values (see [`message_values`]).
#[derive(Debug, Default)]
struct MessageValues {
    /// Message sinks read: [`MESSAGE_MACROS`] and [`MESSAGE_CALLS`] invocations and
    /// [`MESSAGE_FIELD`] initialisers.
    sinks: usize,
    /// Values that reach a message as a [`CAUSE_PRODUCERS`] call, escaped where the sink
    /// needs it: the accepted shape.
    causes: usize,
    /// `(line, sink, expression)` for every error value a message interpolates otherwise:
    /// raw (`{e}`, `e.to_string()`), escaped but with its paths (`safe_inline(&e)`), or a
    /// cause left unescaped where the sink does not escape it.
    raw: Vec<(usize, String, String)>,
}

/// Judge every value the product code of `src` interpolates into a message, where the
/// value is an error ([`error_valued`]) or a cause ([`CAUSE_PRODUCERS`]).
fn message_values(src: &str) -> MessageValues {
    let code = product_code(src);
    let names = error_names(&code);
    let mut values = MessageValues::default();
    let judge = |values: &mut MessageValues, line: usize, sink: &str, expr: &str| {
        let sink_escapes = CAUSE_ESCAPING_CALLS.contains(&sink);
        match judge_message_value(expr, &names, sink_escapes) {
            Some(true) => values.causes += 1,
            Some(false) => values.raw.push((line, sink.to_string(), normalize(expr))),
            None => {}
        }
    };
    for name in MESSAGE_MACROS {
        for inv in nested_invocations(&code, name) {
            values.sinks += 1;
            let text = if name.starts_with("write") {
                split_first_arg(&inv.body).map_or("", |(_, rest)| rest)
            } else {
                inv.body.as_str()
            };
            for expr in message_exprs(text) {
                judge(&mut values, inv.line, name, &expr);
            }
        }
    }
    for name in MESSAGE_CALLS {
        for inv in nested_invocations(&code, name) {
            values.sinks += 1;
            for arg in split_top_level(&inv.body) {
                judge(&mut values, inv.line, name, &arg);
            }
        }
    }
    for (line, expr) in message_fields(&code) {
        values.sinks += 1;
        judge(&mut values, line, MESSAGE_FIELD, &expr);
    }
    values
}

/// `Some(true)` when `expr` is a cause in the shape its sink takes —
/// `safe_inline(io_cause(…))` (any [`SANITIZERS`] around any [`CAUSE_PRODUCERS`] call,
/// with any module path), or for a sink that escapes the cause itself the bare producer
/// call too. `Some(false)` when it is an error value in any other shape, or a cause left
/// unescaped. `None` when it is neither.
fn judge_message_value(expr: &str, names: &[String], sink_escapes: bool) -> Option<bool> {
    let e = strip_refs(expr);
    if let Some(inner) = whole_call_to(e, SANITIZERS) {
        if whole_call_to(strip_refs(&inner), CAUSE_PRODUCERS).is_some() {
            return Some(true);
        }
        return error_valued(&inner, names).then_some(false);
    }
    if whole_call_to(e, CAUSE_PRODUCERS).is_some() {
        return Some(sink_escapes);
    }
    error_valued(e, names).then_some(false)
}

/// Does `expr` hold an error — one of `names`, as itself, through a method or a field
/// (`e.to_string()`, `e.error`), or behind `&` / `*` — and not merely `e.kind()`, an
/// `io::ErrorKind`, which names no path?
fn error_valued(expr: &str, names: &[String]) -> bool {
    let e = strip_refs(expr);
    let end = e
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(e.len());
    let root = &e[..end];
    names.iter().any(|name| name == root) && normalize(&e[end..]) != ".kind()"
}

/// `expr` without leading `&`, `&mut`, `*` or whitespace.
fn strip_refs(expr: &str) -> &str {
    let mut e = expr.trim();
    // Bounded: every pass that does not return removes at least one byte.
    loop {
        let stripped = e.trim_start_matches(['&', '*']).trim_start();
        let stripped = stripped
            .strip_prefix("mut ")
            .unwrap_or(stripped)
            .trim_start();
        if stripped.len() == e.len() {
            return e;
        }
        e = stripped;
    }
}

/// The argument text of `expr` when `expr` is wholly a call to a function whose last
/// path segment is one of `names` (`safe_inline(x)`, `crate::output::io_cause(&e)`):
/// every segment of the callee a plain identifier, nothing after the closing paren.
fn whole_call_to(expr: &str, names: &[&str]) -> Option<String> {
    let e = expr.trim();
    let open = e.find('(')?;
    let callee = e[..open].trim();
    let segments: Vec<&str> = callee.split("::").map(str::trim).collect();
    if !segments.iter().all(|s| is_ident(s)) || !names.contains(segments.last()?) {
        return None;
    }
    let close = matching_paren(e, e.as_bytes(), open)?;
    e[close + 1..]
        .trim()
        .is_empty()
        .then(|| e[open + 1..close].to_string())
}

/// The names `code` holds an error in: [`ERROR_NAMES`]; every `Err(name)` /
/// `Err(name @ …)` pattern; the closure parameter of every `map_err`, `or_else`,
/// `unwrap_or_else` and `inspect_err`; every function parameter whose type names an
/// `Error`; and, one hop on, every `let` whose initialiser holds one of those
/// (`let msg = e.to_string();`).
fn error_names(code: &str) -> Vec<String> {
    let b = code.as_bytes();
    let mut names: Vec<String> = ERROR_NAMES.iter().map(|n| (*n).to_string()).collect();

    for inv in find_invocations(code, &["Err"]) {
        let body = inv.body.trim();
        let end = body
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(body.len());
        let (head, tail) = body.split_at(end);
        let tail = tail.trim_start();
        if is_ident(head) && head != "_" && (tail.is_empty() || tail.starts_with('@')) {
            names.push(head.to_string());
        }
    }
    for inv in find_invocations(
        code,
        &["map_err", "or_else", "unwrap_or_else", "inspect_err"],
    ) {
        let Some(params) = inv.body.trim_start().strip_prefix('|') else {
            continue;
        };
        let Some(close) = params.find('|') else {
            continue;
        };
        let pat = params[..close].split(':').next().unwrap_or_default().trim();
        let pat = pat.strip_prefix("mut ").unwrap_or(pat).trim();
        if is_ident(pat) && pat != "_" {
            names.push(pat.to_string());
        }
    }
    let mut i = 0usize;
    while i < b.len() {
        if let Some(next) = skip_literal(code, b, i) {
            i = next;
            continue;
        }
        let is_fn_keyword = b[i..].starts_with(b"fn")
            && !prev_is_ident(b, i)
            && b.get(i + 2).is_some_and(u8::is_ascii_whitespace);
        if is_fn_keyword {
            if let Some(open) = code[i..].find('(').map(|rel| i + rel) {
                if let Some(close) = matching_paren(code, b, open) {
                    for param in split_top_level(&code[open + 1..close]) {
                        let Some((pat, ty)) = param.split_once(':') else {
                            continue;
                        };
                        let pat = pat.trim();
                        let pat = pat.strip_prefix("mut ").unwrap_or(pat).trim();
                        if is_ident(pat) && ty.contains("Error") {
                            names.push(pat.to_string());
                        }
                    }
                    i = close;
                    continue;
                }
            }
        }
        i += 1;
    }

    let direct = names.clone();
    for binding in collect_let_bindings(code) {
        if error_valued(&binding.init, &direct) {
            names.push(binding.name);
        }
    }
    names.sort();
    names.dedup();
    names
}

/// Every `name(…)` invocation in `text`, those nested in another one's arguments
/// included (`format!("{}", format!(…))`), with their 1-based lines in `text`.
fn nested_invocations(text: &str, name: &str) -> Vec<Invocation> {
    let mut found = Vec::new();
    let mut pending: Vec<(String, usize)> = vec![(text.to_string(), 0)];
    // Bounded: each invocation found is queued once, and its body is strictly shorter
    // than the text it was found in.
    while let Some((scope, lines_before)) = pending.pop() {
        for inv in find_invocations(&scope, &[name]) {
            let line = lines_before + inv.line;
            pending.push((inv.body.clone(), line - 1));
            found.push(Invocation {
                line,
                name: inv.name,
                body: inv.body,
            });
        }
    }
    found.sort_by_key(|inv| inv.line);
    found
}

/// The values a message invocation's arguments interpolate: the format string's
/// captures and the arguments after it, or — when the first argument is not a string
/// literal — every argument.
fn message_exprs(body: &str) -> Vec<String> {
    if parse_string_literal(body.trim_start()).is_some() {
        return interpolated_exprs(body);
    }
    split_top_level(body)
        .iter()
        .map(|arg| normalize(strip_named_arg(arg.trim())))
        .filter(|arg| !arg.is_empty())
        .collect()
}

/// `(line, initialiser)` of every `message: <expr>` field in `code` — a struct literal's
/// field, or a declaration's type, which names no value.
fn message_fields(code: &str) -> Vec<(usize, String)> {
    let b = code.as_bytes();
    let field = MESSAGE_FIELD.as_bytes();
    let mut fields = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        if let Some(next) = skip_literal(code, b, i) {
            i = next;
            continue;
        }
        let named = b[i..].starts_with(field)
            && !prev_is_ident(b, i)
            && !b
                .get(i + field.len())
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
            && !follows_a_dot(b, i)
            && !follows_a_path_separator(b, i);
        if !named {
            i += 1;
            continue;
        }
        let colon = skip_ws(b, i + field.len());
        if b.get(colon) != Some(&b':') || b.get(colon + 1) == Some(&b':') {
            i += field.len();
            continue;
        }
        let start = colon + 1;
        let end = expr_end(code, b, start);
        fields.push((
            code[..i].matches('\n').count() + 1,
            normalize(&code[start..end]),
        ));
        i = end;
    }
    fields
}

/// Index just past the expression starting at `from`: the first `,`, `;`, or unmatched
/// closing delimiter outside any nesting.
fn expr_end(text: &str, b: &[u8], from: usize) -> usize {
    let mut depth = 0i32;
    let mut i = from;
    while i < b.len() {
        if let Some(next) = skip_literal(text, b, i) {
            i = next;
            continue;
        }
        match b[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if depth == 0 => return i,
            b')' | b']' | b'}' => depth -= 1,
            b',' | b';' if depth == 0 => return i,
            _ => {}
        }
        i += 1;
    }
    b.len()
}

/// Every message in the product code of `src` whose format string says
/// [`WATCH_FAILURE_TEXT`]: its line, and whether the value that fills its first
/// placeholder — the directory — is `safe_path(&shown_watched_dir(…))`.
fn watch_failure_sites(src: &str) -> Vec<(usize, bool)> {
    let code = product_code(src);
    let mut sites = Vec::new();
    for name in MESSAGE_MACROS {
        for inv in nested_invocations(&code, name) {
            let Some((fmt, _)) = parse_string_literal(inv.body.trim_start()) else {
                continue;
            };
            if fmt.contains(WATCH_FAILURE_TEXT) {
                let named =
                    first_placeholder_value(&inv.body).is_some_and(|dir| names_a_watched_dir(&dir));
                sites.push((inv.line, named));
            }
        }
    }
    sites.sort_unstable();
    sites
}

/// The value that fills the first placeholder of a format invocation's string: a named
/// capture (`{dir}`), or the positional argument a `{}` / `{N}` takes.
fn first_placeholder_value(body: &str) -> Option<String> {
    let trimmed = body.trim_start();
    let (fmt, fmt_end) = parse_string_literal(trimmed)?;
    let bytes = fmt.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'{' if bytes.get(i + 1) == Some(&b'{') => i += 2,
            b'{' => {
                let rel = fmt[i..].find('}')?;
                let inner = &fmt[i + 1..i + rel];
                let name = inner.split(':').next().unwrap_or_default();
                if !name.is_empty() && !name.chars().all(|c| c.is_ascii_digit()) {
                    return Some(name.to_string());
                }
                let index: usize = if name.is_empty() {
                    0
                } else {
                    name.parse().ok()?
                };
                let args = trimmed[fmt_end..].trim_start().strip_prefix(',')?;
                let arg = split_top_level(args).into_iter().nth(index)?;
                return Some(normalize(strip_named_arg(arg.trim())));
            }
            _ => i += 1,
        }
    }
    None
}

/// Is `expr` wholly `safe_path(&shown_watched_dir(…))`, with any module paths?
fn names_a_watched_dir(expr: &str) -> bool {
    whole_call_to(strip_refs(expr), &["safe_path"])
        .is_some_and(|dir| whole_call_to(strip_refs(&dir), &[WATCH_FAILURE_LABEL]).is_some())
}
