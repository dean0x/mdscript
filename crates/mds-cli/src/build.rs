//! Build subcommand implementation and shared compilation helpers.
//!
//! This module re-exports the helpers other CLI modules use.
//! Build children use `super::child::item` to refer to items in sibling modules.
mod admit;
mod config;
mod directory;
mod entry;
mod out_paths;
mod single;
mod source_map;
mod stdio;
mod vars;

use std::collections::HashMap;
use std::ffi::OsString;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};

use crate::write::{write_compiled, write_compiled_and_look, Beside};
use mds::{CompiledOutput, MdsError};
use miette::Result;

pub(crate) use admit::{admit_output, resolve_dir_as_created, EntryPaths};
pub(crate) use config::{compile_inputs, load_config, run_reads, source_inputs, ProjectConfig};
#[cfg(test)]
pub(crate) use config::{BuildConfig, MdsConfig};
pub(crate) use entry::{auto_detect_mds_file, resolve_input};
pub(crate) use out_paths::{
    reject_forbidden_output_flags, resolve_output_path_for_kind, OutputKind,
};
pub(crate) use source_map::map_path_for;
use source_map::{apply_source_map_file_label, embed_carrier};
pub(crate) use stdio::{read_stdin, write_output};
pub(crate) use vars::{
    build_runtime_vars, build_runtime_vars_logged, emit_duplicate_var_warnings,
    emit_duplicate_vars_file_warnings, parse_key_value, vars_file_error, RuntimeVarArgs,
    RuntimeVars,
};

/// Map an error to a categorized exit code.
///
/// Exit codes:
/// - 0: success (never returned here — handled by happy path)
/// - 1: logical/syntax error (undefined variable, arity mismatch, recursion, etc.)
/// - 2: I/O or file-system error (file not found, not an MDS file, I/O failure)
/// - 3: resource limit exceeded (output too large, too many iterations)
/// - 101: internal compiler error
///
/// Errors created via `miette::miette!()` do NOT downcast to `MdsError`
/// and correctly fall through to exit code 1. Only `MdsError` values converted via
/// `.map_err(miette::Error::from)` are categorized.
pub(crate) fn exit_code(err: &miette::Error) -> i32 {
    // AD-211-5: `StdinRelabeledError` is a render-only wrapper around an `MdsError`.
    // It must be unwrapped here or wrapping an error to fix its DISPLAY label would
    // silently change its EXIT CODE (a wrapped FileNotFound would fall through to 1
    // instead of 2). The label swap is not allowed to have behavioural side effects.
    let mds_err = err.downcast_ref::<MdsError>().or_else(|| {
        err.downcast_ref::<crate::output::StdinRelabeledError>()
            .map(crate::output::StdinRelabeledError::inner)
    });
    if let Some(mds_err) = mds_err {
        match mds_err {
            MdsError::Io { .. } | MdsError::FileNotFound { .. } | MdsError::NotMdsFile { .. } => 2,
            MdsError::ResourceLimit { .. } => 3,
            MdsError::Internal => crate::output::PANIC_EXIT,
            // R4: --set/--set-string collision is a usage error over runtime
            // VARIABLES — the logical/content error class (exit 1).  Explicit
            // arm so the classification is a decision, not an accident of the
            // fall-through below.
            MdsError::VarConflict { .. } => 1,
            _ => 1,
        }
    } else {
        1
    }
}

// ── Input-validation helpers ──────────────────────────────────────────────────

/// Validate `path` for single-file fmt/lint: a forbidden path character is
/// refused first (→ `mds::io`, exit 2, #265), then existence is checked (→
/// `mds::file_not_found`, exit 2) and then the `.mds` extension (→
/// `mds::not_mds`, exit 2).
///
/// Every error names the path as the user typed it, escaped with
/// [`mds::escape_path_for_message`] (#417). The refusal comes first and is worded
/// like `NativeFs::check_symlink`'s, so a hostile file argument reports the same
/// error whether or not the file exists.
///
/// Existence-before-extension ordering is required so that a user pointing at a
/// non-existent path without `.mds` receives a "file not found" error rather than
/// the confusing "not an MDS file" error (C4/F6).
pub(crate) fn ensure_existing_mds_file(path: &Path) -> Result<(), MdsError> {
    crate::output::reject_forbidden_output_path("path", path.as_os_str())?;
    let lossy = path.to_string_lossy();
    let shown = mds::escape_path_for_message(&lossy);
    let exists = path.try_exists().map_err(|e| MdsError::Io {
        message: format!(
            "cannot check {shown}: {}",
            crate::output::safe_inline(crate::output::io_cause(&e))
        ),
    })?;
    if !exists {
        return Err(MdsError::FileNotFound {
            path: shown.into_owned(),
            span: None,
            src: None,
        });
    }
    if path.extension().and_then(|e| e.to_str()) != Some("mds") {
        return Err(MdsError::not_mds_file(shown.into_owned()));
    }
    Ok(())
}

// ── Shared compile-and-write helper (used by build and watch) ─────────────────

/// The compiled output content and its transitive dependency list.
///
/// Returned by [`compile_to_content`] so the watch loop can compare content before
/// deciding whether to write (content-based dedup — see watch.rs).
pub(crate) struct CompileOutput {
    /// The compiled string ready to write (markdown or pretty JSON depending on kind).
    pub(crate) content: String,
    /// The output kind (derived intrinsically from the compiled output).
    pub(crate) kind: OutputKind,
    /// Transitive dependency paths (empty when no `@import`s), as the compiler reports
    /// them (`CompileResult.dependencies`). `mds watch` alone reads them, and keys them by
    /// path itself (#390, #409).
    pub(crate) dependencies: Vec<String>,
    /// Source map if `opts.source_map` was `true` and the compilation produced one.
    pub(crate) source_map: Option<mds::SourceMap>,
}

/// Serialize `CompiledOutput` to the CLI wire format.
///
/// - Markdown: the rendered string as-is (moved, no copy).
/// - Messages: pretty-printed JSON array of `{role,content}` with a trailing newline (AC-FUNC-09).
///
/// Takes ownership so the markdown arm avoids an up-to-10 MiB clone (issue 2).
fn serialize_output(output: CompiledOutput) -> Result<String> {
    match output {
        CompiledOutput::Markdown(s) => Ok(s),
        CompiledOutput::Messages(msgs) => messages_json(&msgs).map_err(|e| {
            miette::miette!(
                "failed to serialize messages to JSON: {}",
                crate::output::safe_inline(&e)
            )
        }),
        // `CompiledOutput` is `#[non_exhaustive]`; update this match when new variants land.
        _ => unreachable!("unknown CompiledOutput variant"),
    }
}

/// `messages` as a messages output is written: `serde_json`'s pretty form and a newline.
/// A directory build writes a stale `.json` back through it to show that mds wrote the
/// file before it removes it (#160).
pub(crate) fn messages_json(messages: &[impl serde::Serialize]) -> serde_json::Result<String> {
    let mut json = serde_json::to_string_pretty(messages)?;
    json.push('\n');
    Ok(json)
}

/// Compile `input` and return the content + kind + deps WITHOUT writing any output.
///
/// The output kind (Markdown vs Messages) is determined intrinsically from the compiled
/// result — the caller does not specify it. This is the pure "compile" step used by the
/// watch loop for content-based dedup.
///
/// `build` calls this directly; `mds watch` calls it for its entry and every source
/// once it has checked that the path it compiles still leads to the file it watches.
///
/// Pass `opts = mds::CompileOptions::default()` from watch callers that do not want
/// source maps; the watch paths never emit maps so they always use the default.
///
/// # PF-004 compliance
/// All file reads go through `mds::compile_with_deps_opts` or
/// `mds::compile_str_with_deps_opts` (which use the resolver that enforces
/// MAX_FILE_SIZE). Stdin input is read through `read_stdin` which enforces the same cap.
/// There is no bare `std::fs::read_to_string`.
pub(crate) fn compile_to_content(
    input: &Path,
    runtime_vars: Option<HashMap<String, mds::Value>>,
    quiet: bool,
    opts: mds::CompileOptions,
) -> Result<CompileOutput> {
    let result = if input == Path::new("-") {
        // Stdin: compile from source string with the working directory as base_dir
        // (`None` — see `read_stdin`). read_stdin enforces MAX_FILE_SIZE (PF-004).
        let source = read_stdin()?;
        // AD-211-1 / AD-211-5: a string-source compile labels its errors `<source>`
        // (resolver's SOURCE_LABEL). Relabel to the uniform CLI sentinel here, at the
        // boundary that knows the input was stdin.
        mds::compile_str_with_deps_opts(&source, None, runtime_vars, opts)
            .map_err(|e| crate::output::relabel_stdin_error(&e, &source))?
    } else {
        // File path: compile_with_deps_opts routes through the resolver which enforces
        // MAX_FILE_SIZE and check_symlink (PF-004 compliance).
        mds::compile_with_deps_opts(input, runtime_vars, opts).map_err(miette::Error::from)?
    };
    compiled_content(result, quiet)
}

/// Compile a native entry through the caller's filesystem and entry pin.
pub(crate) fn compile_native_to_content(
    fs: mds::NativeFs,
    input: &Path,
    runtime_vars: Option<HashMap<String, mds::Value>>,
    quiet: bool,
    opts: mds::CompileOptions,
) -> Result<CompileOutput> {
    let result =
        mds::compile_native_fs(fs, input, runtime_vars, opts).map_err(miette::Error::from)?;
    compiled_content(result, quiet)
}

/// Emit compile warnings and convert a core result to the CLI output format.
fn compiled_content(result: mds::CompileResult, quiet: bool) -> Result<CompileOutput> {
    if !quiet {
        for w in &result.warnings {
            crate::output::eprint_warning(w);
        }
    }

    let kind = OutputKind::from(&result.output);
    let source_map = result.source_map;
    // Move result.output into serialize_output so the Markdown arm avoids a clone
    // (the kind was already derived from the borrow above — issue 2).
    let content = serialize_output(result.output)?;
    Ok(CompileOutput {
        content,
        kind,
        dependencies: result.dependencies,
        source_map,
    })
}

// ── Build args struct ─────────────────────────────────────────────────────────

pub(crate) struct BuildArgs {
    pub(crate) input: Option<PathBuf>,
    /// `-o/--output` as given; [`reject_forbidden_output_flags`] turns it into text.
    pub(crate) output: Option<OsString>,
    pub(crate) out_dir: Option<PathBuf>,
    pub(crate) vars: Option<PathBuf>,
    pub(crate) set_vars: Vec<(String, String)>,
    pub(crate) set_string_vars: Vec<(String, String)>,
    pub(crate) quiet: bool,
    /// `--source-map` CLI flag (requires `mds build`).
    pub(crate) source_map: bool,
    /// `--no-source-map` CLI flag (overrides `build.source_map` from mds.json).
    pub(crate) no_source_map: bool,
    /// `--inline` CLI flag: embed map as data URI comment in the output file.
    pub(crate) inline: bool,
    /// `--embed-sources` CLI flag: include source text in sourcesContent[].
    pub(crate) embed_sources: bool,
}

pub(crate) fn run_build(args: BuildArgs) -> Result<()> {
    let BuildArgs {
        input,
        output,
        out_dir,
        vars,
        set_vars,
        set_string_vars,
        quiet,
        source_map: flag_source_map,
        no_source_map,
        inline,
        embed_sources: flag_embed_sources,
    } = args;
    // #265, #390: refuse a hostile output location before anything is read or compiled.
    let output = reject_forbidden_output_flags(output.as_deref(), out_dir.as_deref())?;
    let resolved = build_runtime_vars(RuntimeVarArgs {
        vars,
        set_vars,
        set_string_vars,
    })?;
    emit_duplicate_var_warnings(&resolved, quiet);

    // Resolve the input: explicit path, or auto-detect from cwd.
    // When auto-detected, print a "Building {path}" banner so users know which file was selected.
    let (input, auto_detected) = resolve_input(input, "build")?;
    if auto_detected && !quiet {
        crate::output::ewriteln!("Building {}", crate::output::safe_path(&input));
    }

    let run = single::BuildRun {
        output,
        out_dir,
        quiet,
        flag_source_map,
        no_source_map,
        inline,
        flag_embed_sources,
    };

    // Directory mode: compile every non-partial .mds file in the tree.
    if input != Path::new("-") && input.is_dir() {
        return directory::build_directory_mode(input, resolved, run);
    }

    // The vars, and the `--vars` file they came from, which no output is written over
    // (#425).
    if input == Path::new("-") {
        return single::run_build_stdin(input, resolved, &run);
    }

    single::run_build_file(input, resolved, &run)
}

/// Compile every non-partial `.mds` file under `dir`, streaming one at a time
/// (AC-PERF-02: peak RSS ≈ O(largest single file), not O(total)).
///
/// Continue-on-error: a per-file compile error does NOT abort the run.
/// All valid files are written; a summary is printed (unless `--quiet` and the run
/// succeeded); non-zero exit when any failed (AC-FUNC-18).
///
/// **Summary / quiet contract (AD-216-1):** the summary line is suppressed under
/// `--quiet` on a fully-successful run (`fail_count == 0`).  When any file fails,
/// the summary is always emitted so the non-zero exit is never unexplained.
/// This mirrors the gate used by `mds check` (`main.rs`) and `mds fmt` (`fmt.rs`).
///
/// **Nothing to build is an error (#204, #387):** when the walk yields no files to
/// compile the run exits 1 with a one-line stderr diagnostic that bypasses `--quiet`.
/// Three shapes, in the order they are checked: the all-excluded count diagnostic
/// (every candidate sits under a default-excluded directory), `no .mds files found
/// in <dir>; nothing was built` (a genuinely empty tree), and the partials-only
/// count diagnostic (#387 — the tree is non-empty but every candidate is a
/// `_`-prefixed partial). All three exit through [`crate::output::exit`]: no `MdsError`
/// variant exists for "nothing to do" and `exit_code` must not grow one for a
/// non-error class.
/// `mds check` mirrors all three with exit 1; `mds fmt` (exit 1) and `mds lint`
/// (exit 2) mirror only the first two — they iterate every file including partials,
/// so a partials-only tree is real work for them, not "nothing to do".
/// `mds watch <dir>` deliberately does NOT error on any of the three.
///
/// **I/O failures (#157):** an output directory that cannot be created, an output or
/// `.map` sidecar that cannot be written, and a stale sibling that cannot be read or
/// whose proven removal fails are each reported as one `mds::io` error through
/// [`crate::output::eprint_io_failure`], which records it for the exit code, so the run
/// exits 2 while the other files are still built. A compile that fails is reported
/// through [`crate::output::eprint_file_failure`], which records it the same way when it
/// is an I/O or file-system failure — a source that cannot be read, say — and as a
/// resource limit when it is one, so the run exits 3. All but the stale sibling count
/// their file as failed; its file was built. A run whose failures are only template
/// errors exits 1.
///
/// Subtree mirroring: with `--out-dir`, mirrors the source subtree into the out-dir
/// with the intrinsic extension per file (AC-FUNC-16). Without `--out-dir`, each
/// output is placed next to its source (AC-FUNC-19).
///
/// Stale-output cleanup: after writing an output, looks at the other kind's output of
/// the same name, left when the source compiled to that kind, and removes it only when
/// mds provably wrote it — a stale `.md` never — keeping anything else with a warning
/// ([`crate::output::probe_and_remove_stale`], #160).
///
/// No output is written over a file the run reads (#425): the source it was compiled
/// from, the modules that compile imported, the `--vars` file `vars` came from, the
/// `mds.json` in force ([`compile_inputs`]) and the `mds.json` nearest that source
/// ([`source_inputs`]).
fn run_build_directory(
    dir: &Path,
    out_dir: Option<PathBuf>,
    vars: RuntimeVars,
    quiet: bool,
    source_map: bool,
    embed_sources: bool,
    inline: bool,
) -> Result<()> {
    use crate::output::{
        collect_mds_files_detailed, is_partial, output_base_no_ext, output_path_for,
        probe_and_remove_stale, resolve_output_base, stale_output, OutputBase, RootPaths,
    };

    const MAX_DEPTH: usize = 64;

    // Load project config from the directory root.
    let config = load_config(dir)?;
    let reads = run_reads(vars.vars_file.as_deref(), config.as_ref());
    let runtime_vars = vars.vars;
    // The out-dir's canonical form keeps the starts_with checks reliable; its shown form
    // is the out-dir as typed (#390).
    let output_base = resolve_output_base(out_dir.as_ref(), &config)?;

    // Keep the canonical directory for output exclusion and the walk-time entry pins.
    let canonical_dir =
        crate::input::resolve_directory_argument(dir).map_err(miette::Error::from)?;

    // Exclude the out-dir from collection when it is nested inside the source root.
    let exclude_prefix: Option<PathBuf> = match &output_base {
        OutputBase::Dir { canonical: d, .. } if d.starts_with(&canonical_dir) => Some(d.clone()),
        _ => None,
    };

    let walk = collect_mds_files_detailed(dir, MAX_DEPTH, exclude_prefix.as_deref());
    let files = &walk.files;

    if files.is_empty() {
        if walk.excluded_by_default > 0 {
            // All candidates were inside default-excluded directories.  Emit the
            // diagnostic even under --quiet (avoids a silent CI green pass — avoids
            // PF-004 enforcement gap where the limit is real on one path and absent
            // on another).
            crate::output::ewriteln!(
                "{} .mds file(s) found but all are under default-excluded directories \
                 (hidden dirs, node_modules); nothing was built",
                walk.excluded_by_default
            );
            crate::output::exit(1);
        }
        // #204: an empty tree is "nothing to build", not success.  Same shape as the
        // all-excluded arm above — emitted even under --quiet (a silent green pass on
        // a mistyped or not-yet-populated directory is the CI failure mode this
        // closes) and exit 1, the build/check/fmt "nothing was done" code (spec §7.9).
        // `mds watch <dir>` deliberately still starts on an empty tree: a file created
        // later is a valid flow there.
        crate::output::ewriteln!(
            "no .mds files found in {}; nothing was built",
            crate::output::safe_path(dir)
        );
        crate::output::exit(1);
    }

    // #387: a tree whose only .mds files are partials is "nothing to build" too. The
    // walker collects partials (watch/fmt/lint need them) but this loop skips them, so
    // without this arm the run ends `0 built, 0 failed`, exit 0 — the silent green pass
    // #204 closed for the empty tree. Same shape as the all-excluded arm: count-carrying,
    // emitted even under --quiet, exit 1. fmt and lint operate on partials and keep their
    // behaviour; `mds watch <dir>` still starts.
    if let Some(partials_only_count) = crate::output::partials_only(files) {
        crate::output::ewriteln!(
            "{partials_only_count} .mds file(s) found in {} but all are _-prefixed partials; \
             nothing was built",
            crate::output::safe_path(dir)
        );
        crate::output::exit(1);
    }

    let mut ok_count: usize = 0;
    let mut fail_count: usize = 0;
    // R5: successful compilations whose written artifact is ZERO bytes (strict
    // `is_empty()`, not trim — messages-mode JSON is never empty, and a
    // whitespace-only markdown body is still content the author wrote).  A
    // definitions-only module compiles to zero bytes silently; "N built" alone
    // implies N useful artifacts (PF-034-adjacent).
    let mut empty_count: usize = 0;

    for file in files {
        // Skip partials: they contribute to imports but produce no standalone output.
        if is_partial(file) {
            continue;
        }

        // Per-file source_map_base: the output directory for this file, computed
        // from the kind-independent directory oracle, without creating it — an early
        // create_dir_all would leave an empty directory on compile failure (Step 6
        // Caveat 1). A walked file's output stem always has a parent, the directory it
        // lands in, so the base is always that directory.
        let base_no_ext = output_base_no_ext(file, dir, &output_base);
        let opts = mds::CompileOptions::default()
            .with_source_map(source_map)
            .with_include_sources_content(embed_sources)
            .with_source_map_base(base_no_ext.parent().map(Path::to_path_buf));

        // Compile (all reads go through mds-core which enforces MAX_FILE_SIZE — PF-004).
        // A panic in it fails this file alone, and the batch goes on (#389).
        let pin = match crate::output::walked_pin(&walk, file, dir, &canonical_dir) {
            Ok(pin) => pin,
            Err(e) => {
                crate::output::eprint_file_failure(e.into());
                fail_count += 1;
                continue;
            }
        };
        let fs = mds::NativeFs::new().with_entry_pin(pin);
        let compiled = crate::output::catch_compile(
            file,
            AssertUnwindSafe(|| {
                compile_native_to_content(fs, file, runtime_vars.clone(), quiet, opts)
            }),
        );
        match compiled {
            Ok(Ok(mut compiled)) => {
                let ext = compiled.kind.extension();
                let inputs = source_inputs(file, &compiled.dependencies, &reads);
                let target = output_path_for(file, RootPaths::as_typed(dir), &output_base, ext);

                // Set `file` field for this output path (sources already relativized by core).
                if let Some(ref mut sm) = compiled.source_map {
                    apply_source_map_file_label(sm, Some(&target.path), false);
                }

                // Determine final content (inline embeds the carrier).
                let final_content = if source_map && inline {
                    if let Some(ref sm) = compiled.source_map {
                        embed_carrier(compiled.content, &sm.to_json())
                    } else {
                        compiled.content
                    }
                } else {
                    compiled.content
                };

                // R5: capture emptiness BEFORE the write moves/borrows the content;
                // counted only in the Ok arm below — a failed write is a failure,
                // not an empty success.
                let wrote_empty = final_content.is_empty();

                // #227: the dir-mode twin of `write_output` — same atomic contract, but
                // it accumulates per-file counters instead of returning early, so it is
                // its own call site. Both are enforced by `tests/write_funnel.rs`. The
                // write creates the out-dir and the mirrored directories below it, and
                // refuses a symlink among them (#160) and an MDS module at the output
                // (#425): that file fails, and the build goes on with the next. Below an
                // out-dir it also looks at the other kind's output of the same name, in the
                // directory it wrote in, for the stale-output cleanup below (#160).
                let stale = matches!(output_base, OutputBase::Dir { .. })
                    .then(|| stale_output(&target, compiled.kind));
                let written = match &stale {
                    Some(stale) => {
                        write_compiled_and_look(&target, &final_content, &inputs, stale).map(Some)
                    }
                    None => write_compiled(&target, &final_content, &inputs).map(|()| None),
                };
                match written {
                    Ok(beside) => {
                        if wrote_empty {
                            empty_count += 1;
                        }
                        if !quiet {
                            crate::output::ewriteln!(
                                "Compiled to {}",
                                crate::output::safe_path(&target.shown)
                            );
                        }

                        // Write sidecar map (non-inline mode).
                        if source_map && !inline {
                            if let Some(ref sm) = compiled.source_map {
                                let map = target.sibling(map_path_for);
                                let map_json = sm.to_json();
                                if let Err(e) = write_compiled(&map, &map_json, &inputs) {
                                    // The primitive's message already names the path —
                                    // re-prefixing it would print the path twice (#227).
                                    crate::output::eprint_io_failure(e);
                                    fail_count += 1;
                                    continue;
                                }
                                if !quiet {
                                    crate::output::ewriteln!(
                                        "Source map written to {}",
                                        crate::output::safe_path(&map.shown)
                                    );
                                }
                            }
                        }

                        // Stale-output cleanup (#160): the other kind's output of the
                        // name just written — left by a build when the source compiled
                        // to that kind — is removed only when mds provably wrote it, and
                        // a stale `.md` never (see `probe_and_remove_stale`). It is
                        // looked at below an out-dir only: next to the source that name
                        // can be a hand-authored file's, or another source's output, one
                        // this run wrote included — `a.b.mds` and `a.mds` both name
                        // theirs `a.md` or `a.json` there. A name the write found nothing
                        // at is passed over: there is nothing to prove or remove.
                        if beside == Some(Beside::Something) {
                            // The output itself was built, so the file is not counted as
                            // failed; a failed removal still lifts the exit code (#157).
                            if let Err(e) = probe_and_remove_stale(&target, compiled.kind, quiet) {
                                crate::output::eprint_io_failure(e);
                            }
                        }
                        ok_count += 1;
                    }
                    Err(e) => {
                        // The primitive's message already names the path (#227).
                        crate::output::eprint_io_failure(e);
                        fail_count += 1;
                    }
                }
            }
            Ok(Err(e)) => {
                // Route through the single render choke point, so no path renders a
                // report without its escaping; a source that cannot be read lifts the
                // exit code to 2, one over a resource limit to 3 (#157).
                crate::output::eprint_file_failure(e);
                fail_count += 1;
            }
            // The panic hook reported it, and the run will exit 101.
            Err(crate::output::Panicked) => fail_count += 1,
        }
    }

    // AD-216-1: mirror the gate used by `mds check` (main.rs) and `mds fmt` (fmt.rs):
    // suppress the summary under --quiet when every file succeeded, so a quiet CI job
    // produces no output on a clean run.  AD-216-2: exit codes are untouched — a
    // --quiet build with failures still exits non-zero with the summary explaining why
    // (1, or 2 once an I/O failure was recorded, or 3 once a resource limit was — the exit
    // funnel applies it, #157).
    // R5: the empty clause appears ONLY when empty_count > 0 — the historical
    // `{ok} built, {fail} failed` form is byte-identical otherwise (no golden
    // churn).  The suppression gate itself is unchanged (AD-216-1): empty
    // outputs are informational, not failures, so they do not force the summary
    // through --quiet.
    if !quiet || fail_count > 0 {
        if empty_count > 0 {
            crate::output::ewriteln!("{ok_count} built ({empty_count} empty), {fail_count} failed");
        } else {
            crate::output::ewriteln!("{ok_count} built, {fail_count} failed");
        }
    }

    if fail_count > 0 {
        crate::output::exit(1);
    }
    Ok(())
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_maps_internal_failures_to_101() {
        let error = MdsError::Internal;
        for report in [
            miette::Report::new(error.clone()),
            crate::output::relabel_stdin_error(&error, ""),
        ] {
            assert_eq!(exit_code(&report), 101);
        }
    }
}
