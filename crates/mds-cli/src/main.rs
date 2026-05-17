use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process;

use clap::{Parser, Subcommand};
use miette::Result;
use mds::MdsError;
use serde::Deserialize;

// ── Project config (mds.json) ─────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
struct MdsConfig {
    #[serde(default)]
    build: BuildConfig,
}

#[derive(Debug, Default, Deserialize)]
struct BuildConfig {
    output_dir: Option<String>,
}

/// Maximum allowed size for `mds.json` (1 MB) to prevent runaway memory use.
const MAX_CONFIG_SIZE: u64 = 1024 * 1024;

/// Maximum directory traversal depth when searching for `mds.json`.
///
/// Imported from the library crate — single source of truth with `find_project_root`
/// in `resolver.rs`.
use mds::MAX_TRAVERSAL_DEPTH;

/// Walk up from `start` looking for `mds.json`.
///
/// Returns `Ok(Some((config, config_dir)))` when found, `Ok(None)` when no
/// `mds.json` exists in the hierarchy, or `Err(...)` when a file is found but
/// contains invalid JSON.
///
/// The `config_dir` is the directory that *contains* `mds.json` — used to
/// resolve relative `output_dir` values.
fn load_config(
    start: &Path,
) -> std::result::Result<Option<(MdsConfig, PathBuf)>, miette::Error> {
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
            let bytes = std::fs::read(&candidate).map_err(|e| {
                miette::miette!("cannot read {}: {e}", candidate.display())
            })?;
            if bytes.len() as u64 > MAX_CONFIG_SIZE {
                return Err(miette::miette!(
                    "mds.json at {} is too large ({} bytes; maximum is 1 MB)",
                    candidate.display(),
                    bytes.len()
                ));
            }
            let raw = String::from_utf8(bytes).map_err(|e| {
                miette::miette!("invalid UTF-8 in {}: {e}", candidate.display())
            })?;
            let config: MdsConfig = serde_json::from_str(&raw).map_err(|e| {
                miette::miette!(
                    "invalid mds.json at {}: {e}",
                    candidate.display()
                )
            })?;
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
fn derive_output_filename(input: &Path) -> OsString {
    let stem = input.file_stem().unwrap_or(input.as_os_str());
    let mut name = OsString::from(stem);
    name.push(".md");
    name
}

/// Create `dir` (if absent) and return `dir/<derived-name>.md`.
///
/// `input_path` drives the filename: if `Some`, the stem is reused (e.g. `foo.mds` → `foo.md`);
/// if `None` (stdin), the fallback name `output.md` is used.
fn prepare_output_dir(
    dir: &Path,
    input_path: Option<&Path>,
) -> std::result::Result<PathBuf, miette::Error> {
    let filename = input_path
        .map(derive_output_filename)
        .unwrap_or_else(|| OsString::from("output.md"));
    std::fs::create_dir_all(dir).map_err(|e| {
        miette::miette!("cannot create output directory {}: {e}", dir.display())
    })?;
    Ok(dir.join(filename))
}

/// Resolve the output path according to the precedence chain:
///
/// 1. `-o -`                         → stdout (returns `None`)
/// 2. `-o <path>`                    → that exact path
/// 3. Stdin with no -o / --out-dir   → stdout (returns `None`)
/// 4. `--out-dir <dir>`              → `<dir>/<name>.md` (directory created if needed)
/// 5. `mds.json`                     → `<config_dir>/<output_dir>/<name>.md` (dir created)
/// 6. Default                        → source dir + `<name>.md`
fn resolve_output_path(
    input: &Option<PathBuf>,
    output: &Option<String>,
    out_dir: &Option<PathBuf>,
    config: &Option<(MdsConfig, PathBuf)>,
) -> std::result::Result<Option<PathBuf>, miette::Error> {
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

// ── CLI entry point ───────────────────────────────────────────────────────────

/// Scan the current directory for `.mds` files.
///
/// Returns `Ok(path)` if exactly one `.mds` file is found, or an `Err` describing
/// why auto-detection failed (zero files, multiple files, or I/O error).
fn auto_detect_mds_file() -> std::result::Result<PathBuf, miette::Error> {
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

#[derive(Parser)]
#[command(
    name = "mds",
    about = "MDS (Markdown Script) compiler",
    long_about = "MDS (Markdown Script) compiler — composable LLM prompt templates\n\nCompile .mds template files into Markdown. Use variables, loops,\nconditionals, functions, and imports to build reusable prompts.\n\nQuick start:\n  mds init                       Create a starter template\n  mds build hello.mds            Compile to hello.md\n  mds build hello.mds -o -       Compile to stdout\n  mds build hello.mds -o out.md  Compile to a specific file",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    /// Suppress status messages
    #[arg(long, short = 'q', global = true)]
    quiet: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Compile an MDS file to Markdown
    #[command(
        after_help = "Examples:\n  mds build                                  Auto-detect the .mds file in current dir\n  mds build template.mds                     Compile to template.md (next to source)\n  mds build template.mds -o -               Compile to stdout\n  mds build template.mds -o output.md       Compile to specific file\n  mds build template.mds --out-dir dist     Compile to dist/template.md\n  mds build template.mds --vars vars.json   With variable overrides\n  mds build template.mds --set name=Alice   Set a single variable\n  echo \"Hello {name}!\" | mds build -         Compile from stdin (writes to stdout)"
    )]
    Build {
        /// Input .mds file (use "-" for stdin; omit to auto-detect in current directory)
        input: Option<PathBuf>,
        /// Output destination: a file path, or "-" for stdout.
        /// Defaults to <name>.md next to the source file.
        /// Mutually exclusive with --out-dir.
        #[arg(short = 'o', long = "output", conflicts_with = "out_dir")]
        output: Option<String>,
        /// Output directory. The output file is named <input-stem>.md inside this directory.
        /// Directory is created if it does not exist.
        /// Mutually exclusive with -o/--output.
        #[arg(long = "out-dir", conflicts_with = "output")]
        out_dir: Option<PathBuf>,
        /// JSON file with runtime variable overrides
        #[arg(long)]
        vars: Option<PathBuf>,
        /// Set a runtime variable (repeatable, e.g. --set name=Alice --set count=3)
        #[arg(long = "set", value_name = "KEY=VALUE", value_parser = parse_key_value)]
        set_vars: Vec<(String, String)>,
    },
    /// Validate an MDS file without rendering
    #[command(
        after_help = "Examples:\n  mds check                                  Auto-detect the .mds file in current dir\n  mds check template.mds                     Validate a specific file\n  mds check template.mds --set name=Alice    Validate with variable overrides"
    )]
    Check {
        /// Input .mds file (use "-" for stdin; omit to auto-detect in current directory)
        input: Option<PathBuf>,
        /// JSON file with runtime variable overrides
        #[arg(long)]
        vars: Option<PathBuf>,
        /// Set a runtime variable (repeatable, e.g. --set name=Alice --set count=3)
        #[arg(long = "set", value_name = "KEY=VALUE", value_parser = parse_key_value)]
        set_vars: Vec<(String, String)>,
    },
    /// Create a starter MDS file
    Init {
        /// Output filename
        #[arg(default_value = "hello.mds")]
        filename: PathBuf,
        /// Overwrite existing file
        #[arg(long)]
        force: bool,
    },
}

fn parse_key_value(s: &str) -> std::result::Result<(String, String), String> {
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
fn parse_cli_value(val: String) -> mds::Value {
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
/// Errors created via `miette::miette!()` in main.rs do NOT downcast to `MdsError`
/// and correctly fall through to exit code 1. Only `MdsError` values converted via
/// `.map_err(miette::Error::from)` are categorized.
fn exit_code(err: &miette::Error) -> i32 {
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

fn main() {
    let cli = Cli::parse();

    let result = run(cli);
    if let Err(e) = result {
        eprintln!("{e:?}");
        process::exit(exit_code(&e));
    }
}

/// Load vars from an optional file path, returning None if no file was given.
fn load_vars_file(
    path: Option<PathBuf>,
) -> Result<Option<HashMap<String, mds::Value>>, miette::Error> {
    path.map(|p| mds::load_vars_file(&p).map_err(|e| miette::miette!("{e}")))
        .transpose()
}

/// Merge a `--vars` file with any `--set key=value` overrides into a single map.
fn build_runtime_vars(
    vars: Option<PathBuf>,
    set_vars: Vec<(String, String)>,
) -> Result<Option<HashMap<String, mds::Value>>, miette::Error> {
    let mut runtime_vars = load_vars_file(vars)?;
    for (key, val) in set_vars {
        runtime_vars
            .get_or_insert_with(HashMap::new)
            .insert(key, parse_cli_value(val));
    }
    Ok(runtime_vars)
}

/// Return an error if the input path is a directory (only file or stdin allowed).
fn reject_directory_input(input: &Path) -> Result<(), miette::Error> {
    if input != Path::new("-") && input.is_dir() {
        return Err(miette::miette!(
            "expected a file, got a directory: {}",
            input.display()
        ));
    }
    Ok(())
}

/// Maximum stdin bytes accepted — shares the per-file limit from the library.
use mds::MAX_FILE_SIZE as MAX_STDIN_SIZE;

/// Read from stdin and return the source string along with the current working directory.
///
/// Reads at most `MAX_STDIN_SIZE + 1` bytes so we can detect over-sized input without
/// buffering the entire stream first.
fn read_stdin() -> Result<(String, std::path::PathBuf), miette::Error> {
    let mut source = String::new();
    std::io::stdin()
        .take(MAX_STDIN_SIZE + 1)
        .read_to_string(&mut source)
        .map_err(|e| miette::miette!("cannot read stdin: {e}"))?;
    if source.len() as u64 > MAX_STDIN_SIZE {
        return Err(miette::miette!("stdin input exceeds maximum size of 10 MB"));
    }
    let cwd = std::env::current_dir()
        .map_err(|e| miette::miette!("cannot determine current directory: {e}"))?;
    Ok((source, cwd))
}

/// Resolve the input path: use the explicit value, or auto-detect from cwd.
///
/// Returns `(path, auto_detected)` where `auto_detected` is `true` when the path was
/// discovered via `auto_detect_mds_file()` rather than supplied explicitly by the caller.
/// Callers can use the flag to decide whether to print a banner (e.g. `run_build`).
///
/// Does not check for directory or validate the file; callers perform those checks after resolution.
fn resolve_input(
    input: Option<PathBuf>,
) -> std::result::Result<(PathBuf, bool), miette::Error> {
    match input {
        Some(p) => Ok((p, false)),
        None => auto_detect_mds_file().map(|p| (p, true)),
    }
}

/// Write compiled output to a file or stdout.
///
/// When `output_path` is `Some(path)`, creates any missing parent directories,
/// writes the compiled string, and prints `"Compiled to {path}"` to stderr
/// unless `quiet` is set.  When `output_path` is `None`, prints the compiled
/// string to stdout with no trailing newline.
fn write_output(
    output_path: Option<PathBuf>,
    compiled: &str,
    quiet: bool,
) -> std::result::Result<(), miette::Error> {
    match output_path {
        Some(path) => {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        miette::miette!(
                            "cannot create output directory {}: {e}",
                            parent.display()
                        )
                    })?;
                }
            }
            std::fs::write(&path, compiled).map_err(|e| {
                miette::miette!("cannot write {}: {e}", path.display())
            })?;
            if !quiet {
                eprintln!("Compiled to {}", path.display());
            }
        }
        None => {
            print!("{compiled}");
        }
    }
    Ok(())
}

fn run_build(
    input: Option<PathBuf>,
    output: Option<String>,
    out_dir: Option<PathBuf>,
    vars: Option<PathBuf>,
    set_vars: Vec<(String, String)>,
    quiet: bool,
) -> Result<(), miette::Error> {
    let runtime_vars = build_runtime_vars(vars, set_vars)?;

    // Resolve the input: explicit path, or auto-detect from cwd.
    // When auto-detected, print a "Building {path}" banner so users know which file was selected.
    let (input, auto_detected) = resolve_input(input)?;
    if auto_detected && !quiet {
        eprintln!("Building {}", input.display());
    }

    reject_directory_input(&input)?;

    // Load project config (mds.json), walking up from the input file.
    let config = load_config(&input)?;

    // Resolve output destination before compiling (config discovery happens once).
    let output_path = resolve_output_path(&Some(input.clone()), &output, &out_dir, &config)?;

    let (compiled, warnings) = if input == Path::new("-") {
        let (source, cwd) = read_stdin()?;
        mds::compile_str_collecting_warnings(&source, Some(&cwd), runtime_vars)
            .map_err(miette::Error::from)?
    } else {
        mds::compile_collecting_warnings(&input, runtime_vars)
            .map_err(miette::Error::from)?
    };

    if !quiet {
        for w in &warnings {
            eprintln!("{w}");
        }
    }

    write_output(output_path, &compiled, quiet)
}

fn run_check(
    input: Option<PathBuf>,
    vars: Option<PathBuf>,
    set_vars: Vec<(String, String)>,
    quiet: bool,
) -> Result<(), miette::Error> {
    let runtime_vars = build_runtime_vars(vars, set_vars)?;

    // Resolve the input: explicit path/stdin, or auto-detect from cwd.
    // run_check does not print a banner on auto-detect — check is a silent validation.
    let (input, _auto_detected) = resolve_input(input)?;

    reject_directory_input(&input)?;

    if input == Path::new("-") {
        let (source, cwd) = read_stdin()?;
        let ((), warnings) =
            mds::check_str_collecting_warnings(&source, Some(&cwd), runtime_vars)
                .map_err(miette::Error::from)?;
        if !quiet {
            for w in &warnings {
                eprintln!("{w}");
            }
            eprintln!("OK: <stdin>");
        }
    } else {
        let ((), warnings) = mds::check_collecting_warnings(&input, runtime_vars)
            .map_err(miette::Error::from)?;
        if !quiet {
            for w in &warnings {
                eprintln!("{w}");
            }
            eprintln!("OK: {}", input.display());
        }
    }
    Ok(())
}

fn run_init(filename: PathBuf, force: bool, quiet: bool) -> Result<(), miette::Error> {
    if filename
        .components()
        .any(|c| c == std::path::Component::ParentDir)
    {
        return Err(miette::miette!(
            "init filename must not contain '..' components"
        ));
    }
    if filename.exists() && !force {
        return Err(miette::miette!(
            "{} already exists (use --force to overwrite)",
            filename.display()
        ));
    }
    let starter = "\
---
name: World
items: [one, two, three]
---

Hello {name}!

Your items:
@for item in items:
- {item}
@end
";
    std::fs::write(&filename, starter)
        .map_err(|e| miette::miette!("cannot write {}: {e}", filename.display()))?;
    if !quiet {
        eprintln!(
            "Created {}\n  Try: mds build {}",
            filename.display(),
            filename.display()
        );
    }
    Ok(())
}

fn run(cli: Cli) -> Result<(), miette::Error> {
    let quiet = cli.quiet;
    match cli.command {
        Commands::Build {
            input,
            output,
            out_dir,
            vars,
            set_vars,
        } => run_build(input, output, out_dir, vars, set_vars, quiet),
        Commands::Check {
            input,
            vars,
            set_vars,
        } => run_check(input, vars, set_vars, quiet),
        Commands::Init { filename, force } => run_init(filename, force, quiet),
    }
}

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
        let result = resolve_output_path(
            &Some(PathBuf::from("-")),
            &None,
            &None,
            &None,
        )
        .unwrap();
        assert_eq!(result, None, "stdin input with no -o should resolve to stdout");
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
