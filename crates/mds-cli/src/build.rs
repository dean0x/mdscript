//! Build subcommand implementation and shared compilation helpers.
//!
//! All helpers in this module are `pub(crate)` so that `watch.rs` can reuse them
//! without duplicating logic or bypassing resource limits.
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};

use mds::{MdsError, MAX_FILE_SIZE, MAX_TRAVERSAL_DEPTH};
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

/// Derive the output filename by replacing the extension with `.md`.
///
/// Examples:
/// - `foo.mds`     → `foo.md`
/// - `foo.bar.mds` → `foo.bar.md`
/// - `README`      → `README.md`
/// - `foo.txt`     → `foo.md`
pub(crate) fn derive_output_filename(input: &Path) -> OsString {
    let stem = input.file_stem().unwrap_or(input.as_os_str());
    let mut name = OsString::from(stem);
    name.push(".md");
    name
}

/// Compute `dir/<derived-name>.md` WITHOUT creating the directory.
///
/// `input_path` drives the filename: if `Some`, the stem is reused (e.g. `foo.mds` → `foo.md`);
/// if `None` (stdin), the fallback name `output.md` is used.
///
/// Use [`prepare_output_dir`] when the directory also needs to be created.
pub(crate) fn compute_output_dir_path(dir: &Path, input_path: Option<&Path>) -> PathBuf {
    let filename = input_path
        .map(derive_output_filename)
        .unwrap_or_else(|| OsString::from("output.md"));
    dir.join(filename)
}

/// Create `dir` (if absent) and return `dir/<derived-name>.md`.
///
/// `input_path` drives the filename: if `Some`, the stem is reused (e.g. `foo.mds` → `foo.md`);
/// if `None` (stdin), the fallback name `output.md` is used.
pub(crate) fn prepare_output_dir(dir: &Path, input_path: Option<&Path>) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)
        .map_err(|e| miette::miette!("cannot create output directory {}: {e}", dir.display()))?;
    Ok(compute_output_dir_path(dir, input_path))
}

/// Resolve the output path according to the precedence chain.
///
/// Any required output directory is created via `create_dir_all`.
///
/// Precedence:
/// 1. `-o -`                         → stdout (returns `None`)
/// 2. `-o <path>`                    → that exact path
/// 3. Stdin with no -o / --out-dir   → stdout (returns `None`)
/// 4. `--out-dir <dir>`              → `<dir>/<name>.md`
/// 5. `mds.json`                     → `<config_dir>/<output_dir>/<name>.md`
/// 6. Default                        → source dir + `<name>.md`
pub(crate) fn resolve_output_path(
    input: &Option<PathBuf>,
    output: &Option<String>,
    out_dir: &Option<PathBuf>,
    config: &Option<(MdsConfig, PathBuf)>,
) -> Result<Option<PathBuf>> {
    // 1 & 2. Explicit `-o` flag: `-` means stdout, anything else is a literal path.
    match output.as_deref() {
        Some("-") => return Ok(None),
        Some(o) => return Ok(Some(PathBuf::from(o))),
        None => {}
    }

    // Derive the output filename from the input path (needed for steps 3-6).
    // Treat stdin ("-") as None so we fall back to "output.md" instead of "-.md".
    let input_path = input.as_deref().filter(|p| *p != Path::new("-"));

    // 3. Stdin input with no explicit output destination → stdout.
    //    But if --out-dir is set, fall through so the user's explicit CLI flag
    //    is honored (using "output.md" as the derived filename).
    if input_path.is_none() && out_dir.is_none() {
        return Ok(None);
    }

    // 4. `--out-dir <dir>`
    if let Some(dir) = out_dir {
        return Ok(Some(prepare_output_dir(dir, input_path)?));
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
            return Ok(Some(prepare_output_dir(&dir, input_path)?));
        }
    }

    // 6. Default: file next to source
    match input_path {
        Some(p) => {
            let filename = derive_output_filename(p);
            let dir = p.parent().unwrap_or(Path::new("."));
            Ok(Some(dir.join(filename)))
        }
        // Should not reach here (auto-detect always sets Some), but stdout as safe fallback.
        None => Ok(None),
    }
}

// ── Output format ─────────────────────────────────────────────────────────────

/// Output format for the `build` and `watch` commands.
#[derive(Debug, Default, Clone, PartialEq, clap::ValueEnum)]
pub(crate) enum OutputFormat {
    /// Render the template to Markdown text (default).
    #[default]
    Markdown,
    /// Compile `@message` blocks to a pretty-printed JSON array.
    Messages,
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

/// Return an error if the input path is a directory (only file or stdin allowed).
pub(crate) fn reject_directory_input(input: &Path) -> Result<()> {
    if input != Path::new("-") && input.is_dir() {
        return Err(miette::miette!(
            "expected a file, got a directory: {}",
            input.display()
        ));
    }
    Ok(())
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

/// Read the source string for compilation, handling both stdin and file inputs.
///
/// Returns `(source, base_dir)` where:
/// - `source` is the UTF-8 text of the input
/// - `base_dir` is the directory used to resolve relative imports (cwd for stdin, parent
///   dir of the file otherwise)
///
/// Enforces `MAX_FILE_SIZE` on file reads (PF-004: shared enforcement point — both
/// markdown and messages modes route through this function so neither can bypass the cap).
/// Stdin is already guarded by `read_stdin`.
pub(crate) fn read_build_input(input: &Path) -> Result<(String, PathBuf)> {
    if input == Path::new("-") {
        return read_stdin();
    }
    let path_str = input
        .to_str()
        .ok_or_else(|| miette::miette!("input path is not valid UTF-8"))?;
    let bytes = std::fs::read(input).map_err(|e| miette::miette!("cannot read {path_str}: {e}"))?;
    if bytes.len() as u64 > MAX_FILE_SIZE {
        return Err(miette::miette!(
            "file too large ({} bytes, max {} bytes): {path_str}",
            bytes.len(),
            MAX_FILE_SIZE
        ));
    }
    let source = String::from_utf8(bytes)
        .map_err(|e| miette::miette!("invalid UTF-8 in {path_str}: {e}"))?;
    let base_dir = input.parent().unwrap_or(Path::new(".")).to_path_buf();
    Ok((source, base_dir))
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
    /// The compiled string ready to write (markdown or pretty JSON depending on format).
    pub(crate) content: String,
    /// Transitive dependency paths (empty when no `@import`s).
    pub(crate) dependencies: Vec<String>,
}

/// Compile `input` and return the content + deps WITHOUT writing any output.
///
/// This is the pure "compile" step used by the watch loop for content-based dedup.
/// `build` and the initial watch compile use [`compile_and_write`], which calls
/// this internally and then always writes.
///
/// # PF-004 compliance
/// All file reads go through [`read_build_input`] or `mds::compile_with_deps`
/// (which uses the resolver that enforces MAX_FILE_SIZE). There is no bare
/// `std::fs::read_to_string` path here.
pub(crate) fn compile_to_content(
    input: &Path,
    runtime_vars: Option<HashMap<String, mds::Value>>,
    format: &OutputFormat,
    quiet: bool,
) -> Result<CompileOutput> {
    match format {
        OutputFormat::Markdown => {
            let result =
                mds::compile_with_deps(input, runtime_vars).map_err(miette::Error::from)?;
            if !quiet {
                for w in &result.warnings {
                    eprintln!("{w}");
                }
            }
            Ok(CompileOutput {
                content: result.output,
                dependencies: result.dependencies,
            })
        }
        OutputFormat::Messages => {
            let (source, base_dir) = read_build_input(input)?;
            let result =
                mds::compile_messages_str_with_deps(&source, Some(&base_dir), runtime_vars)
                    .map_err(miette::Error::from)?;
            if !quiet {
                for w in &result.warnings {
                    eprintln!("{w}");
                }
            }
            let mut json = serde_json::to_string_pretty(&result.messages)
                .map_err(|e| miette::miette!("failed to serialize messages to JSON: {e}"))?;
            json.push('\n');
            Ok(CompileOutput {
                content: json,
                dependencies: result.dependencies,
            })
        }
    }
}

/// Compile `input` and write to `output_path`, returning the list of transitive deps.
///
/// - Markdown mode: `mds::compile_with_deps` → print warnings (unless quiet) →
///   `write_output` → return deps.
/// - Messages mode: `read_build_input` → `mds::compile_messages_str_with_deps` →
///   pretty JSON + trailing `\n` → `write_output` → return deps.
///
/// The returned `Vec<String>` contains canonical absolute dependency paths (empty
/// for stdin and for templates with no `@import`s).  `build` ignores the return
/// value; `watch` uses it to update the set of watched files (ADR-016: deps
/// recomputed on every rebuild, never trusted from a stale set).
///
/// # PF-004 compliance
/// All file reads go through `compile_to_content` → [`read_build_input`] or
/// `mds::compile_with_deps` (which uses the resolver that enforces MAX_FILE_SIZE).
/// There is no bare `std::fs::read_to_string` path here.
pub(crate) fn compile_and_write(
    input: &Path,
    output_path: Option<PathBuf>,
    runtime_vars: Option<HashMap<String, mds::Value>>,
    format: &OutputFormat,
    quiet: bool,
) -> Result<Vec<String>> {
    let out = compile_to_content(input, runtime_vars, format, quiet)?;
    write_output(output_path, &out.content, quiet, true)?;
    Ok(out.dependencies)
}

// ── Build args struct ─────────────────────────────────────────────────────────

pub(crate) struct BuildArgs {
    pub(crate) input: Option<PathBuf>,
    pub(crate) output: Option<String>,
    pub(crate) out_dir: Option<PathBuf>,
    pub(crate) vars: Option<PathBuf>,
    pub(crate) set_vars: Vec<(String, String)>,
    pub(crate) quiet: bool,
    pub(crate) format: OutputFormat,
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
        format,
    } = args;
    let runtime_vars = build_runtime_vars(vars, set_vars)?;

    // Resolve the input: explicit path, or auto-detect from cwd.
    // When auto-detected, print a "Building {path}" banner so users know which file was selected.
    let (input, auto_detected) = resolve_input(input)?;
    if auto_detected && !quiet {
        eprintln!("Building {}", input.display());
    }

    reject_directory_input(&input)?;

    match format {
        OutputFormat::Messages => {
            // --out-dir is silently dropped in messages mode (output always goes to stdout
            // or an explicit -o path).  Warn so the user knows their flag had no effect.
            if out_dir.is_some() && !quiet {
                eprintln!(
                    "warning: --out-dir is ignored in --format messages mode; \
                     use -o <file> to write to a file"
                );
            }
            run_build_messages(input, output, runtime_vars, quiet)
        }
        OutputFormat::Markdown => run_build_markdown(input, output, out_dir, runtime_vars, quiet),
    }
}

/// Compile `@message` blocks to a JSON array and write to stdout or `-o`.
///
/// Skips the output-dir / `mds.json` project-config logic — output always goes
/// to stdout or an explicit `-o` path.
fn run_build_messages(
    input: PathBuf,
    output: Option<String>,
    runtime_vars: Option<HashMap<String, mds::Value>>,
    quiet: bool,
) -> Result<()> {
    // Messages mode: output always goes to stdout (or -o); no output-dir / config logic.
    let output_path = match output.as_deref() {
        Some("-") | None => None,
        Some(o) => Some(PathBuf::from(o)),
    };
    // Route through compile_and_write to go through read_build_input (PF-004 compliance).
    let _ = compile_and_write(
        &input,
        output_path,
        runtime_vars,
        &OutputFormat::Messages,
        quiet,
    )?;
    Ok(())
}

/// Compile a template to Markdown and write to the resolved output destination.
///
/// Loads `mds.json` project config for output-dir resolution. Handles stdin
/// (`-`) and file paths, emitting warnings to stderr unless `quiet` is set.
fn run_build_markdown(
    input: PathBuf,
    output: Option<String>,
    out_dir: Option<PathBuf>,
    runtime_vars: Option<HashMap<String, mds::Value>>,
    quiet: bool,
) -> Result<()> {
    if input == Path::new("-") {
        // Stdin: can't use compile_with_deps (needs a path), so keep inline.
        let config = None; // no project config for stdin
        let output_path = resolve_output_path(&Some(input.clone()), &output, &out_dir, &config)?;
        let (source, cwd) = read_stdin()?;
        let (compiled, warnings) =
            mds::compile_str_collecting_warnings(&source, Some(&cwd), runtime_vars)
                .map_err(miette::Error::from)?;
        if !quiet {
            for w in &warnings {
                eprintln!("{w}");
            }
        }
        return write_output(output_path, &compiled, quiet, true);
    }

    // Load project config (mds.json), walking up from the input file.
    let config = load_config(&input)?;

    // Resolve output destination before compiling (config discovery happens once).
    let output_path = resolve_output_path(&Some(input.clone()), &output, &out_dir, &config)?;

    // Route through compile_and_write (PF-004 compliance: uses compile_with_deps which
    // enforces MAX_FILE_SIZE through the resolver).
    let _ = compile_and_write(
        &input,
        output_path,
        runtime_vars,
        &OutputFormat::Markdown,
        quiet,
    )?;
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
            derive_output_filename(Path::new("foo.mds")),
            OsString::from("foo.md")
        );
    }

    #[test]
    fn derive_output_filename_preserves_compound_extension() {
        assert_eq!(
            derive_output_filename(Path::new("foo.bar.mds")),
            OsString::from("foo.bar.md")
        );
    }

    #[test]
    fn derive_output_filename_no_extension() {
        assert_eq!(
            derive_output_filename(Path::new("README")),
            OsString::from("README.md")
        );
    }

    #[test]
    fn derive_output_filename_other_extension() {
        assert_eq!(
            derive_output_filename(Path::new("foo.txt")),
            OsString::from("foo.md")
        );
    }

    #[test]
    fn resolve_output_path_dash_o_dash_is_stdout() {
        let result = resolve_output_path(
            &Some(PathBuf::from("foo.mds")),
            &Some("-".to_string()),
            &None,
            &None,
        )
        .unwrap();
        assert_eq!(result, None, "-o - should resolve to stdout (None)");
    }

    #[test]
    fn resolve_output_path_stdin_no_o_is_stdout() {
        let result = resolve_output_path(&Some(PathBuf::from("-")), &None, &None, &None).unwrap();
        assert_eq!(
            result, None,
            "stdin input with no -o should resolve to stdout"
        );
    }

    #[test]
    fn resolve_output_path_default_file_next_to_source() {
        let result = resolve_output_path(
            &Some(PathBuf::from("/some/dir/hello.mds")),
            &None,
            &None,
            &None,
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
        let result = resolve_output_path(
            &Some(PathBuf::from("-")),
            &None,
            &Some(out_dir.clone()),
            &None,
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
        let result = resolve_output_path(
            &Some(PathBuf::from("/project/hello.mds")),
            &Some("out.md".to_string()),
            &None,
            &config,
        )
        .unwrap();
        assert_eq!(
            result,
            Some(PathBuf::from("out.md")),
            "-o should win over mds.json config"
        );
    }
}
