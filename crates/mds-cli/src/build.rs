//! Build subcommand implementation and shared compilation helpers.
//!
//! All helpers in this module are `pub(crate)` so that `watch.rs` can reuse them
//! without duplicating logic or bypassing resource limits.
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};

use mds::{CompiledOutput, MdsError, MAX_FILE_SIZE, MAX_TRAVERSAL_DEPTH};
use miette::Result;
use serde::Deserialize;

// ── Project config (mds.json) ─────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
pub(crate) struct MdsConfig {
    #[serde(default)]
    pub(crate) build: BuildConfig,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct BuildConfig {
    pub(crate) output_dir: Option<String>,
}

/// Maximum allowed size for `mds.json` (1 MB) to prevent runaway memory use.
const MAX_CONFIG_SIZE: u64 = 1024 * 1024;

/// Walk up from `start` looking for `mds.json`.
///
/// Returns `Ok(Some((config, config_dir)))` when found, `Ok(None)` when no
/// `mds.json` exists in the hierarchy, or `Err(...)` when a file is found but
/// contains invalid JSON.
///
/// The `config_dir` is the directory that *contains* `mds.json` — used to
/// resolve relative `output_dir` values.
pub(crate) fn load_config(start: &Path) -> Result<Option<(MdsConfig, PathBuf)>> {
    // Walk upward from `start` (which may be a file; begin at its parent).
    let start_dir = if start.is_dir() {
        start.to_path_buf()
    } else {
        start
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    };

    let mut current = start_dir;
    // Cap prevents unbounded traversal on unusual filesystems.
    for _ in 0..MAX_TRAVERSAL_DEPTH {
        let candidate = current.join("mds.json");
        if candidate.is_file() {
            // Read the file first, then check size — avoids a TOCTOU race between
            // a separate metadata() call and the actual read().
            let bytes = std::fs::read(&candidate)
                .map_err(|e| miette::miette!("cannot read {}: {e}", candidate.display()))?;
            if bytes.len() as u64 > MAX_CONFIG_SIZE {
                return Err(miette::miette!(
                    "mds.json at {} is too large ({} bytes; maximum is 1 MB)",
                    candidate.display(),
                    bytes.len()
                ));
            }
            let raw = String::from_utf8(bytes)
                .map_err(|e| miette::miette!("invalid UTF-8 in {}: {e}", candidate.display()))?;
            let config: MdsConfig = serde_json::from_str(&raw)
                .map_err(|e| miette::miette!("invalid mds.json at {}: {e}", candidate.display()))?;
            return Ok(Some((config, current)));
        }
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => break,
        }
    }
    Ok(None)
}

// ── Output path resolution ────────────────────────────────────────────────────

/// The output kind, derived intrinsically from the compiled output variant.
///
/// This is separate from `CompiledOutput` so callers can carry the kind
/// through the single-file path-derivation pipeline without the actual content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputKind {
    Markdown,
    Messages,
}

impl OutputKind {
    /// File extension for this kind (without leading `.`).
    pub(crate) fn extension(self) -> &'static str {
        match self {
            OutputKind::Markdown => "md",
            OutputKind::Messages => "json",
        }
    }

    /// Extension of the *other* kind — used to identify a stale sibling to remove
    /// after a format flip (Markdown → remove stale `.json`; Messages → remove stale `.md`).
    ///
    /// Kept adjacent to `extension()` so the two stay in sync when a new kind is added.
    pub(crate) fn stale_extension(self) -> &'static str {
        match self {
            OutputKind::Markdown => "json", // we just wrote .md → stale is .json
            OutputKind::Messages => "md",   // we just wrote .json → stale is .md
        }
    }
}

impl From<&CompiledOutput> for OutputKind {
    fn from(output: &CompiledOutput) -> Self {
        match output {
            CompiledOutput::Markdown(_) => OutputKind::Markdown,
            CompiledOutput::Messages(_) => OutputKind::Messages,
        }
    }
}

/// Derive the output filename by replacing the extension with the kind-appropriate extension.
///
/// - Markdown: `foo.mds` → `foo.md`
/// - Messages: `foo.mds` → `foo.json`
pub(crate) fn derive_output_filename_for_kind(input: &Path, kind: OutputKind) -> OsString {
    let stem = input.file_stem().unwrap_or(input.as_os_str());
    let mut name = OsString::from(stem);
    name.push(".");
    name.push(kind.extension());
    name
}

/// Compute `dir/<derived-name>.<ext>` WITHOUT creating the directory.
///
/// `input_path` drives the filename: if `Some`, the stem is reused (e.g. `foo.mds` → `foo.md`);
/// if `None` (stdin), the fallback name is `output.md` for markdown, `output.json` for messages.
///
/// Use [`prepare_output_dir_for_kind`] when the directory also needs to be created.
pub(crate) fn compute_output_dir_path_for_kind(
    dir: &Path,
    input_path: Option<&Path>,
    kind: OutputKind,
) -> PathBuf {
    let filename = input_path
        .map(|p| derive_output_filename_for_kind(p, kind))
        .unwrap_or_else(|| {
            OsString::from(match kind {
                OutputKind::Markdown => "output.md",
                OutputKind::Messages => "output.json",
            })
        });
    dir.join(filename)
}

/// Create `dir` (if absent) and return `dir/<derived-name>.<ext>`.
///
/// Extension is determined by `kind` (markdown → `.md`, messages → `.json`).
/// `input_path` drives the filename stem: if `Some`, the stem is reused;
/// if `None` (stdin), the fallback is `output.md` / `output.json`.
pub(crate) fn prepare_output_dir_for_kind(
    dir: &Path,
    input_path: Option<&Path>,
    kind: OutputKind,
) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)
        .map_err(|e| miette::miette!("cannot create output directory {}: {e}", dir.display()))?;
    Ok(compute_output_dir_path_for_kind(dir, input_path, kind))
}

/// Resolve the output path according to the precedence chain (kind-aware variant).
///
/// Any required output directory is created via `create_dir_all`.
///
/// Precedence:
/// 1. `-o -`                         → stdout (returns `None`)
/// 2. `-o <path>`                    → that exact path (verbatim; warn on ext mismatch)
/// 3. Stdin with no -o / --out-dir   → stdout (returns `None`)
/// 4. `--out-dir <dir>`              → `<dir>/<name>.<ext>` (ext from kind)
/// 5. `mds.json`                     → `<config_dir>/<output_dir>/<name>.<ext>` (ext from kind)
/// 6. Default                        → source dir + `<name>.<ext>` (ext from kind)
///
/// For rules 4–6 the extension is derived from `kind` (markdown → `.md`, messages → `.json`).
/// For rule 2 (`-o <path>`), the path is used verbatim; if its extension conflicts with `kind`
/// a warning is emitted to stderr (AC-FUNC-11: write still proceeds to the requested path).
pub(crate) fn resolve_output_path_for_kind(
    input: &Option<PathBuf>,
    output: &Option<String>,
    out_dir: &Option<PathBuf>,
    config: &Option<(MdsConfig, PathBuf)>,
    kind: OutputKind,
    quiet: bool,
) -> Result<Option<PathBuf>> {
    // 1 & 2. Explicit `-o` flag: `-` means stdout, anything else is a literal path.
    match output.as_deref() {
        Some("-") => return Ok(None),
        Some(o) => {
            let path = PathBuf::from(o);
            // AC-FUNC-11: warn when the extension contradicts the kind.
            if !quiet {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    let expected = kind.extension();
                    if ext != expected {
                        eprintln!(
                            "warning: output path '{o}' has extension '.{ext}' but compiled \
                             output is {kind_name}; writing to '{o}' anyway",
                            kind_name = match kind {
                                OutputKind::Markdown => "markdown (.md)",
                                OutputKind::Messages => "messages JSON (.json)",
                            }
                        );
                    }
                }
            }
            return Ok(Some(path));
        }
        None => {}
    }

    // Derive the output filename from the input path (needed for steps 3-6).
    // Treat stdin ("-") as None so we fall back to "output.md/json" instead of "-.md".
    let input_path = input.as_deref().filter(|p| *p != Path::new("-"));

    // 3. Stdin input with no explicit output destination → stdout.
    //    But if --out-dir is set, fall through so the user's explicit CLI flag
    //    is honored (using "output.md/json" as the derived filename).
    if input_path.is_none() && out_dir.is_none() {
        return Ok(None);
    }

    // 4. `--out-dir <dir>`
    if let Some(dir) = out_dir {
        return Ok(Some(prepare_output_dir_for_kind(dir, input_path, kind)?));
    }

    // 5. `mds.json` output_dir
    if let Some((cfg, config_dir)) = config {
        if let Some(ref output_dir) = cfg.build.output_dir {
            // Reject path traversal: `output_dir` must not contain `..` components.
            // We check raw path components rather than canonicalizing because the
            // directory may not exist yet (it gets created by create_dir_all below).
            let traversal = Path::new(output_dir)
                .components()
                .any(|c| c == std::path::Component::ParentDir);
            if traversal {
                return Err(miette::miette!(
                    "mds.json output_dir '{}' must not contain '..' components",
                    output_dir
                ));
            }
            let dir = config_dir.join(output_dir);
            return Ok(Some(prepare_output_dir_for_kind(&dir, input_path, kind)?));
        }
    }

    // 6. Default: file next to source, with kind-derived extension.
    match input_path {
        Some(p) => {
            let filename = derive_output_filename_for_kind(p, kind);
            let dir = p.parent().unwrap_or(Path::new("."));
            Ok(Some(dir.join(filename)))
        }
        // Should not reach here (auto-detect always sets Some), but stdout as safe fallback.
        None => Ok(None),
    }
}

// ── Key-value parsing ─────────────────────────────────────────────────────────

pub(crate) fn parse_key_value(s: &str) -> std::result::Result<(String, String), String> {
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid KEY=VALUE: no '=' found in '{s}'"))?;
    Ok((s[..pos].to_string(), s[pos + 1..].to_string()))
}

/// Coerce a CLI `--set KEY=VALUE` string to the most specific typed Value.
///
/// Matches the ergonomics of YAML frontmatter parsing: `true`/`false` become
/// booleans, integer and float literals become numbers, `null` becomes Null,
/// and bracket-delimited lists become arrays.  Everything else stays a string.
pub(crate) fn parse_cli_value(val: String) -> mds::Value {
    // Keywords first.
    match val.as_str() {
        "true" => return mds::Value::Boolean(true),
        "false" => return mds::Value::Boolean(false),
        "null" => return mds::Value::Null,
        _ => {}
    }

    // Integer — parse as i64 so we don't accept "1e3" (scientific notation) here;
    // then widen to f64 for storage.
    if let Ok(n) = val.parse::<i64>() {
        return mds::Value::Number(n as f64);
    }

    // Float — accept decimal fractions like "3.14".
    // Reject non-finite values (NaN, Infinity, -Infinity) — fall through to string.
    if let Ok(f) = val.parse::<f64>() {
        if f.is_finite() {
            return mds::Value::Number(f);
        }
    }

    // Simple bracket-list: "[a, b, c]" → Array of strings.
    // Only handles flat lists of unquoted tokens; does not recurse.
    if val.starts_with('[') && val.ends_with(']') {
        let inner = &val[1..val.len() - 1];
        if inner.trim().is_empty() {
            return mds::Value::Array(vec![]);
        }
        let items: Vec<mds::Value> = inner
            .split(',')
            .map(|s| mds::Value::String(s.trim().to_string()))
            .collect();
        return mds::Value::Array(items);
    }

    mds::Value::String(val)
}

/// Map an error to a categorized exit code.
///
/// Exit codes:
/// - 0: success (never returned here — handled by happy path)
/// - 1: logical/syntax error (undefined variable, arity mismatch, recursion, etc.)
/// - 2: I/O or file-system error (file not found, not an MDS file, I/O failure)
/// - 3: resource limit exceeded (output too large, too many iterations)
///
/// Errors created via `miette::miette!()` do NOT downcast to `MdsError`
/// and correctly fall through to exit code 1. Only `MdsError` values converted via
/// `.map_err(miette::Error::from)` are categorized.
pub(crate) fn exit_code(err: &miette::Error) -> i32 {
    if let Some(mds_err) = err.downcast_ref::<MdsError>() {
        match mds_err {
            MdsError::Io { .. } | MdsError::FileNotFound { .. } | MdsError::NotMdsFile { .. } => 2,
            MdsError::ResourceLimit { .. } => 3,
            _ => 1,
        }
    } else {
        1
    }
}

// ── Runtime vars helpers ──────────────────────────────────────────────────────

/// Load vars from an optional file path, returning None if no file was given.
pub(crate) fn load_optional_vars_file(
    path: Option<PathBuf>,
) -> Result<Option<HashMap<String, mds::Value>>> {
    path.map(|p| mds::load_vars_file(&p).map_err(|e| miette::miette!("{e}")))
        .transpose()
}

/// Merge a `--vars` file with any `--set key=value` overrides into a single map.
pub(crate) fn build_runtime_vars(
    vars: Option<PathBuf>,
    set_vars: Vec<(String, String)>,
) -> Result<Option<HashMap<String, mds::Value>>> {
    let mut runtime_vars = load_optional_vars_file(vars)?;
    for (key, val) in set_vars {
        runtime_vars
            .get_or_insert_with(HashMap::new)
            .insert(key, parse_cli_value(val));
    }
    Ok(runtime_vars)
}

/// Read from stdin and return the source string along with the current working directory.
///
/// Reads at most `MAX_FILE_SIZE + 1` bytes so we can detect over-sized input without
/// buffering the entire stream first.
pub(crate) fn read_stdin() -> Result<(String, PathBuf)> {
    let mut source = String::new();
    std::io::stdin()
        .take(MAX_FILE_SIZE + 1)
        .read_to_string(&mut source)
        .map_err(|e| miette::miette!("cannot read stdin: {e}"))?;
    if source.len() as u64 > MAX_FILE_SIZE {
        return Err(miette::miette!("stdin input exceeds maximum size of 10 MB"));
    }
    let cwd = std::env::current_dir()
        .map_err(|e| miette::miette!("cannot determine current directory: {e}"))?;
    Ok((source, cwd))
}

/// Write compiled output to a file or stdout.
///
/// When `output_path` is `Some(path)`, creates any missing parent directories,
/// writes the compiled string, and prints `"Compiled to {path}"` to stderr
/// unless `quiet` or `announce` is false.  When `output_path` is `None`,
/// prints the compiled string to stdout with no trailing newline.
///
/// Set `announce = false` in watch-loop rebuilds so only the `"Recompiled …"`
/// summary line is emitted (not a redundant `"Compiled to …"` line).
/// Set `announce = true` for the initial/startup compile and for `mds build`.
pub(crate) fn write_output(
    output_path: Option<PathBuf>,
    compiled: &str,
    quiet: bool,
    announce: bool,
) -> Result<()> {
    match output_path {
        Some(path) => {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        miette::miette!("cannot create output directory {}: {e}", parent.display())
                    })?;
                }
            }
            std::fs::write(&path, compiled)
                .map_err(|e| miette::miette!("cannot write {}: {e}", path.display()))?;
            if !quiet && announce {
                eprintln!("Compiled to {}", path.display());
            }
        }
        None => {
            use std::io::Write as _;
            print!("{compiled}");
            // Flush stdout so pipe consumers receive the content immediately.
            // This is a no-op when stdout is a file; on pipes it ensures the
            // bytes are not held in the libc/Rust buffer until the process exits.
            std::io::stdout()
                .flush()
                .map_err(|e| miette::miette!("cannot flush stdout: {e}"))?;
        }
    }
    Ok(())
}

/// Scan the current directory for `.mds` files.
///
/// Returns `Ok(path)` if exactly one `.mds` file is found, or an `Err` describing
/// why auto-detection failed (zero files, multiple files, or I/O error).
pub(crate) fn auto_detect_mds_file() -> Result<PathBuf> {
    let cwd = std::env::current_dir()
        .map_err(|e| miette::miette!("cannot determine current directory: {e}"))?;

    let entries: Vec<PathBuf> = std::fs::read_dir(&cwd)
        .map_err(|e| miette::miette!("cannot read directory {}: {e}", cwd.display()))?
        .filter_map(|res| {
            let path = res.ok()?.path();
            (path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("mds"))
                .then_some(path)
        })
        .collect();

    match entries.as_slice() {
        [] => Err(miette::miette!(
            "no .mds files found in current directory\n  \
             hint: run 'mds init' to create a starter template"
        )),
        [single] => Ok(single.clone()),
        _ => {
            let mut names: Vec<String> = entries
                .iter()
                .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_owned))
                .collect();
            names.sort();
            Err(miette::miette!(
                "multiple .mds files found: {}\n  \
                 hint: specify which file to compile, e.g. 'mds build {}'",
                names.join(", "),
                names.first().map(|s| s.as_str()).unwrap_or("<file>.mds"),
            ))
        }
    }
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
    /// Transitive dependency paths (empty when no `@import`s).
    pub(crate) dependencies: Vec<String>,
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
        CompiledOutput::Messages(msgs) => {
            let mut json = serde_json::to_string_pretty(&msgs)
                .map_err(|e| miette::miette!("failed to serialize messages to JSON: {e}"))?;
            json.push('\n');
            Ok(json)
        }
    }
}

/// Compile `input` and return the content + kind + deps WITHOUT writing any output.
///
/// The output kind (Markdown vs Messages) is determined intrinsically from the compiled
/// result — the caller does not specify it. This is the pure "compile" step used by the
/// watch loop for content-based dedup.
///
/// `build` and the initial watch compile use [`compile_and_write`], which calls
/// this internally and then always writes.
///
/// # PF-004 compliance
/// All file reads go through `mds::compile_with_deps` or `mds::compile_str_with_deps`
/// (which use the resolver that enforces MAX_FILE_SIZE). Stdin input is read through
/// `read_stdin` which enforces the same cap. There is no bare `std::fs::read_to_string`.
pub(crate) fn compile_to_content(
    input: &Path,
    runtime_vars: Option<HashMap<String, mds::Value>>,
    quiet: bool,
) -> Result<CompileOutput> {
    let result = if input == Path::new("-") {
        // Stdin: compile from source string using cwd as base_dir.
        // read_stdin enforces MAX_FILE_SIZE (PF-004).
        let (source, cwd) = read_stdin()?;
        mds::compile_str_with_deps(&source, Some(&cwd), runtime_vars)
            .map_err(miette::Error::from)?
    } else {
        // File path: compile_with_deps routes through the resolver which enforces
        // MAX_FILE_SIZE and check_symlink (PF-004 compliance).
        mds::compile_with_deps(input, runtime_vars).map_err(miette::Error::from)?
    };

    if !quiet {
        for w in &result.warnings {
            eprintln!("{w}");
        }
    }

    let kind = OutputKind::from(&result.output);
    // Move result.output into serialize_output so the Markdown arm avoids a clone
    // (the kind was already derived from the borrow above — issue 2).
    let content = serialize_output(result.output)?;
    Ok(CompileOutput {
        content,
        kind,
        dependencies: result.dependencies,
    })
}

/// Compile `input`, derive the output path from the compiled kind, and write.
///
/// Returns the resolved output path and the list of transitive deps.
///
/// The output path is derived AFTER compiling (compile-then-route) so the kind
/// (and thus extension: `.json` for messages, `.md` for markdown) is known before
/// the path is constructed. This is the single-file intrinsic extension path.
///
/// If `-o <path>` is given explicitly, that path is used verbatim and an ext-mismatch
/// warning is emitted when the extension contradicts the kind (AC-FUNC-11).
/// If `-o -` or stdin-with-no-flags, content is written to stdout.
///
/// # PF-004 compliance
/// All file reads go through `compile_to_content` → `mds::compile_with_deps` or
/// `mds::compile_str_with_deps` (which use the resolver that enforces MAX_FILE_SIZE).
/// There is no bare `std::fs::read_to_string` path here.
pub(crate) fn compile_and_write(
    input: &Path,
    output: &Option<String>,
    out_dir: &Option<PathBuf>,
    config: &Option<(MdsConfig, PathBuf)>,
    runtime_vars: Option<HashMap<String, mds::Value>>,
    quiet: bool,
) -> Result<(Option<PathBuf>, Vec<String>)> {
    let compiled = compile_to_content(input, runtime_vars, quiet)?;
    let output_path = resolve_output_path_for_kind(
        &Some(input.to_path_buf()),
        output,
        out_dir,
        config,
        compiled.kind,
        quiet,
    )?;
    write_output(output_path.clone(), &compiled.content, quiet, true)?;
    Ok((output_path, compiled.dependencies))
}

// ── Build args struct ─────────────────────────────────────────────────────────

pub(crate) struct BuildArgs {
    pub(crate) input: Option<PathBuf>,
    pub(crate) output: Option<String>,
    pub(crate) out_dir: Option<PathBuf>,
    pub(crate) vars: Option<PathBuf>,
    pub(crate) set_vars: Vec<(String, String)>,
    pub(crate) quiet: bool,
}

/// Resolve the input path: use the explicit value, or auto-detect from cwd.
///
/// Returns `(path, auto_detected)`.
pub(crate) fn resolve_input(input: Option<PathBuf>) -> Result<(PathBuf, bool)> {
    match input {
        Some(p) => Ok((p, false)),
        None => auto_detect_mds_file().map(|p| (p, true)),
    }
}

pub(crate) fn run_build(args: BuildArgs) -> Result<()> {
    let BuildArgs {
        input,
        output,
        out_dir,
        vars,
        set_vars,
        quiet,
    } = args;
    let runtime_vars = build_runtime_vars(vars, set_vars)?;

    // Resolve the input: explicit path, or auto-detect from cwd.
    // When auto-detected, print a "Building {path}" banner so users know which file was selected.
    let (input, auto_detected) = resolve_input(input)?;
    if auto_detected && !quiet {
        eprintln!("Building {}", input.display());
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
        // Reject a symlinked directory root for build parity (commit aa0c538).
        if input
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err(miette::miette!(
                "directory argument must not be a symlink: {}",
                input.display()
            ));
        }
        return run_build_directory(&input, out_dir, runtime_vars, quiet);
    }

    if input == Path::new("-") {
        // Stdin: compile from source string, route to stdout (or -o).
        // No project config for stdin; output direction follows -o/-o-.
        let (source, cwd) = read_stdin()?;
        let result = mds::compile_str_with_deps(&source, Some(&cwd), runtime_vars)
            .map_err(miette::Error::from)?;
        if !quiet {
            for w in &result.warnings {
                eprintln!("{w}");
            }
        }
        let kind = OutputKind::from(&result.output);
        // Move result.output (issue 2 — avoids clone for Markdown arm).
        let content = serialize_output(result.output)?;
        // Stdin: no project config; output path follows -o flag or defaults to stdout.
        let output_path =
            resolve_output_path_for_kind(&Some(input), &output, &out_dir, &None, kind, quiet)?;
        return write_output(output_path, &content, quiet, true);
    }

    // File input: load project config and compile.
    // compile-then-route: compile first, derive output path from kind, then write.
    let config = load_config(&input)?;
    let _ = compile_and_write(&input, &output, &out_dir, &config, runtime_vars, quiet)?;
    Ok(())
}

/// Compile every non-partial `.mds` file under `dir`, streaming one at a time
/// (AC-PERF-02: peak RSS ≈ O(largest single file), not O(total)).
///
/// Continue-on-error: a per-file compile error does NOT abort the run.
/// All valid files are written; a summary is printed; non-zero exit when any failed
/// (AC-FUNC-18).
///
/// Subtree mirroring: with `--out-dir`, mirrors the source subtree into the out-dir
/// with the intrinsic extension per file (AC-FUNC-16). Without `--out-dir`, each
/// output is placed next to its source (AC-FUNC-19).
///
/// Stale-output cleanup: after writing, probes for the wrong-extension sibling and
/// deletes it to handle format flips (md↔json) across builds.
fn run_build_directory(
    dir: &Path,
    out_dir: Option<PathBuf>,
    runtime_vars: Option<std::collections::HashMap<String, mds::Value>>,
    quiet: bool,
) -> Result<()> {
    use crate::output::{
        canonicalize_out_dir, collect_mds_files, is_partial, output_base_no_ext, output_path_for,
        probe_and_remove_stale, resolve_output_base, OutputBase,
    };

    const MAX_DEPTH: usize = 64;

    // Load project config from the directory root.
    let config = load_config(dir)?;
    // Canonicalize out_dir as absolute so starts_with checks are reliable.
    let abs_out_dir = canonicalize_out_dir(out_dir.as_ref());
    let output_base = resolve_output_base(abs_out_dir.as_deref(), &config)?;

    // Exclude the out-dir from collection when it is nested inside the source root.
    let exclude_prefix: Option<PathBuf> = match &output_base {
        OutputBase::Dir(d) if d.starts_with(dir) => Some(d.clone()),
        _ => None,
    };

    let files = collect_mds_files(dir, MAX_DEPTH, exclude_prefix.as_deref());

    if files.is_empty() {
        if !quiet {
            eprintln!("No .mds files found in {}", dir.display());
        }
        return Ok(());
    }

    let mut ok_count: usize = 0;
    let mut fail_count: usize = 0;

    for file in &files {
        // Skip partials: they contribute to imports but produce no standalone output.
        if is_partial(file) {
            continue;
        }

        // Compile (all reads go through mds-core which enforces MAX_FILE_SIZE — PF-004).
        match compile_to_content(file, runtime_vars.clone(), quiet) {
            Ok(compiled) => {
                let ext = compiled.kind.extension();
                let out_path = output_path_for(file, dir, &output_base, ext);

                // Ensure parent directory exists.
                if let Some(parent) = out_path.parent() {
                    if !parent.as_os_str().is_empty() {
                        if let Err(e) = std::fs::create_dir_all(parent) {
                            eprintln!(
                                "error: cannot create output directory {}: {e}",
                                parent.display()
                            );
                            fail_count += 1;
                            continue;
                        }
                    }
                }

                match std::fs::write(&out_path, &compiled.content) {
                    Ok(()) => {
                        if !quiet {
                            eprintln!("Compiled to {}", out_path.display());
                        }
                        // Stale-output cleanup: remove the wrong-extension sibling if it exists.
                        let base_no_ext = output_base_no_ext(file, dir, &output_base);
                        probe_and_remove_stale(&base_no_ext, compiled.kind);
                        ok_count += 1;
                    }
                    Err(e) => {
                        eprintln!("error: cannot write {}: {e}", out_path.display());
                        fail_count += 1;
                    }
                }
            }
            Err(e) => {
                eprintln!("{e:?}");
                fail_count += 1;
            }
        }
    }

    eprintln!("{ok_count} built, {fail_count} failed");

    if fail_count > 0 {
        std::process::exit(1);
    }
    Ok(())
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cli_value_nan_is_string() {
        // "NaN".parse::<f64>() succeeds but is not finite — must fall through to string.
        assert_eq!(
            parse_cli_value("NaN".to_string()),
            mds::Value::String("NaN".to_string()),
            "--set val=NaN must produce Value::String, not Value::Number(NaN)"
        );
    }

    #[test]
    fn parse_cli_value_infinity_is_string() {
        assert_eq!(
            parse_cli_value("Infinity".to_string()),
            mds::Value::String("Infinity".to_string()),
            "--set val=Infinity must produce Value::String, not Value::Number(inf)"
        );
    }

    #[test]
    fn parse_cli_value_neg_infinity_is_string() {
        assert_eq!(
            parse_cli_value("-Infinity".to_string()),
            mds::Value::String("-Infinity".to_string()),
            "--set val=-Infinity must produce Value::String, not Value::Number(-inf)"
        );
    }

    #[test]
    fn parse_cli_value_finite_float_is_number() {
        // Sanity check: legitimate floats still parse as numbers.
        // Use 2.5 (exact in binary) to avoid clippy::approx_constant warning.
        assert_eq!(
            parse_cli_value("2.5".to_string()),
            mds::Value::Number(2.5),
            "finite float must still become Value::Number"
        );
    }

    #[test]
    fn derive_output_filename_swaps_mds_extension() {
        assert_eq!(
            derive_output_filename_for_kind(Path::new("foo.mds"), OutputKind::Markdown),
            OsString::from("foo.md")
        );
    }

    #[test]
    fn derive_output_filename_preserves_compound_extension() {
        assert_eq!(
            derive_output_filename_for_kind(Path::new("foo.bar.mds"), OutputKind::Markdown),
            OsString::from("foo.bar.md")
        );
    }

    #[test]
    fn derive_output_filename_no_extension() {
        assert_eq!(
            derive_output_filename_for_kind(Path::new("README"), OutputKind::Markdown),
            OsString::from("README.md")
        );
    }

    #[test]
    fn derive_output_filename_other_extension() {
        assert_eq!(
            derive_output_filename_for_kind(Path::new("foo.txt"), OutputKind::Markdown),
            OsString::from("foo.md")
        );
    }

    #[test]
    fn resolve_output_path_dash_o_dash_is_stdout() {
        let result = resolve_output_path_for_kind(
            &Some(PathBuf::from("foo.mds")),
            &Some("-".to_string()),
            &None,
            &None,
            OutputKind::Markdown,
            true,
        )
        .unwrap();
        assert_eq!(result, None, "-o - should resolve to stdout (None)");
    }

    #[test]
    fn resolve_output_path_stdin_no_o_is_stdout() {
        let result = resolve_output_path_for_kind(
            &Some(PathBuf::from("-")),
            &None,
            &None,
            &None,
            OutputKind::Markdown,
            true,
        )
        .unwrap();
        assert_eq!(
            result, None,
            "stdin input with no -o should resolve to stdout"
        );
    }

    #[test]
    fn resolve_output_path_default_file_next_to_source() {
        let result = resolve_output_path_for_kind(
            &Some(PathBuf::from("/some/dir/hello.mds")),
            &None,
            &None,
            &None,
            OutputKind::Markdown,
            true,
        )
        .unwrap();
        assert_eq!(
            result,
            Some(PathBuf::from("/some/dir/hello.md")),
            "default should produce .md next to source"
        );
    }

    #[test]
    fn resolve_output_path_stdin_with_out_dir_uses_out_dir() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = dir.path().join("out");
        let result = resolve_output_path_for_kind(
            &Some(PathBuf::from("-")),
            &None,
            &Some(out_dir.clone()),
            &None,
            OutputKind::Markdown,
            true,
        )
        .unwrap();
        assert_eq!(
            result,
            Some(out_dir.join("output.md")),
            "stdin with --out-dir should produce output.md inside the out dir"
        );
    }

    #[test]
    fn resolve_output_path_explicit_o_wins_over_config() {
        let config = Some((
            MdsConfig {
                build: BuildConfig {
                    output_dir: Some("build".to_string()),
                },
            },
            PathBuf::from("/project"),
        ));
        let result = resolve_output_path_for_kind(
            &Some(PathBuf::from("/project/hello.mds")),
            &Some("out.md".to_string()),
            &None,
            &config,
            OutputKind::Markdown,
            true,
        )
        .unwrap();
        assert_eq!(
            result,
            Some(PathBuf::from("out.md")),
            "-o should win over mds.json config"
        );
    }
}
