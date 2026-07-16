//! Build subcommand implementation and shared compilation helpers.
//!
//! All helpers in this module are `pub(crate)` so that `watch.rs` can reuse them
//! without duplicating logic or bypassing resource limits.
use std::collections::{HashMap, HashSet};
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
    /// Loaded (and validated — a malformed `fmt` section still fails config
    /// loading) for forward-compatibility, but not yet consulted by `mds fmt`
    /// — see `FmtConfig`.
    #[allow(
        dead_code,
        reason = "scaffolding for a rule not implemented until a future version"
    )]
    #[serde(default)]
    pub(crate) fmt: FmtConfig,
    /// Per-rule severity overrides for `mds lint` (AC-F-17).
    ///
    /// Unknown severity VALUES fail config loading loudly (closed enum).
    /// Unknown rule NAMES are preserved for forward compat (CLI warns and ignores).
    #[serde(default)]
    pub(crate) lint: LintCliConfig,
}

/// mds.json `lint` section: per-rule severity overrides.
///
/// Mirrors the core `LintConfig` shape but lives in the CLI so it can be
/// loaded alongside `BuildConfig` / `FmtConfig` as part of `MdsConfig`.
///
/// Unknown severity VALUES (e.g. `"banana"`) cause a hard parse error (exit 2)
/// because `Severity` is a closed enum with no sensible fallback. Unknown rule
/// NAMES are warn-and-ignored at the CLI layer (forward compat).
#[derive(Debug, Default, Deserialize)]
pub(crate) struct LintCliConfig {
    #[serde(default)]
    pub(crate) rules: HashMap<String, mds::Severity>,
}

impl LintCliConfig {
    /// Convert to the core `LintConfig` consumed by `mds::lint_*` functions.
    pub(crate) fn into_core_config(self) -> mds::LintConfig {
        mds::LintConfig { rules: self.rules }
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct BuildConfig {
    pub(crate) output_dir: Option<String>,
    /// Enable source-map generation for all builds (equivalent to `--source-map`).
    #[serde(default)]
    pub(crate) source_map: bool,
    /// Embed source text in the source map (equivalent to `--embed-sources`).
    #[serde(default)]
    pub(crate) embed_sources: bool,
}

/// Forward-compatibility scaffolding for `mds fmt` configuration.
///
/// `sort_frontmatter_keys` is not wired into any formatting behavior yet — the
/// v1 ruleset (R1-R4, see `mds-core`'s `formatter` module) deliberately defers
/// frontmatter key sorting to a future version, and there is intentionally no
/// matching CLI flag (a no-op flag would be a clippy/UX liability). The field
/// exists now purely so `{"fmt": {"sort_frontmatter_keys": false}}` parses
/// cleanly today and this won't need a breaking `mds.json` schema change once
/// the rule ships.
#[derive(Debug, Deserialize)]
pub(crate) struct FmtConfig {
    #[allow(
        dead_code,
        reason = "scaffolding for a rule not implemented until a future version"
    )]
    #[serde(default = "default_sort_frontmatter_keys")]
    pub(crate) sort_frontmatter_keys: bool,
}

impl Default for FmtConfig {
    fn default() -> Self {
        Self {
            sort_frontmatter_keys: default_sort_frontmatter_keys(),
        }
    }
}

fn default_sort_frontmatter_keys() -> bool {
    true
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
            // `CompiledOutput` is `#[non_exhaustive]`; update this match when new variants land.
            _ => unreachable!("unknown CompiledOutput variant"),
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

/// Bundled runtime-variable arguments from the CLI.
///
/// Groups `--vars`, `--set`, and `--set-string` together so they can be passed
/// as a single unit through CLI dispatch functions.
pub(crate) struct RuntimeVarArgs {
    /// Optional JSON vars file (`--vars`).
    pub(crate) vars: Option<PathBuf>,
    /// Auto-coerced `--set KEY=VALUE` overrides (bool/number/null/array/string).
    pub(crate) set_vars: Vec<(String, String)>,
    /// String-forced `--set-string KEY=VALUE` overrides (always string, no coercion).
    pub(crate) set_string_vars: Vec<(String, String)>,
}

/// Load vars from an optional file path, returning None if no file was given.
pub(crate) fn load_optional_vars_file(
    path: Option<PathBuf>,
) -> Result<Option<HashMap<String, mds::Value>>> {
    path.map(|p| mds::load_vars_file(&p).map_err(|e| miette::miette!("{e}")))
        .transpose()
}

/// Merge a `--vars` file with `--set` and `--set-string` overrides into a single map.
///
/// Processing order: file vars < `--set`/`--set-string` overrides.
/// Within each flag group, later flags win (last-wins). Cross-flag duplicate keys
/// (a key present in both `--set` and `--set-string`) are rejected with an error.
pub(crate) fn build_runtime_vars(
    args: RuntimeVarArgs,
) -> Result<Option<HashMap<String, mds::Value>>> {
    let RuntimeVarArgs {
        vars,
        set_vars,
        set_string_vars,
    } = args;

    // Collect unique keys used by --set for cross-flag duplicate detection.
    let set_keys: HashSet<&str> = set_vars.iter().map(|(k, _)| k.as_str()).collect();

    // Reject any key that appears in both --set and --set-string.
    for (key, _) in &set_string_vars {
        if set_keys.contains(key.as_str()) {
            return Err(miette::miette!(
                "variable '{key}' is set by both --set and --set-string; use only one"
            ));
        }
    }

    let mut runtime_vars = load_optional_vars_file(vars)?;
    for (key, val) in set_vars {
        runtime_vars
            .get_or_insert_with(HashMap::new)
            .insert(key, parse_cli_value(val));
    }
    for (key, val) in set_string_vars {
        runtime_vars
            .get_or_insert_with(HashMap::new)
            .insert(key, mds::Value::String(val));
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
        CompiledOutput::Messages(msgs) => {
            let mut json = serde_json::to_string_pretty(&msgs)
                .map_err(|e| miette::miette!("failed to serialize messages to JSON: {e}"))?;
            json.push('\n');
            Ok(json)
        }
        // `CompiledOutput` is `#[non_exhaustive]`; update this match when new variants land.
        _ => unreachable!("unknown CompiledOutput variant"),
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
        // Stdin: compile from source string using cwd as base_dir.
        // read_stdin enforces MAX_FILE_SIZE (PF-004).
        let (source, cwd) = read_stdin()?;
        mds::compile_str_with_deps_opts(&source, Some(&cwd), runtime_vars, opts)
            .map_err(miette::Error::from)?
    } else {
        // File path: compile_with_deps_opts routes through the resolver which enforces
        // MAX_FILE_SIZE and check_symlink (PF-004 compliance).
        mds::compile_with_deps_opts(input, runtime_vars, opts).map_err(miette::Error::from)?
    };

    if !quiet {
        for w in &result.warnings {
            eprintln!("{w}");
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

/// Compile `input`, derive the output path from the compiled kind, and write.
///
/// Returns `(output_path, deps, content)`:
/// - `output_path`: the resolved output path (None for stdout).
/// - `deps`: transitive dependency paths.
/// - `content`: the compiled string (issue 3 — reused by the watch baseline block
///   so startup does not compile twice).
///
/// The output path is derived AFTER compiling (compile-then-route) so the kind
/// (and thus extension: `.json` for messages, `.md` for markdown) is known before
/// the path is constructed. This is the single-file intrinsic extension path.
///
/// If `-o <path>` is given explicitly, that path is used verbatim and an ext-mismatch
/// warning is emitted when the extension contradicts the kind (AC-FUNC-11).
/// If `-o -` or stdin-with-no-flags, content is written to stdout.
///
/// Source-map writing is NOT performed here — callers that need maps handle them
/// after this call returns (so map writing logic stays in `run_build`, not here).
/// Watch callers pass `opts = mds::CompileOptions::default()` to opt out of maps.
///
/// # PF-004 compliance
/// All file reads go through `compile_to_content` → `mds::compile_with_deps_opts` or
/// `mds::compile_str_with_deps_opts` (which use the resolver that enforces MAX_FILE_SIZE).
/// There is no bare `std::fs::read_to_string` path here.
pub(crate) fn compile_and_write(
    input: &Path,
    output: &Option<String>,
    out_dir: &Option<PathBuf>,
    config: &Option<(MdsConfig, PathBuf)>,
    runtime_vars: Option<HashMap<String, mds::Value>>,
    quiet: bool,
    opts: mds::CompileOptions,
) -> Result<(Option<PathBuf>, Vec<String>, String)> {
    let compiled = compile_to_content(input, runtime_vars, quiet, opts)?;
    let output_path = resolve_output_path_for_kind(
        &Some(input.to_path_buf()),
        output,
        out_dir,
        config,
        compiled.kind,
        quiet,
    )?;
    write_output(output_path.clone(), &compiled.content, quiet, true)?;
    Ok((output_path, compiled.dependencies, compiled.content))
}

// ── Build args struct ─────────────────────────────────────────────────────────

pub(crate) struct BuildArgs {
    pub(crate) input: Option<PathBuf>,
    pub(crate) output: Option<String>,
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

/// Resolve the input path: use the explicit value, or auto-detect from cwd.
///
/// Returns `(path, auto_detected)`.
pub(crate) fn resolve_input(input: Option<PathBuf>) -> Result<(PathBuf, bool)> {
    match input {
        Some(p) => Ok((p, false)),
        None => auto_detect_mds_file().map(|p| (p, true)),
    }
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

/// Compute a relative path from `base_dir` to `target` using forward slashes.
///
/// Used to relativize `sources[]` entries in the source map so they are
/// map-relative rather than absolute (AC-FUNC-05 / AC-SEC-01).
///
/// Falls back to the absolute path (forward-slash converted) when a relative
/// path cannot be computed (e.g. different drive roots on Windows).
pub(crate) fn relative_path(base_dir: &Path, target: &Path) -> String {
    // Use `pathdiff` logic inline so we don't add a dependency.
    // Build the relative path by walking up from base_dir to the common ancestor,
    // then down to target.
    let base = base_dir;
    let mut base_comps: Vec<_> = base.components().collect();
    let mut target_comps: Vec<_> = target.components().collect();

    // Strip common prefix.
    let common = base_comps
        .iter()
        .zip(target_comps.iter())
        .take_while(|(b, t)| b == t)
        .count();
    base_comps.drain(..common);
    target_comps.drain(..common);

    if base_comps.is_empty() && target_comps.is_empty() {
        return ".".to_string();
    }

    let mut parts: Vec<String> = Vec::new();
    for _ in &base_comps {
        parts.push("..".to_string());
    }
    for c in &target_comps {
        parts.push(c.as_os_str().to_string_lossy().into_owned());
    }

    let rel = parts.join("/");
    // Guard: if rel somehow became empty use ".".
    if rel.is_empty() {
        ".".to_string()
    } else {
        rel
    }
}

/// Relativize and sanitize a single source path for use in `sources[]`.
///
/// Rules (AC-FUNC-05 / AC-SEC-01 / PF-003):
/// 1. `<source>` sentinel → relabeled to `<stdin>` (AC-FUNC-12) when
///    the caller explicitly passes `stdin_label = true`.
/// 2. Windows `\\?\` verbatim-prefix paths → stripped before further processing.
/// 3. Absolute paths → relativized against `map_dir`.
/// 4. Relative paths → left as-is (already relative).
/// 5. All backslashes → forward slashes (AC-FUNC-05).
/// 6. Result MUST NOT be an absolute path (enforced by assertion).
pub(crate) fn relativize_source_path(source: &str, map_dir: &Path, stdin_label: bool) -> String {
    // Rule 1: stdin sentinel relabeling.
    if source == "<source>" && stdin_label {
        return "<stdin>".to_string();
    }
    // Pass through non-path sentinels unchanged (e.g. "<source>" in non-stdin builds).
    if source.starts_with('<') && source.ends_with('>') {
        return source.to_string();
    }

    // Rule 2: strip Windows \\?\ verbatim prefix (PF-003 / AC-SEC-01).
    let stripped = source.strip_prefix(r"\\?\").unwrap_or(source);

    // Normalize to a Path.
    let p = Path::new(stripped);

    let result = if p.is_absolute() {
        // Rule 3: relativize the absolute source against the map directory.
        // `map_dir` derives from the caller's `-o` value and may itself be
        // relative (or empty when `-o` is a bare filename). Absolutize it
        // against the CWD first so both paths share one coordinate space —
        // otherwise the component diff embeds the source's absolute path
        // verbatim (`..//abs/...`) or produces a leading-`/` result, leaking
        // the filesystem path into sources[] (AC-SEC-01).
        let abs_map_dir = if map_dir.is_absolute() {
            map_dir.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(map_dir)
        };
        relative_path(&abs_map_dir, p)
    } else {
        // Rule 4: already relative — convert separators only.
        stripped.replace('\\', "/")
    };

    // AC-SEC-01: never leak an absolute path into sources[]. The relativization
    // above yields a relative path for same-root inputs; as defense-in-depth for
    // exotic cross-root cases (e.g. different Windows drives), degrade a
    // still-absolute result to the bare file name rather than panicking or
    // leaking a filesystem path. Runtime guard (not debug_assert) so the
    // guarantee holds in release builds too.
    //
    // SEC-2 (guards PF-005 theme / AC-SEC-01): broaden the Windows drive-path
    // guard beyond the backslash-only check.  `relative_path` and Rule 4 both
    // normalise separators to `/`, so a forward-slashed drive path such as
    // `C:/secret/foo.mds` or a cross-drive relative result like `../../D:/x.mds`
    // contains `:/` rather than `:\\` and slipped through the old guard.  The
    // three conditions below together catch all three forms:
    //   • `:\` — classic backslash-qualified Windows drive path
    //   • `:/` — forward-slash-normalised Windows drive path (SEC-2 gap)
    //   • leading `<letter>:` — bare drive designator without a separator
    let is_drive_qualified = |s: &str| -> bool {
        s.contains(":\\")
            || s.contains(":/")
            || (s.len() >= 2
                && (s.as_bytes()[0] as char).is_ascii_alphabetic()
                && s.as_bytes()[1] == b':')
    };
    if result.starts_with('/') || is_drive_qualified(&result) {
        return Path::new(stripped)
            .file_name()
            .map(|n| n.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|| "source".to_string());
    }

    result
}

/// Relativize `sources[]` and set `file` in a [`mds::SourceMap`] in place.
///
/// Must be called before writing the map to disk or embedding it inline.
/// - `output_path`: the path where the compiled output is written (used to
///   compute the map directory and the `file` basename).
/// - When `output_path` is `None` (stdout), sources are left as-is and
///   `file` remains as set by the core (no map-relative anchor exists).
pub(crate) fn relativize_source_map_fields(
    sm: &mut mds::SourceMap,
    output_path: Option<&Path>,
    stdin_label: bool,
) {
    let Some(out) = output_path else {
        return;
    };
    let map_dir = out.parent().unwrap_or(Path::new("."));

    // Set `file` to the output basename.
    sm.file = out.file_name().map(|n| n.to_string_lossy().into_owned());

    // Relativize each source.
    for src in &mut sm.sources {
        *src = relativize_source_path(src, map_dir, stdin_label);
    }
}

/// Verify a `.map` file is a valid source-map v3 for `expected_basename`,
/// then delete it (AC-FUNC-10: stale-map reconciliation).
///
/// Silently skips deletion if:
/// - The file does not exist.
/// - The file is not valid UTF-8 JSON.
/// - `version != 3`.
/// - `file` does not match `expected_basename`.
///
/// This avoids clobbering a hand-authored file that happens to share a name with
/// a tool-generated map.
pub(crate) fn verify_then_delete_map(map_path: &Path, expected_basename: &str, quiet: bool) {
    let Ok(bytes) = std::fs::read(map_path) else {
        return;
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };
    if v.get("version").and_then(|x| x.as_u64()) != Some(3) {
        if !quiet {
            eprintln!(
                "warning: leaving {} in place — not a tool-generated SMv3 map (version/file mismatch)",
                map_path.display()
            );
        }
        return;
    }
    if v.get("file").and_then(|x| x.as_str()) != Some(expected_basename) {
        if !quiet {
            eprintln!(
                "warning: leaving {} in place — not a tool-generated SMv3 map (version/file mismatch)",
                map_path.display()
            );
        }
        return;
    }
    if let Err(e) = std::fs::remove_file(map_path) {
        if !quiet {
            eprintln!(
                "warning: could not remove stale map {}: {e}",
                map_path.display()
            );
        }
    } else if !quiet {
        eprintln!("Removed stale map {}", map_path.display());
    }
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
    let runtime_vars = build_runtime_vars(RuntimeVarArgs {
        vars,
        set_vars,
        set_string_vars,
    })?;

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

        // Load project config to determine effective flags for directory mode.
        let dir_config = load_config(&input)?;
        let cfg_source_map = dir_config
            .as_ref()
            .map(|(c, _)| c.build.source_map)
            .unwrap_or(false);
        let cfg_embed_sources = dir_config
            .as_ref()
            .map(|(c, _)| c.build.embed_sources)
            .unwrap_or(false);
        let use_source_map = (flag_source_map || cfg_source_map) && !no_source_map;
        let use_embed_sources = flag_embed_sources || cfg_embed_sources;

        return run_build_directory(
            &input,
            out_dir,
            runtime_vars,
            quiet,
            use_source_map,
            use_embed_sources,
            inline,
        );
    }

    // ── Stdin path ───────────────────────────────────────────────────────────────

    if input == Path::new("-") {
        // Stdin: no project config; effective flags from CLI only.
        let use_source_map = flag_source_map && !no_source_map;
        let use_embed_sources = flag_embed_sources;

        // AC-SEC-02: warn when shipping full source text in a distributable artifact.
        if use_embed_sources && inline && !quiet {
            eprintln!(
                "warning: --embed-sources with --inline ships full source text in the output \
                 (AC-SEC-02)"
            );
        }

        // Inline + stdout is not supported (there is no file to embed the carrier into).
        if use_source_map && inline && output.as_deref() == Some("-") {
            return Err(miette::miette!(
                "--inline cannot be used with -o - (stdout has no file to embed the carrier into)"
            ));
        }

        let opts = mds::CompileOptions {
            source_map: use_source_map,
            include_sources_content: use_embed_sources,
        };

        let (source, cwd) = read_stdin()?;
        let result = mds::compile_str_with_deps_opts(&source, Some(&cwd), runtime_vars, opts)
            .map_err(miette::Error::from)?;
        if !quiet {
            for w in &result.warnings {
                eprintln!("{w}");
            }
        }

        let mut source_map = result.source_map;
        let kind = OutputKind::from(&result.output);
        let content = serialize_output(result.output)?;

        // Stdin: no project config; output path follows -o flag or defaults to stdout.
        let output_path =
            resolve_output_path_for_kind(&Some(input), &output, &out_dir, &None, kind, quiet)?;

        if let Some(ref mut sm) = source_map {
            // Relabel <source> → <stdin> for stdin builds (AC-FUNC-12).
            relativize_source_map_fields(sm, output_path.as_deref(), true);
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
                return write_output(output_path, &final_content, quiet, true);
            } else {
                // Sidecar: write output first (byte-identical to no-flag; ADR-002).
                match &output_path {
                    None => {
                        // Sidecar + stdout: cannot write a sidecar without a file path.
                        return Err(miette::miette!(
                            "--source-map (sidecar) requires an output file; \
                             use -o <file> or --out-dir, or use --inline for stdout"
                        ));
                    }
                    Some(out) => {
                        write_output(Some(out.clone()), &content, quiet, true)?;
                        if let Some(ref sm) = source_map {
                            let map_path = map_path_for(out);
                            let map_json = sm.to_json();
                            std::fs::write(&map_path, &map_json).map_err(|e| {
                                miette::miette!("cannot write {}: {e}", map_path.display())
                            })?;
                            if !quiet {
                                eprintln!("Source map written to {}", map_path.display());
                            }
                        }
                        return Ok(());
                    }
                }
            }
        }

        return write_output(output_path, &content, quiet, true);
    }

    // ── File input path ──────────────────────────────────────────────────────────

    // File input: load project config and compute effective flags.
    let config = load_config(&input)?;
    let cfg_source_map = config
        .as_ref()
        .map(|(c, _)| c.build.source_map)
        .unwrap_or(false);
    let cfg_embed_sources = config
        .as_ref()
        .map(|(c, _)| c.build.embed_sources)
        .unwrap_or(false);

    let use_source_map = (flag_source_map || cfg_source_map) && !no_source_map;
    let use_embed_sources = flag_embed_sources || cfg_embed_sources;

    // AC-SEC-02: warn when shipping full source text in a distributable artifact.
    if use_embed_sources && inline && !quiet {
        eprintln!(
            "warning: --embed-sources with --inline ships full source text in the output \
             (AC-SEC-02)"
        );
    }

    let opts = mds::CompileOptions {
        source_map: use_source_map,
        include_sources_content: use_embed_sources,
    };

    let compiled = compile_to_content(&input, runtime_vars, quiet, opts)?;
    let output_path = resolve_output_path_for_kind(
        &Some(input.clone()),
        &output,
        &out_dir,
        &config,
        compiled.kind,
        quiet,
    )?;

    let mut source_map = compiled.source_map;
    if let Some(ref mut sm) = source_map {
        relativize_source_map_fields(sm, output_path.as_deref(), false);
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
            write_output(output_path.clone(), &final_content, quiet, true)?;

            // Stale sidecar reconciliation: if there is an existing .map file from
            // a prior sidecar build, remove it (AC-FUNC-10).
            if let Some(ref out) = output_path {
                let map_path = map_path_for(out);
                let basename = out
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                verify_then_delete_map(&map_path, &basename, quiet);
            }
        } else {
            // Sidecar: write output byte-identical to no-flag build (ADR-002).
            match &output_path {
                None => {
                    // Sidecar + stdout: not supported.
                    return Err(miette::miette!(
                        "--source-map (sidecar) requires an output file; \
                         use -o <file> or --out-dir, or use --inline for stdout"
                    ));
                }
                Some(out) => {
                    write_output(Some(out.clone()), &compiled.content, quiet, true)?;
                    if let Some(ref sm) = source_map {
                        let map_path = map_path_for(out);
                        let map_json = sm.to_json();
                        std::fs::write(&map_path, &map_json).map_err(|e| {
                            miette::miette!("cannot write {}: {e}", map_path.display())
                        })?;
                        if !quiet {
                            eprintln!("Source map written to {}", map_path.display());
                        }
                    }
                }
            }
        }
    } else {
        write_output(output_path.clone(), &compiled.content, quiet, true)?;

        // No-source-map build: if a stale sidecar exists from a prior source-map build,
        // remove it (AC-FUNC-10).
        if let Some(ref out) = output_path {
            let map_path = map_path_for(out);
            if map_path.exists() {
                let basename = out
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                verify_then_delete_map(&map_path, &basename, quiet);
            }
        }
    }

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
    runtime_vars: Option<HashMap<String, mds::Value>>,
    quiet: bool,
    source_map: bool,
    embed_sources: bool,
    inline: bool,
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

    // Canonicalize dir for the starts_with comparison (issue 4): abs_out_dir (d) is already
    // canonical, but `dir` may be a raw relative or pre-resolved-but-not-canonical path.
    // Mismatch (e.g. /private/tmp vs /tmp on macOS) causes the exclusion to silently fail
    // and includes the out-dir in the collection — not a security issue (only .mds are
    // gathered) but causes redundant scanning. Fall back to raw dir when canonicalize fails
    // (dir may not exist yet in unusual configurations).
    let canonical_dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());

    // Exclude the out-dir from collection when it is nested inside the source root.
    let exclude_prefix: Option<PathBuf> = match &output_base {
        OutputBase::Dir(d) if d.starts_with(&canonical_dir) => Some(d.clone()),
        _ => None,
    };

    let files = collect_mds_files(dir, MAX_DEPTH, exclude_prefix.as_deref());

    if files.is_empty() {
        if !quiet {
            eprintln!("No .mds files found in {}", dir.display());
        }
        return Ok(());
    }

    let opts = mds::CompileOptions {
        source_map,
        include_sources_content: embed_sources,
    };

    let mut ok_count: usize = 0;
    let mut fail_count: usize = 0;
    // Track paths successfully written in this build run so the stale-cleanup
    // step can verify it was tool-produced before deleting (issue 1 guard):
    // prevents clobbering a hand-authored file that shares a stem with a .mds
    // (e.g. notes.md kept next to notes.mds that now compiles to notes.json).
    // Same-stem .md/.json files adjacent to a .mds in NextToSource mode are
    // considered tool-owned; if a collision is a concern use --out-dir to
    // separate source and output trees.
    let mut written_this_run: HashSet<PathBuf> = HashSet::new();

    for file in &files {
        // Skip partials: they contribute to imports but produce no standalone output.
        if is_partial(file) {
            continue;
        }

        // Compile (all reads go through mds-core which enforces MAX_FILE_SIZE — PF-004).
        match compile_to_content(file, runtime_vars.clone(), quiet, opts.clone()) {
            Ok(mut compiled) => {
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

                // Relativize source map fields for this output path.
                if let Some(ref mut sm) = compiled.source_map {
                    relativize_source_map_fields(sm, Some(&out_path), false);
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

                match std::fs::write(&out_path, &final_content) {
                    Ok(()) => {
                        if !quiet {
                            eprintln!("Compiled to {}", out_path.display());
                        }
                        written_this_run.insert(out_path.clone());

                        // Write sidecar map (non-inline mode).
                        if source_map && !inline {
                            if let Some(ref sm) = compiled.source_map {
                                let map_path = map_path_for(&out_path);
                                let map_json = sm.to_json();
                                if let Err(e) = std::fs::write(&map_path, &map_json) {
                                    eprintln!("error: cannot write {}: {e}", map_path.display());
                                    fail_count += 1;
                                    continue;
                                }
                                if !quiet {
                                    eprintln!("Source map written to {}", map_path.display());
                                }
                            }
                        }

                        // Stale-output cleanup: remove the wrong-extension sibling only
                        // if this tool wrote it (i.e. it appears in written_this_run from
                        // a previous iteration, or matches a prior build's output).
                        // For a kind-flip (template switched markdown↔messages between
                        // two runs), the stale sibling won't be in written_this_run yet.
                        // We still want to clean it up in that case — the legitimate
                        // format-flip cleanup. Gate: the stale path must either be in
                        // written_this_run (already written this run — shouldn't happen
                        // but safe), OR the output_base is Dir (separate output tree,
                        // no hand-authored sibling risk). In NextToSource mode and the
                        // stale path was not written by us this run, skip to protect
                        // hand-authored files.
                        let base_no_ext = output_base_no_ext(file, dir, &output_base);
                        let stale_path =
                            base_no_ext.with_extension(compiled.kind.stale_extension());
                        let safe_to_delete = matches!(output_base, OutputBase::Dir(_))
                            || written_this_run.contains(&stale_path);
                        if safe_to_delete {
                            probe_and_remove_stale(&base_no_ext, compiled.kind);
                        }
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
                    ..Default::default()
                },
                ..Default::default()
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

    // ── T5: malformed fmt config fails config loading ─────────────────────────

    #[test]
    fn fmt_config_malformed_bool_field_fails_loading() {
        // `FmtConfig.sort_frontmatter_keys` is a `bool`. Supplying a string
        // value must cause `serde_json` to reject the config, so `load_config`
        // returns `Err` rather than silently using the default. This ensures a
        // bad `mds.json` is reported loudly rather than quietly ignored.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("mds.json"),
            r#"{"fmt": {"sort_frontmatter_keys": "not-a-bool"}}"#,
        )
        .unwrap();

        let result = load_config(dir.path());
        assert!(
            result.is_err(),
            "a malformed fmt config (wrong type for sort_frontmatter_keys) must fail config loading"
        );
    }

    #[test]
    fn fmt_config_valid_section_loads_cleanly() {
        // Complement to the malformed test: a well-typed fmt section must parse
        // without error and produce the expected field value.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("mds.json"),
            r#"{"fmt": {"sort_frontmatter_keys": false}}"#,
        )
        .unwrap();

        let result = load_config(dir.path()).expect("valid fmt config must load");
        let (config, _) = result.expect("mds.json must be found");
        assert!(
            !config.fmt.sort_frontmatter_keys,
            "sort_frontmatter_keys: false must deserialize correctly"
        );
    }

    // ── build_runtime_vars: cross-flag duplicate rejection (#152) ─────────────

    #[test]
    fn build_runtime_vars_cross_flag_duplicate_is_error() {
        // A key present in both --set and --set-string must be a hard error.
        let result = build_runtime_vars(RuntimeVarArgs {
            vars: None,
            set_vars: vec![("x".to_string(), "1".to_string())],
            set_string_vars: vec![("x".to_string(), "2".to_string())],
        });
        assert!(result.is_err(), "cross-flag duplicate key must be rejected");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("variable 'x' is set by both --set and --set-string"),
            "error message must identify the key; got: {msg}"
        );
        assert!(
            msg.contains("use only one"),
            "error message must say 'use only one'; got: {msg}"
        );
    }

    #[test]
    fn build_runtime_vars_check_command_cross_flag_duplicate_is_error() {
        // The same build_runtime_vars function is used by both mds build and mds check.
        // Verify the error fires regardless of which args struct wraps it.
        let result = build_runtime_vars(RuntimeVarArgs {
            vars: None,
            set_vars: vec![("count".to_string(), "3".to_string())],
            set_string_vars: vec![("count".to_string(), "three".to_string())],
        });
        assert!(
            result.is_err(),
            "cross-flag duplicate must be rejected in check parity"
        );
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("variable 'count'"),
            "error must name the duplicate key; got: {msg}"
        );
    }

    #[test]
    fn build_runtime_vars_same_flag_last_wins() {
        // Within a single flag group, the last occurrence wins (no error).
        let result = build_runtime_vars(RuntimeVarArgs {
            vars: None,
            set_vars: vec![
                ("x".to_string(), "1".to_string()),
                ("x".to_string(), "2".to_string()),
            ],
            set_string_vars: vec![],
        })
        .expect("same-flag last-wins must not error");
        let map = result.expect("non-empty vars");
        assert_eq!(
            map.get("x"),
            Some(&mds::Value::Number(2.0)),
            "last --set wins within the same flag"
        );
    }

    #[test]
    fn build_runtime_vars_same_flag_string_last_wins() {
        // Within --set-string, the last occurrence also wins (no error).
        let result = build_runtime_vars(RuntimeVarArgs {
            vars: None,
            set_vars: vec![],
            set_string_vars: vec![
                ("id".to_string(), "007".to_string()),
                ("id".to_string(), "009".to_string()),
            ],
        })
        .expect("same-flag last-wins in --set-string must not error");
        let map = result.expect("non-empty vars");
        assert_eq!(
            map.get("id"),
            Some(&mds::Value::String("009".to_string())),
            "last --set-string wins within the same flag"
        );
    }

    // ── AC-SEC-01: source-path relativization must never leak an absolute path ──

    #[test]
    fn relativize_absolute_source_with_absolute_mapdir_is_clean_relative() {
        // Both absolute, sharing a common ancestor → clean map-relative path.
        let got = relativize_source_path("/proj/src/a.mds", Path::new("/proj/build"), false);
        assert_eq!(
            got, "../src/a.mds",
            "same-root absolute paths relativize cleanly"
        );
    }

    #[test]
    fn relativize_absolute_source_with_relative_mapdir_never_leaks_absolute() {
        // Regression: a relative `-o` (relative map_dir) diffed against an
        // absolute source previously produced `..//abs/...` (or a leading-'/'
        // result that panicked the debug_assert), leaking the absolute path.
        // After absolutizing map_dir against the CWD the result must be a clean
        // relative path — never absolute, never an embedded absolute prefix.
        let got =
            relativize_source_path("/tmp/deep/nested/abs_input.mds", Path::new("build"), false);
        assert!(!got.starts_with('/'), "must not be absolute: {got}");
        assert!(
            !got.contains("//"),
            "must not embed a raw absolute path: {got}"
        );
        assert!(
            !got.contains(":\\"),
            "must not embed a Windows drive path: {got}"
        );
        assert!(
            got.ends_with("abs_input.mds"),
            "must still resolve to the source file: {got}"
        );
    }

    #[test]
    fn relativize_absolute_source_with_empty_mapdir_never_leaks_absolute() {
        // `-o out.md` (bare filename) yields an EMPTY map_dir. Previously this
        // produced a leading-'/' result (`//abs/...`) that panicked in debug and
        // leaked an absolute path in release. Must now be a clean relative path.
        let got = relativize_source_path("/tmp/deep/abs_input.mds", Path::new(""), false);
        assert!(!got.starts_with('/'), "must not be absolute: {got}");
        assert!(
            !got.contains("//"),
            "must not embed a raw absolute path: {got}"
        );
        assert!(
            got.ends_with("abs_input.mds"),
            "must still resolve to the source file: {got}"
        );
    }

    // SEC-2: forward-slashed Windows drive paths must also degrade to filename.
    // (The old guard only checked ":\\" — "C:/" slipped through.)

    #[test]
    fn relativize_forward_slashed_drive_path_degrades_to_filename() {
        // `C:/secret/foo.mds` on Unix is not seen as absolute by Path, so it
        // went through Rule 4 unchanged and slipped past the old `":\\"` guard.
        let got = relativize_source_path("C:/secret/foo.mds", Path::new("build"), false);
        assert_eq!(
            got, "foo.mds",
            "forward-slashed drive path must degrade to filename: {got}"
        );
    }

    #[test]
    fn relativize_cross_drive_relative_path_degrades_to_filename() {
        // A cross-drive path suffix (`../../D:/x.mds`) that passes through
        // Rule 4 contains `:/ ` not `:\\` and previously slipped through.
        let got = relativize_source_path("../../D:/x.mds", Path::new("build"), false);
        assert_eq!(
            got, "x.mds",
            "cross-drive :/ path must degrade to filename: {got}"
        );
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
    fn build_runtime_vars_distinct_keys_no_error() {
        // Using --set and --set-string for DIFFERENT keys must succeed.
        let result = build_runtime_vars(RuntimeVarArgs {
            vars: None,
            set_vars: vec![("num".to_string(), "42".to_string())],
            set_string_vars: vec![("id".to_string(), "007".to_string())],
        })
        .expect("distinct keys across flags must not error");
        let map = result.expect("non-empty vars");
        assert_eq!(map.get("num"), Some(&mds::Value::Number(42.0)));
        assert_eq!(map.get("id"), Some(&mds::Value::String("007".to_string())));
    }
}
