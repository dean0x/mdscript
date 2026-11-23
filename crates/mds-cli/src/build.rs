//! Build subcommand implementation and shared compilation helpers.
//!
//! This module re-exports the helpers other CLI modules use.
//! Build children use `super::child::item` to refer to items in sibling modules.
mod config;
mod entry;
mod out_paths;
mod stdio;
mod vars;

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Read;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};

use crate::output::WriteTarget;
use crate::write::{write_compiled, write_compiled_and_look, Beside, NotRemoved, Removal};
use mds::{effective_parent, CompiledOutput, MdsError, STRING_SOURCE_MAP_LABEL};
use miette::Result;

pub(crate) use config::{compile_inputs, load_config, run_reads, source_inputs, ProjectConfig};
#[cfg(test)]
pub(crate) use config::{BuildConfig, MdsConfig};
pub(crate) use entry::{auto_detect_mds_file, resolve_input};
use out_paths::warn_output_extension_mismatch;
pub(crate) use out_paths::{
    reject_forbidden_output_flags, resolve_output_path_for_kind, OutputKind,
};
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

/// A single-file entry in the two forms [`admit_output`] takes, always in
/// `(typed, canonical)` order, so the two cannot be passed swapped.
///
/// `typed` is the path as the user typed it: a refusal names the entry that way, never
/// by its canonical absolute path. `canonical` is the entry's identity, which
/// [`admit_output`] compares with the output's through [`file_identity`], canonical with
/// canonical — `typed` is never compared with it as text (#408). `mds watch` lends the
/// entry it watches in its two forms. `mds build` compiles its entry once, by the path as
/// typed, and never holds a canonical form, so it passes the typed path as both:
/// [`file_identity`] canonicalizes it.
#[derive(Clone, Copy)]
pub(crate) struct EntryPaths<'a> {
    pub(crate) typed: &'a Path,
    pub(crate) canonical: &'a Path,
}

impl<'a> EntryPaths<'a> {
    /// An entry held only as typed, as `mds build` holds it: the typed path as both.
    pub(crate) fn as_typed(path: &'a Path) -> Self {
        Self {
            typed: path,
            canonical: path,
        }
    }
}

/// The canonical path of the file `path` names, for comparing two paths as files, or
/// `None` when not even its directory resolves.
///
/// Its directory is resolved as [`write_output`] will leave it, created if it does not
/// exist yet ([`resolve_dir_as_created`]), and its name is looked up there. An existing
/// file is canonicalized, which respells its name the way the volume stores it — so
/// `PAGE.md` and `page.md` are one file on a case-insensitive volume (#408), in a
/// directory reached back out of one the write creates (`newdir/../PAGE.md`) as much as
/// in one that exists. A name that does not exist yet, or is a symlink, is kept as it
/// stands: the symlink itself is the directory entry a write would replace, not the
/// file it points to.
fn file_identity(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?;
    let file = resolve_dir_as_created(effective_parent(path))?.join(name);
    let is_link = std::fs::symlink_metadata(&file).is_ok_and(|m| m.file_type().is_symlink());
    if !is_link {
        if let Ok(canonical) = file.canonicalize() {
            return Some(canonical);
        }
    }
    Some(file)
}

/// The canonical path `dir` has once [`write_output`] has created it — the write creates
/// an output's directory, its anchor, with `create_dir_all` (#160) — found without
/// creating anything; `None` when not even the working directory
/// resolves, which a write of `dir` could not get past either.
///
/// An existing `dir` is canonicalized. Otherwise it is walked component by component, as
/// the system resolves it while creating it: an existing component is canonicalized —
/// a symlink is followed — and the first missing one is created as a plain directory,
/// so below it nothing exists yet: a name is appended as it stands, and a `..` leads back
/// to the parent, where the next name is looked up on disk again. So `newdir/..` is the
/// directory `newdir` would be created in, and `newdir/../lnk` follows `lnk`. The walk
/// visits each component of `dir` once, and a `..` never climbs above the root.
///
/// A relative `dir` is anchored at the working directory with [`std::path::absolute`].
/// On Windows that also collapses `..` lexically, which is how every Win32 file call
/// reads a path, so the walk and the write agree there too.
///
/// `mds watch` resolves its out-dir this way before each write, to tell the directory
/// the session started with from one the path now leads to (#160).
pub(crate) fn resolve_dir_as_created(dir: &Path) -> Option<PathBuf> {
    use std::path::Component;

    if let Ok(canonical) = dir.canonicalize() {
        return Some(canonical);
    }
    let absolute = std::path::absolute(dir).ok()?;
    let mut resolved = PathBuf::new();
    // How many trailing components of `resolved` the write creates.
    let mut created: usize = 0;
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => resolved.push(component),
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
                created = created.saturating_sub(1);
            }
            Component::Normal(name) => {
                let next = resolved.join(name);
                if created > 0 {
                    resolved = next;
                    created += 1;
                } else if let Ok(existing) = next.canonicalize() {
                    resolved = existing;
                } else {
                    resolved = next;
                    created = 1;
                }
            }
        }
    }
    Some(resolved)
}

/// Refuse to write the compiled entry over the entry file itself (#425): `mds::io`,
/// exit 2, naming the entry as `typed`, escaped.
///
/// A `.md` entry that declares `type: mds` compiles to Markdown, whose default output
/// name — the entry's stem plus `.md` — is the entry's own name; `--out-dir` or
/// `mds.json` `build.output_dir` naming the entry's directory, and `-o` naming the
/// entry itself (any extension), land on it too. Writing would replace the source with
/// its compiled form, which no longer declares `type: mds`, so the next build fails.
///
/// `output` and `entry.canonical` are compared as the files they name
/// ([`file_identity`]), canonical with canonical, never a path with a spelling of it
/// (#408) — an output whose directory does not exist yet as the write will create it, so
/// `newdir/../page.md` is `page.md`. `None` (stdout) is never the entry. It runs only
/// inside [`admit_output`], after the output path is resolved and before anything is
/// written.
fn refuse_output_over_entry(output: Option<&Path>, entry: EntryPaths<'_>) -> Result<(), MdsError> {
    let Some(output) = output else {
        return Ok(());
    };
    match (file_identity(output), file_identity(entry.canonical)) {
        (Some(output), Some(canonical)) if output == canonical => Err(MdsError::Io {
            message: format!(
                "output would overwrite the entry file: \"{}\"; \
                 write it elsewhere with -o <file> or --out-dir <dir>",
                mds::escape_path_for_message(&entry.typed.to_string_lossy())
            ),
        }),
        _ => Ok(()),
    }
}

/// Admit `output_path` as the destination of the compiled `entry`: refuse it when it is
/// the entry file itself ([`refuse_output_over_entry`], #425), and only then warn when
/// an explicit `-o` (`output_arg`) contradicts `kind` ([`warn_output_extension_mismatch`])
/// — the warning announces the write, so a refused output never gets one.
///
/// Every route to a compiled entry's output goes through it: `mds build` file mode,
/// `mds watch`'s startup compile, its startup fallback and every rebuild. A rebuild
/// passes no `output_arg`: the warning is a startup message, printed once for the path
/// every rebuild reuses.
pub(crate) fn admit_output(
    output_path: Option<&Path>,
    entry: EntryPaths<'_>,
    output_arg: &Option<String>,
    kind: OutputKind,
    quiet: bool,
) -> Result<(), MdsError> {
    refuse_output_over_entry(output_path, entry)?;
    warn_output_extension_mismatch(output_arg, kind, quiet);
    Ok(())
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

// ── Source-map helpers ────────────────────────────────────────────────────────

/// Return the sidecar map path: append `.map` to the full output name.
///
/// `foo.md` → `foo.md.map`, `foo.json` → `foo.json.map`.
/// Never uses `with_extension` because that replaces the final component
/// (e.g. `foo.md.map` would become `foo.map`), not appends.
pub(crate) fn map_path_for(output_path: &Path) -> PathBuf {
    let mut s = output_path.as_os_str().to_owned();
    s.push(".map");
    PathBuf::from(s)
}

/// RFC-4648 standard base64 encode (no line wrapping).
///
/// Alphabet: `A-Za-z0-9+/` with `=` padding.  None of these characters are
/// `<`, `>`, or `-`, so the encoded payload cannot break out of an HTML comment
/// (AC-SEC-03).
pub(crate) fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Build the inline sourceMappingURL comment carrier for a source map JSON.
///
/// Format: `<!--# sourceMappingURL=data:application/json;base64,<B64> -->`
///
/// The `<!--# ... -->` syntax is the canonical format supported by browser
/// devtools and bundler tooling.  The base64 alphabet excludes `<`, `>`, and
/// `-` so the payload cannot break out of the HTML comment (AC-SEC-03).
pub(crate) fn carrier_line(map_json: &str) -> String {
    let b64 = base64_encode(map_json.as_bytes());
    format!("<!--# sourceMappingURL=data:application/json;base64,{b64} -->")
}

/// Strip a trailing inline sourceMappingURL carrier comment from `content`
/// (for idempotent re-embedding).
///
/// Looks for `<!--# sourceMappingURL=data:` as the last non-empty line.
fn strip_existing_carrier(content: &str) -> &str {
    // Scan from the end, skipping a final newline if present.
    let trimmed = content.trim_end_matches('\n');
    // Find the last newline boundary.
    if let Some(pos) = trimmed.rfind('\n') {
        let last_line = &trimmed[pos + 1..];
        if last_line.starts_with("<!--# sourceMappingURL=data:") {
            // Strip the last line plus its preceding newline.
            return &content[..pos + 1];
        }
    } else if trimmed.starts_with("<!--# sourceMappingURL=data:") {
        // The entire content is the carrier (single-line file).
        return "";
    }
    content
}

/// Embed an inline carrier comment at the end of `content`.
///
/// Strips any existing carrier first (idempotent), then appends a newline
/// separator (if needed) and the carrier line.
pub(crate) fn embed_carrier(content: String, map_json: &str) -> String {
    let base = strip_existing_carrier(&content);
    // Ensure there is exactly one newline before the carrier.
    let sep = if base.ends_with('\n') { "" } else { "\n" };
    format!("{base}{sep}{}\n", carrier_line(map_json))
}

/// Compute the directory to use as `source_map_base` in [`mds::CompileOptions`].
///
/// This is the map-file directory: the anchor against which core's
/// `relativize_source` relativizes every `sources[]` entry (ADR-005 / PF-004 —
/// single choke-point in core).  Must be called BEFORE constructing
/// `CompileOptions` so `source_map_base` can be set on the options struct.
///
/// The result is always absolutized against the working directory so that core's
/// root-containment check works correctly against the absolute project root. Only a
/// build that writes a source map needs it: core relativizes `sources[]` against it and
/// reads it for nothing else.
///
/// Mirrors the output-directory rules of [`resolve_output_path_for_kind`]:
/// - `-o -` or stdin-with-no-output → current working directory.
/// - `-o <file>` → directory of that file (absolutized if relative).
/// - `--out-dir <dir>` → that directory (absolutized if relative).
/// - mds.json `output_dir` → config-directory-relative.
/// - Default → beside the source file.
///
/// # Errors
///
/// A base that needs the working directory when it cannot be determined: `mds::io`, in
/// [`crate::output::current_dir`]'s words (#390). It is never anchored at `"."`
/// instead: core takes a relative base as an absolute one, so `sources[]` would be
/// computed against the wrong directory without a word.
fn compute_source_map_base(
    input: &Path,
    output: &Option<String>,
    out_dir: &Option<PathBuf>,
    config: &Option<ProjectConfig>,
) -> Result<PathBuf, MdsError> {
    use crate::output::current_dir;
    let abs = |p: PathBuf| -> Result<PathBuf, MdsError> {
        if p.is_absolute() {
            Ok(p)
        } else {
            Ok(current_dir()?.join(p))
        }
    };

    match output.as_deref() {
        Some("-") => {
            // -o - : stdout; relativize against CWD (PF-005: unconditional).
            current_dir()
        }
        Some(o) => {
            // -o <file>: map lives beside the output file.
            // effective_parent maps "" (bare filename) to "." — PF-006.
            abs(effective_parent(Path::new(o)).to_path_buf())
        }
        None => {
            if let Some(dir) = out_dir {
                // --out-dir <dir>: absolutize if relative.
                abs(dir.clone())
            } else if input == Path::new("-") {
                // Stdin with no -o or --out-dir → stdout → relativize against CWD.
                current_dir()
            } else if let Some(ProjectConfig { config, dir, .. }) = config {
                if let Some(ref output_dir) = config.build.output_dir {
                    // mds.json output_dir: config-directory-relative.  `dir`
                    // is canonical (load_config canonicalizes before walking up) so
                    // the join is already absolute in practice; `abs` makes the
                    // "result is always absolutized" contract above structural
                    // rather than incidental — a relative base would silently
                    // demote core's map-relative emission to root-relative.
                    abs(dir.join(output_dir))
                } else {
                    // Default: beside the source file.
                    abs(effective_parent(input).to_path_buf())
                }
            } else {
                // No config, no -o, no --out-dir: beside the source file.
                abs(effective_parent(input).to_path_buf())
            }
        }
    }
}

/// Set the SMv3 `file` field and relabel the stdin source in a [`mds::SourceMap`].
///
/// These are the CLI's two genuinely CLI-only post-processing jobs after core
/// has already applied `relativize_source` at both finalize sites (ADR-005 /
/// PF-004 — single choke-point in core):
///
/// 1. `sm.file = output_basename` — the SMv3 `file` field names the generated
///    artifact; core always emits `file: None` because it has no notion of the
///    output path.
/// 2. The `<stdin>` relabel: maps `STRING_SOURCE_MAP_LABEL` (`"input.mds"`) →
///    `"<stdin>"` for stdin builds.  Core canonicalizes the source entry at
///    `MapBuilder::new`/`source_index`, so the exact-string check here always
///    matches the canonicalized value.  This is a pure label swap — no path
///    logic.
pub(crate) fn apply_source_map_file_label(
    sm: &mut mds::SourceMap,
    output_path: Option<&Path>,
    stdin_label: bool,
) {
    // Job 1: set the `file` field for file output.
    if let Some(out) = output_path {
        sm.file = out.file_name().map(|n| n.to_string_lossy().into_owned());
    }
    // `sm.file` stays None for stdout output (no output filename to anchor).

    // Job 2: relabel the stdin source entry.
    if stdin_label {
        for src in &mut sm.sources {
            if src == STRING_SOURCE_MAP_LABEL {
                // AD-211-3: use the centralised sentinel from output.rs.
                *src = crate::output::STDIN_DISPLAY_LABEL.to_string();
            }
        }
    }
}

/// Delete the `.map` file at `map.path` when it is the sidecar an earlier
/// `--source-map` build wrote for the output named `expected_basename` (stale-map
/// reconciliation), and leave anything else in place, never clobbering a hand-authored
/// file that happens to share its name.
///
/// A missing file is skipped silently. Anything else that is not such a sidecar is left
/// in place with a warning (unless `quiet`): one that is not a regular file is never
/// opened — opening a FIFO with no writer blocks — and a regular file is recognised by
/// its first bytes alone ([`has_sidecar_head`]), so none is read whole (#428). The
/// removal goes through [`crate::write::remove_proven`] (#160): the map is looked at,
/// read and removed below its anchor, through no symlink there or at the map, and only
/// while it is the file read. Every message names the map by `map.shown` (#390).
///
/// # Errors
///
/// A map that cannot be read — so nothing is known of its content — and a sidecar that
/// is not removed — refused, or the removal failed — are `mds::io` (exit 2, #157).
pub(crate) fn verify_then_delete_map(
    map: &WriteTarget,
    expected_basename: &str,
    quiet: bool,
) -> Result<(), MdsError> {
    let proof = |file: &mut std::fs::File| has_sidecar_head(file, expected_basename);
    match crate::write::remove_proven(map, proof) {
        Ok(Removal::Removed) => {
            if !quiet {
                crate::output::ewriteln!(
                    "Removed stale map {}",
                    crate::output::safe_path(&map.shown)
                );
            }
            Ok(())
        }
        Ok(Removal::Missing) => Ok(()),
        Ok(Removal::Kept) => keep_stale_map(map, "not proven to be written by mds", quiet),
        Err(not_a_file @ NotRemoved::NotAFile) => keep_stale_map(map, not_a_file.cause(), quiet),
        Err(not_removed) => Err(crate::output::stale_removal_error(
            "stale map",
            &map.shown,
            &not_removed,
        )),
    }
}

/// The warning for a stale map left in place, `warning: kept stale map <path>: <why>`, in
/// the words of the stale outputs `mds build` keeps ([`crate::output::probe_and_remove_stale`]);
/// none under `quiet`. The path is `map.shown` (#390).
fn keep_stale_map(map: &WriteTarget, why: &str, quiet: bool) -> Result<(), MdsError> {
    if !quiet {
        crate::output::ewriteln!(
            "warning: kept stale map {}: {}",
            crate::output::safe_path(&map.shown),
            crate::output::safe_inline(why)
        );
    }
    Ok(())
}

/// Whether `reader` starts with the bytes every sidecar mds writes for the output named
/// `expected_basename` starts with: `{"version":3,"file":<the name as a JSON string>,`.
/// `SourceMap::to_json` writes `version`, `file` and then `sources` in that order, with
/// no whitespace, and a sidecar always carries the output's name as `file`, so a map it
/// wrote always starts so; a map formatted by hand does not, even with the same fields.
/// Only those bytes are read (#428); a read that fails is the error.
fn has_sidecar_head(reader: &mut impl Read, expected_basename: &str) -> std::io::Result<bool> {
    let Ok(name) = serde_json::to_string(expected_basename) else {
        return Ok(false);
    };
    let head = format!("{{\"version\":3,\"file\":{name},");
    let len = head.len() as u64;
    Ok(mds::read_at_most(reader, len, len)? == head.as_bytes())
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

    // Directory mode: compile every non-partial .mds file in the tree.
    if input != Path::new("-") && input.is_dir() {
        // Reject -o/--output in directory mode: output goes to files, not a single destination.
        if output.is_some() {
            return Err(miette::miette!(
                "build directory mode does not support -o/--output; \
                 use --out-dir to specify an output directory"
            ));
        }
        // #413: the one directory-argument check every directory-mode subcommand makes
        // (a symlink, the filesystem root, a forbidden character — all `mds::io`).
        crate::input::resolve_directory_argument(&input).map_err(miette::Error::from)?;

        // Load project config to determine effective flags for directory mode.
        let dir_config = load_config(&input)?;
        let cfg_source_map = dir_config
            .as_ref()
            .map(|c| c.config.build.source_map)
            .unwrap_or(false);
        let cfg_embed_sources = dir_config
            .as_ref()
            .map(|c| c.config.build.embed_sources)
            .unwrap_or(false);
        let use_source_map = (flag_source_map || cfg_source_map) && !no_source_map;
        let use_embed_sources = flag_embed_sources || cfg_embed_sources;

        // C3/D6: embed_sources without source_map is a no-op (applies PF-004 — all paths).
        if use_embed_sources && !use_source_map && !quiet {
            crate::output::ewriteln!(
                "warning: embed_sources has no effect without source maps; \
                 set build.source_map=true or pass --source-map"
            );
        }

        return run_build_directory(
            &input,
            out_dir,
            resolved,
            quiet,
            use_source_map,
            use_embed_sources,
            inline,
        );
    }

    // The vars, and the `--vars` file they came from, which no output is written over
    // (#425).
    let RuntimeVars {
        vars: runtime_vars,
        vars_file,
        ..
    } = resolved;

    // ── Stdin path ───────────────────────────────────────────────────────────────

    if input == Path::new("-") {
        // Stdin: no project config; effective flags from CLI only.
        let use_source_map = flag_source_map && !no_source_map;
        let use_embed_sources = flag_embed_sources;

        // C3/D6: embed_sources without source_map is a no-op; warn so users don't wonder
        // why their output contains no sourcesContent (applies PF-004 — both paths checked).
        if use_embed_sources && !use_source_map && !quiet {
            crate::output::ewriteln!(
                "warning: embed_sources has no effect without source maps; \
                 set build.source_map=true or pass --source-map"
            );
        }

        // AC-SEC-02: warn when shipping full source text in a distributable artifact.
        if use_embed_sources && inline && !quiet {
            crate::output::ewriteln!(
                "warning: --embed-sources with --inline ships full source text in the output \
                 (AC-SEC-02)"
            );
        }

        let source_map_base = use_source_map
            .then(|| compute_source_map_base(Path::new("-"), &output, &out_dir, &None))
            .transpose()?;
        let opts = mds::CompileOptions::default()
            .with_source_map(use_source_map)
            .with_include_sources_content(use_embed_sources)
            .with_source_map_base(source_map_base);

        let source = read_stdin()?;
        // AD-211-1 / AD-211-5: same stdin relabel as `compile_to_content`; `None` is the
        // working directory, shown as "." (see `read_stdin`).
        let result = mds::compile_str_with_deps_opts(&source, None, runtime_vars, opts)
            .map_err(|e| crate::output::relabel_stdin_error(&e, &source))?;
        if !quiet {
            for w in &result.warnings {
                crate::output::eprint_warning(w);
            }
        }

        let mut source_map = result.source_map;
        let kind = OutputKind::from(&result.output);
        let content = serialize_output(result.output)?;
        let inputs = compile_inputs(
            None,
            &result.dependencies,
            &run_reads(vars_file.as_deref(), None),
        );

        // Stdin: no project config; output path follows -o flag or defaults to stdout.
        // There is no entry file to refuse (#425), so the `-o` warning is printed now.
        let output_path = resolve_output_path_for_kind(
            Some(EntryPaths::as_typed(&input)),
            &output,
            &out_dir,
            &None,
            kind,
        )?;
        warn_output_extension_mismatch(&output, kind, quiet);

        if let Some(ref mut sm) = source_map {
            // Set `file` field and relabel source entry for stdin builds (AC-FUNC-12).
            apply_source_map_file_label(sm, output_path.as_ref().map(|t| t.path.as_path()), true);
        }

        if use_source_map {
            if inline {
                // Inline: embed carrier into the output content, then write.
                let map_json = source_map.as_ref().map(|sm| sm.to_json());
                let final_content = if let Some(ref json) = map_json {
                    embed_carrier(content, json)
                } else {
                    content
                };
                return write_output(output_path.as_ref(), &final_content, &inputs, quiet, true);
            } else {
                // Sidecar: write output first (byte-identical to no-flag; ADR-002).
                match &output_path {
                    None => {
                        // Sidecar + stdout: three-case ladder (stdin has no config, so cases 1–2).
                        if source_map.is_none() {
                            // Case 1 — messages mode: compiler produced no map; write to stdout.
                            return write_output(None, &content, &inputs, quiet, true);
                        }
                        // Case 2 — explicit --source-map flag: hard error.
                        return Err(miette::miette!(
                            "--source-map (sidecar) requires an output file; use -o <file> or \
                             --out-dir, --inline to embed the map for stdout, or --no-source-map \
                             to skip it"
                        ));
                    }
                    Some(out) => {
                        write_output(Some(out), &content, &inputs, quiet, true)?;
                        if let Some(ref sm) = source_map {
                            let map = out.sibling(map_path_for);
                            // The sidecar's `file` / `sources` / `sourcesContent` are
                            // written VERBATIM, by decision — spec §7.5 "Carve-out:
                            // functional path references". They are resolved against the
                            // filesystem by devtools and bundlers, so a `\uXXXX`-escaped
                            // path would not exist. Consumers must treat them as
                            // untrusted; see the `mds::SourceMap` rustdoc. The status
                            // line below is a diagnostic surface and IS escaped.
                            let map_json = sm.to_json();
                            write_compiled(&map, &map_json, &inputs)?;
                            if !quiet {
                                crate::output::ewriteln!(
                                    "Source map written to {}",
                                    crate::output::safe_path(&map.shown)
                                );
                            }
                        }
                        return Ok(());
                    }
                }
            }
        }

        return write_output(output_path.as_ref(), &content, &inputs, quiet, true);
    }

    // ── File input path ──────────────────────────────────────────────────────────

    // File input: load project config and compute effective flags.
    let config = load_config(&input)?;
    let cfg_source_map = config
        .as_ref()
        .map(|c| c.config.build.source_map)
        .unwrap_or(false);
    let cfg_embed_sources = config
        .as_ref()
        .map(|c| c.config.build.embed_sources)
        .unwrap_or(false);

    let use_source_map = (flag_source_map || cfg_source_map) && !no_source_map;
    let use_embed_sources = flag_embed_sources || cfg_embed_sources;

    // C3/D6: embed_sources without source_map is a no-op; warn so users don't wonder
    // why their output contains no sourcesContent (applies PF-004 — both paths checked).
    if use_embed_sources && !use_source_map && !quiet {
        crate::output::ewriteln!(
            "warning: embed_sources has no effect without source maps; \
             set build.source_map=true or pass --source-map"
        );
    }

    // AC-SEC-02: warn when shipping full source text in a distributable artifact.
    if use_embed_sources && inline && !quiet {
        crate::output::ewriteln!(
            "warning: --embed-sources with --inline ships full source text in the output \
             (AC-SEC-02)"
        );
    }

    let source_map_base = use_source_map
        .then(|| compute_source_map_base(&input, &output, &out_dir, &config))
        .transpose()?;
    let opts = mds::CompileOptions::default()
        .with_source_map(use_source_map)
        .with_include_sources_content(use_embed_sources)
        .with_source_map_base(source_map_base);

    let compiled = compile_to_content(&input, runtime_vars, quiet, opts)?;
    let inputs = compile_inputs(
        Some(&input),
        &compiled.dependencies,
        &run_reads(vars_file.as_deref(), config.as_ref()),
    );
    // `mds build` holds only the typed path: the output is resolved from it, and
    // `admit_output` canonicalizes both sides itself.
    let entry = EntryPaths::as_typed(&input);
    let output_path =
        resolve_output_path_for_kind(Some(entry), &output, &out_dir, &config, compiled.kind)?;
    // #425: nothing — output or sidecar map — is written once the output is the entry,
    // and no warning announces that write.
    let written_path = output_path.as_ref().map(|t| t.path.as_path());
    admit_output(written_path, entry, &output, compiled.kind, quiet)
        .map_err(miette::Error::from)?;

    let mut source_map = compiled.source_map;
    if let Some(ref mut sm) = source_map {
        apply_source_map_file_label(sm, written_path, false);
    }

    if use_source_map {
        if inline {
            // Inline: embed carrier into the output, then write (ADR-002 not applicable:
            // inline mode intentionally modifies the output file).
            let map_json = source_map.as_ref().map(|sm| sm.to_json());
            let final_content = if let Some(ref json) = map_json {
                embed_carrier(compiled.content, json)
            } else {
                compiled.content
            };
            write_output(output_path.as_ref(), &final_content, &inputs, quiet, true)?;

            // Stale sidecar reconciliation: if there is an existing .map file from
            // a prior sidecar build, remove it (AC-FUNC-10).
            if let Some(ref out) = output_path {
                verify_then_delete_map(&out.sibling(map_path_for), &file_name_of(out), quiet)?;
            }
        } else {
            // Sidecar: write output byte-identical to no-flag build (ADR-002).
            match &output_path {
                None => {
                    // Sidecar + stdout: three-case ladder.
                    if source_map.is_none() {
                        // Case 1 — messages mode: compiler produced no map; write to stdout.
                        return write_output(None, &compiled.content, &inputs, quiet, true);
                    }
                    if flag_source_map {
                        // Case 2 — explicit --source-map flag: hard error.
                        return Err(miette::miette!(
                            "--source-map (sidecar) requires an output file; use -o <file> or \
                             --out-dir, --inline to embed the map for stdout, or --no-source-map \
                             to skip it"
                        ));
                    }
                    // Case 3 — config-sourced source_map: degrade gracefully. The warning
                    // names `mds.json` as the input reached it, as a config error does
                    // (#390).
                    if !quiet {
                        let cfg_path = config
                            .as_ref()
                            .map(ProjectConfig::shown_file)
                            .unwrap_or_else(|| PathBuf::from("mds.json"));
                        crate::output::ewriteln!(
                            "warning: source_map in {} has no effect when writing to \
                             stdout (sidecar requires -o <file> or --out-dir); use --inline to \
                             embed the map, or --no-source-map to silence this warning",
                            crate::output::safe_path(&cfg_path)
                        );
                    }
                    return write_output(None, &compiled.content, &inputs, quiet, true);
                }
                Some(out) => {
                    write_output(Some(out), &compiled.content, &inputs, quiet, true)?;
                    if let Some(ref sm) = source_map {
                        let map = out.sibling(map_path_for);
                        let map_json = sm.to_json();
                        write_compiled(&map, &map_json, &inputs)?;
                        if !quiet {
                            crate::output::ewriteln!(
                                "Source map written to {}",
                                crate::output::safe_path(&map.shown)
                            );
                        }
                    }
                }
            }
        }
    } else {
        write_output(
            output_path.as_ref(),
            &compiled.content,
            &inputs,
            quiet,
            true,
        )?;

        // No-source-map build: if a stale sidecar exists from a prior source-map build,
        // remove it (AC-FUNC-10).
        if let Some(ref out) = output_path {
            verify_then_delete_map(&out.sibling(map_path_for), &file_name_of(out), quiet)?;
        }
    }

    Ok(())
}

/// The file name of `output`, the name its sidecar map records as `file` — lossy, as
/// [`has_sidecar_head`] compares it.
fn file_name_of(output: &WriteTarget) -> String {
    output
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
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
    use super::stdio::tests::Stream;
    use super::*;

    /// #425: `admit_output` refuses an output that is the entry file and names the entry
    /// by `typed`, never by `canonical`; any other output is admitted, stdout included.
    /// Every write site — `mds build`, `mds watch`'s startup compile, its startup
    /// fallback and every rebuild — passes the entry as one `EntryPaths`, so none of them
    /// can hand the two forms over swapped.
    #[test]
    fn admit_output_refuses_naming_the_entry_as_typed() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("page.md"), "x").unwrap();
        let typed = dir.path().join("sub").join("..").join("page.md");
        let canonical = dir.path().join("page.md").canonicalize().unwrap();
        let entry = EntryPaths {
            typed: &typed,
            canonical: &canonical,
        };
        let admit = |output: Option<&Path>| {
            admit_output(output, entry, &None, OutputKind::Markdown, true)
                .map_err(|e| e.to_string())
        };

        let refused = admit(Some(&canonical)).unwrap_err();
        assert_eq!(
            refused,
            format!(
                "output would overwrite the entry file: \"{}\"; \
                 write it elsewhere with -o <file> or --out-dir <dir>",
                typed.display()
            )
        );
        assert!(
            !refused.contains(&*canonical.to_string_lossy()),
            "{refused}"
        );
        assert_eq!(
            admit(Some(&dir.path().join("sub").join("..").join("page.md"))),
            Err(refused),
            "another spelling of the entry is the entry"
        );

        assert_eq!(admit(Some(&dir.path().join("out.md"))), Ok(()), "control");
        assert_eq!(admit(None), Ok(()), "stdout is never the entry");
    }

    /// #425: an output whose directory does not exist yet is the file the write will
    /// reach once `write_output` has created that directory. A created directory is a
    /// plain one, so a `..` after it leads back to its parent — the entry's directory
    /// here — while an existing component is followed as the write follows it, a
    /// symlink included. The name is then looked up in the directory reached, so on a
    /// case-insensitive volume a case variant of the entry's name is the entry there
    /// too, however that directory was reached. Nothing is created by asking.
    #[test]
    fn file_identity_resolves_a_directory_the_write_creates() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("page.md"), "x").unwrap();
        let page = root.join("page.md");
        let case_insensitive = root.join("PAGE.md").exists();
        // A case variant is the entry on a case-insensitive volume, another file on a
        // case-sensitive one: each volume holds the variant to its own answer.
        let variant = if case_insensitive {
            page.clone()
        } else {
            root.join("PAGE.md")
        };

        let rows = [
            ("page.md", page.clone()),
            ("sub/../page.md", page.clone()),
            ("newdir/../page.md", page.clone()),
            ("a/b/../../page.md", page.clone()),
            ("sub/new/../../page.md", page.clone()),
            ("new/./x/../../page.md", page.clone()),
            ("newdir/page.md", root.join("newdir").join("page.md")),
            (
                "sub/new/page.md",
                root.join("sub").join("new").join("page.md"),
            ),
            ("PAGE.md", variant.clone()),
            ("newdir/../PAGE.md", variant.clone()),
            ("a/b/../../PAGE.md", variant),
        ];
        // `dir.path()` is not canonical on macOS (`/var` → `/private/var`), so the
        // existing part of each path is canonicalized, not merely joined.
        let mismatches: Vec<String> = rows
            .iter()
            .filter_map(|(rel, expected)| {
                let got = file_identity(&dir.path().join(rel));
                (got.as_ref() != Some(expected)).then(|| format!("{rel}: {got:?}"))
            })
            .collect();
        assert!(mismatches.is_empty(), "{mismatches:#?}");

        #[cfg(unix)]
        {
            // Out of a directory the write creates, then through an existing link that
            // leads back to the entry's directory: the link is followed.
            std::os::unix::fs::symlink(&root, root.join("lnk")).unwrap();
            assert_eq!(
                file_identity(&dir.path().join("newdir/../lnk/page.md")),
                Some(page.clone())
            );
        }
        assert!(!root.join("newdir").exists() && !root.join("a").exists());
    }

    // ── compute_source_map_base ───────────────────────────────────────────────
    //
    // `source_map_base` is the anchor core's `relativize_source` uses to emit
    // map-relative `sources[]` (ADR-005).  Core resolves it against an ABSOLUTE
    // project root, so a relative base fails the containment check and silently
    // demotes the result to root-relative — a `sources[]` entry that no longer
    // resolves from the map file's directory.  The invariant is therefore
    // "every branch returns an absolute path", asserted here per branch because
    // the failure mode is silent (wrong paths, not an error).

    #[test]
    fn source_map_base_is_absolute_for_every_output_mode() {
        let input = PathBuf::from("src/a.mds");
        let cases: Vec<(&str, Option<String>, Option<PathBuf>)> = vec![
            ("-o - (stdout)", Some("-".to_string()), None),
            (
                "-o relative/out.md",
                Some("relative/out.md".to_string()),
                None,
            ),
            ("-o bare.md", Some("bare.md".to_string()), None),
            ("-o /abs/out.md", Some("/abs/out.md".to_string()), None),
            ("--out-dir relative", None, Some(PathBuf::from("dist"))),
            ("--out-dir /abs", None, Some(PathBuf::from("/abs/dist"))),
            ("default (beside source)", None, None),
        ];
        for (label, output, out_dir) in cases {
            let got = compute_source_map_base(&input, &output, &out_dir, &None)
                .unwrap_or_else(|e| panic!("{label}: the working directory exists: {e}"));
            assert!(
                got.is_absolute(),
                "{label}: source_map_base must be absolute so core's root-containment \
                 check succeeds; got {got:?}"
            );
        }
    }

    #[test]
    fn source_map_base_for_bare_filename_output_is_cwd() {
        // PF-006: Path::parent() of a bare filename is Some("") not None, so a
        // naive parent() would yield an empty (relative) base here.
        let got = compute_source_map_base(
            Path::new("src/a.mds"),
            &Some("out.md".to_string()),
            &None,
            &None,
        )
        .expect("bare -o must still produce a base");
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            got, cwd,
            "`-o out.md` writes into the CWD, so the map base is the CWD"
        );
    }

    #[test]
    fn source_map_base_tracks_the_output_file_directory() {
        // The map is written beside the output file, so the base must be the
        // output file's directory — this is what makes sources[] map-relative.
        let got = compute_source_map_base(
            Path::new("src/a.mds"),
            &Some("dist/nested/a.md".to_string()),
            &None,
            &None,
        )
        .expect("base must be Some");
        assert_eq!(got, std::env::current_dir().unwrap().join("dist/nested"));
    }

    // ── AC-SEC-03: inline carrier is a self-contained, idempotent HTML comment ──

    #[test]
    fn embed_carrier_is_idempotent() {
        // Re-embedding must strip the existing carrier first, so applying it
        // twice yields the same bytes as applying it once (a distributable
        // artifact must never accumulate stacked carriers).
        let json = r#"{"version":3,"sources":["a.mds"],"mappings":"AAAA"}"#;
        let once = embed_carrier("Hello world\n".to_string(), json);
        let twice = embed_carrier(once.clone(), json);
        assert_eq!(
            once, twice,
            "embedding the carrier twice must be idempotent"
        );
        assert_eq!(
            twice.matches("sourceMappingURL=data:").count(),
            1,
            "the carrier must appear exactly once, got:\n{twice}"
        );
    }

    #[test]
    fn carrier_line_cannot_break_out_of_html_comment() {
        // AC-SEC-03: the base64 payload must exclude '<', '>', and '-' so it
        // cannot terminate the enclosing HTML comment early, even when the map
        // JSON itself contains '-->' (e.g. embedded source text).
        let hostile_json = r#"{"sourcesContent":["--> break out <script>"]}"#;
        let line = carrier_line(hostile_json);
        let payload = line
            .strip_prefix("<!--# sourceMappingURL=data:application/json;base64,")
            .and_then(|s| s.strip_suffix(" -->"))
            .expect("carrier must have the expected envelope");
        assert!(!payload.contains('<'), "payload must not contain '<'");
        assert!(!payload.contains('>'), "payload must not contain '>'");
        assert!(!payload.contains('-'), "payload must not contain '-'");
    }

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

    /// #428: the stale-map check reads only the bytes a sidecar mds wrote starts with —
    /// `SourceMap::to_json` writes `version`, then `file`, then `sources` — and deletes
    /// nothing else: a map written for the output (its name escaped as JSON, or not) is
    /// recognised, while one for another output, a hand-formatted one with the same
    /// fields, one that ends after `file`, and a large file of anything else are not,
    /// and the large one is never read whole.
    #[test]
    fn stale_map_check_reads_only_the_sidecar_head() {
        let sidecar = |name: &str| {
            let mut sm = mds::compile_str_with_deps_opts(
                "Hi\n",
                None,
                None,
                mds::CompileOptions::default().with_source_map(true),
            )
            .unwrap()
            .source_map
            .expect("a map was built");
            apply_source_map_file_label(&mut sm, Some(Path::new(name)), false);
            sm.to_json()
        };
        let verdict =
            |bytes: &str, name: &str| has_sidecar_head(&mut bytes.as_bytes(), name).unwrap();

        let mut mismatches = Vec::new();
        for name in ["out.md", "a \"quoted\" name.md", "caf\u{e9}.md"] {
            if !verdict(&sidecar(name), name) {
                mismatches.push(format!("{name}: its own sidecar is not recognised"));
            }
        }
        let rows = [
            ("another output's sidecar", sidecar("other.md")),
            (
                "hand-formatted, same fields",
                "{ \"version\": 3, \"file\": \"out.md\", \"sources\": [], \"names\": [], \"mappings\": \"\" }".to_owned(),
            ),
            ("ends after file", "{\"version\":3,\"file\":\"out.md\"}".to_owned()),
            ("version 2", "{\"version\":2,\"file\":\"out.md\",\"sources\":[]}".to_owned()),
        ];
        for (label, bytes) in rows {
            if verdict(&bytes, "out.md") {
                mismatches.push(format!("{label}: recognised as out.md's sidecar"));
            }
        }
        let head = "{\"version\":3,\"file\":\"out.md\",".len() as u64;
        let mut large = Stream::sized(64 * 1024 * 1024);
        if has_sidecar_head(&mut large, "out.md").unwrap() || large.served > head {
            mismatches.push(format!(
                "64 MiB of x: read {} bytes; the head is {head}",
                large.served
            ));
        }
        assert!(mismatches.is_empty(), "{mismatches:#?}");
    }

    /// #157: a stale sidecar that cannot be removed is an `mds::io` error, not a
    /// warning; the control removes the same sidecar from a writable directory.
    ///
    /// `#[cfg(unix)]`: the unlink failure comes from a `0o555`-mode directory, which
    /// Windows' read-only attribute does not reproduce.
    #[cfg(unix)]
    #[test]
    fn a_stale_map_that_cannot_be_removed_is_an_io_error() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let map = dir.path().join("out.md.map");
        let sidecar = "{\"version\":3,\"file\":\"out.md\",\"sources\":[],\"mappings\":\"\"}";

        // Control: a writable directory, and the sidecar is removed.
        std::fs::write(&map, sidecar).unwrap();
        assert!(
            verify_then_delete_map(&WriteTarget::as_typed(map.clone()), "out.md", true).is_ok()
        );
        assert!(!map.exists(), "control: the stale sidecar must be removed");

        std::fs::write(&map, sidecar).unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        let probe = std::fs::write(dir.path().join("probe"), b"");
        let result = verify_then_delete_map(&WriteTarget::as_typed(map.clone()), "out.md", true);
        let _ = std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755));
        if probe.is_ok() {
            crate::output::ewriteln!(
                "skipped: a file can be created at mode 0o555 (running as root?)"
            );
            return;
        }
        match result {
            Err(MdsError::Io { message }) => assert!(
                message.starts_with("could not remove stale map "),
                "unexpected message: {message}"
            ),
            other => panic!("want Err(MdsError::Io {{ .. }}); got {other:?}"),
        }
        assert!(
            map.exists(),
            "the sidecar that could not be removed is still there"
        );
    }

    /// #157: a stale `.map` that cannot be read is an `mds::io` error, not a "not a
    /// tool-generated map" warning — nothing is known about its content. The control: a
    /// readable map that is not a sidecar is left in place without an error.
    ///
    /// `#[cfg(unix)]`: the read failure comes from a `0o000`-mode file.
    #[cfg(unix)]
    #[test]
    fn a_stale_map_that_cannot_be_read_is_an_io_error() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let map = dir.path().join("out.md.map");

        // Control: readable, not a sidecar — left in place, no error.
        std::fs::write(&map, "{\"hand\":\"written\"}").unwrap();
        assert!(
            verify_then_delete_map(&WriteTarget::as_typed(map.clone()), "out.md", true).is_ok()
        );
        assert!(map.exists(), "control: a hand-written map is left in place");

        let sidecar = "{\"version\":3,\"file\":\"out.md\",\"sources\":[],\"mappings\":\"\"}";
        std::fs::write(&map, sidecar).unwrap();
        std::fs::set_permissions(&map, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&map).is_ok() {
            crate::output::ewriteln!(
                "skipped: a file can be read at mode 0o000 (running as root?)"
            );
            return;
        }
        let result = verify_then_delete_map(&WriteTarget::as_typed(map.clone()), "out.md", true);
        match result {
            Err(MdsError::Io { message }) => assert!(
                message.starts_with("cannot read stale map ") && message.contains("out.md.map"),
                "unexpected message: {message}"
            ),
            other => panic!("want Err(MdsError::Io {{ .. }}); got {other:?}"),
        }
        assert!(map.exists(), "a map that cannot be read is left in place");
    }

    /// #160: a stale map whose directory below the anchor is a symlink — `mds.json`'s
    /// `build.output_dir` puts directories there — is not removed through it: the removal
    /// is refused (`mds::io`), naming the map and the link as shown, and the sidecar of
    /// that name where the link leads is left. Control: the same sidecar below a real
    /// directory is removed.
    #[cfg(unix)]
    #[test]
    fn a_stale_map_is_never_removed_through_a_symlink_below_its_anchor() {
        let sidecar = "{\"version\":3,\"file\":\"x.md\",\"sources\":[],\"mappings\":\"\"}";
        let dir = tempfile::tempdir().unwrap();
        let anchor = dir.path().join("out");
        std::fs::create_dir_all(anchor.join("real")).unwrap();
        std::fs::create_dir(dir.path().join("victim")).unwrap();
        std::os::unix::fs::symlink("../victim", anchor.join("sub")).unwrap();
        let victim = dir.path().join("victim/x.md.map");
        std::fs::write(&victim, sidecar).unwrap();

        let map = WriteTarget::below(&anchor, Path::new("out"), Path::new("sub/x.md.map"));
        let result = verify_then_delete_map(&map, "x.md", true);
        assert_eq!(
            std::fs::read_to_string(&victim).ok().as_deref(),
            Some(sidecar),
            "nothing is removed through the symlink"
        );
        match result {
            Err(MdsError::Io { message }) => assert_eq!(
                message,
                "could not remove stale map out/sub/x.md.map: \
                 refusing to follow a symlink at out/sub"
            ),
            other => panic!("want Err(MdsError::Io {{ .. }}); got {other:?}"),
        }

        // Control: below a real directory, the sidecar is removed.
        let real = anchor.join("real/x.md.map");
        std::fs::write(&real, sidecar).unwrap();
        let map = WriteTarget::below(&anchor, Path::new("out"), Path::new("real/x.md.map"));
        assert!(verify_then_delete_map(&map, "x.md", true).is_ok());
        assert!(!real.exists(), "control: the stale sidecar is removed");
    }
}
