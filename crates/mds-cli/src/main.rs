use std::path::PathBuf;
use std::process;

use clap::{Parser, Subcommand};
use miette::Result;

mod build;
mod fmt;
mod lint;
mod output;
mod watch;

use build::{
    build_runtime_vars, exit_code, parse_key_value, resolve_input, run_build, BuildArgs,
    RuntimeVarArgs,
};

// ── CLI entry point ───────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "mds",
    about = "MDS (Markdown Script) compiler",
    long_about = "MDS (Markdown Script) compiler — composable LLM prompt templates\n\nCompile .mds template files into Markdown. Use variables, loops,\nconditionals, functions, and imports to build reusable prompts.\n\nQuick start:\n  mds init                       Create a starter template\n  mds build hello.mds            Compile to hello.md\n  mds build hello.mds -o -       Compile to stdout\n  mds build hello.mds -o out.md  Compile to a specific file\n  mds watch hello.mds            Watch and recompile on save",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    /// Suppress status and diagnostic output; errors always print; exit codes unaffected
    #[arg(long, short = 'q', global = true)]
    quiet: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Compile an MDS file to Markdown or JSON messages (output shape is intrinsic)
    ///
    /// Templates with `@message` blocks compile to a JSON array (.json).
    /// All other templates compile to Markdown (.md).
    /// The output extension is derived automatically from the compiled kind.
    #[command(
        after_help = "Examples:\n  mds build                                  Auto-detect the .mds file in current dir\n  mds build template.mds                     Compile to template.md (next to source)\n  mds build chat.mds                         Compile @message template to chat.json\n  mds build template.mds -o -               Compile to stdout\n  mds build template.mds -o output.md       Compile to specific file\n  mds build template.mds --out-dir dist     Compile to dist/template.md or dist/template.json\n  mds build template.mds --vars vars.json   With variable overrides\n  mds build template.mds --set name=Alice   Set a single variable\n  echo \"Hello {{name}}!\" | mds build -       Compile from stdin (writes to stdout)"
    )]
    Build {
        /// Input .mds file (use "-" for stdin; omit to auto-detect in current directory)
        input: Option<PathBuf>,
        /// Output destination: a file path, or "-" for stdout.
        /// Defaults to `<name>.md` or `<name>.json` next to the source file, based on output kind.
        /// Mutually exclusive with --out-dir.
        #[arg(short = 'o', long = "output", conflicts_with = "out_dir")]
        output: Option<String>,
        /// Output directory. The output file is named `<input-stem>.md` or `<input-stem>.json`
        /// inside this directory, based on output kind.
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
        /// Set a runtime variable as a string (repeatable, no type coercion; e.g. --set-string count=3 sets count to the string "3")
        #[arg(long = "set-string", value_name = "KEY=VALUE", value_parser = parse_key_value)]
        set_string_vars: Vec<(String, String)>,
        /// Generate a source map alongside the compiled output (sidecar: <output-file>.map, e.g. -o out.md → out.md.map).
        /// Conflicts with --no-source-map.
        #[arg(long = "source-map", conflicts_with = "no_source_map")]
        source_map: bool,
        /// Disable source-map generation (overrides mds.json build.source_map=true).
        /// Conflicts with --source-map.
        #[arg(long = "no-source-map", conflicts_with = "source_map")]
        no_source_map: bool,
        /// Embed the source map as a data URI comment in the output file instead of a sidecar.
        /// Requires --source-map.
        #[arg(long = "inline", requires = "source_map")]
        inline: bool,
        /// Embed source file contents in sourcesContent[]. Ships full source text — use with care.
        /// Requires --source-map.
        #[arg(long = "embed-sources", requires = "source_map")]
        embed_sources: bool,
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
        /// Set a runtime variable as a string (repeatable, no type coercion; e.g. --set-string count=3 sets count to the string "3")
        #[arg(long = "set-string", value_name = "KEY=VALUE", value_parser = parse_key_value)]
        set_string_vars: Vec<(String, String)>,
    },
    /// Reformat MDS file(s) in place (opinionated, safety-gated)
    ///
    /// Rewrites are guaranteed compile-equivalent: a safety gate re-compiles the
    /// formatted source and refuses to write if it would change compiled output.
    /// Normalizes line endings to LF on directive lines, strips trailing
    /// whitespace on directive lines, and ensures exactly one final newline —
    /// never touches body-text content (Markdown hard breaks, blank-line
    /// structure, whitespace-only lines), frontmatter / code-fence internals,
    /// or the byte-for-byte content of `@message` / `@define` bodies.
    #[command(
        after_help = "Examples:\n  mds fmt                             Auto-detect and format the .mds file in current dir\n  mds fmt template.mds                Format a file in place\n  mds fmt .                           Format every .mds file recursively (incl. partials)\n  mds fmt --check template.mds        Exit 1 if the file would change; writes nothing\n  mds fmt --diff template.mds         Print a unified diff of pending changes; writes nothing\n  mds fmt --check --diff .            Show diffs for every file that would change, exit 1 if any would\n  printf '@if ready:   \\nGo\\n@end\\n' | mds fmt -  Format from stdin, write to stdout; creates no file"
    )]
    Fmt {
        /// Input .mds file, directory, or "-" for stdin (omit to auto-detect in current directory)
        input: Option<PathBuf>,
        /// Read-only: exit non-zero if any file would change; never writes
        #[arg(long)]
        check: bool,
        /// Read-only: print a unified diff of pending changes; never writes.
        /// Combines with --check (diff is the rendering, check is the exit behavior).
        #[arg(long)]
        diff: bool,
    },
    /// Check MDS files for style and correctness issues beyond `mds check`
    ///
    /// Runs 10 static-analysis rules (3 error-level, 6 warning-level, 1 default-off) on the file
    /// without executing it. Partials and imported files are included in directory mode.
    ///
    /// Exit codes: 0 = clean, 1 = warnings only, 2 = errors or analysis failure,
    /// 3 = resource limit.
    #[command(
        after_help = "Examples:\n  mds lint template.mds               Lint a single file\n  mds lint .                          Lint all .mds files recursively\n  mds lint --fix template.mds         Fix auto-fixable issues in place\n  mds lint --fix --check template.mds Preview fixes (exit 1 if any would apply)\n  mds lint --fix --diff template.mds  Show diff of pending fixes\n  mds lint --format json template.mds Machine-readable JSON output\n  mds lint --quiet template.mds       Suppress output; exits 1 on warnings, 2 on errors\n  cat template.mds | mds lint -       Lint from stdin\n  cat template.mds | mds lint --fix - Fix from stdin, write fixed source to stdout"
    )]
    Lint {
        /// Input .mds file, directory, or `-` for stdin (omit to auto-detect)
        input: Option<PathBuf>,
        /// Apply auto-fixable issues in place (Tier A always; Tier B when standalone)
        #[arg(long)]
        fix: bool,
        /// With --fix: exit 1 if any file would change; never writes
        #[arg(long, requires = "fix")]
        check: bool,
        /// With --fix: print unified diff of pending changes; never writes
        #[arg(long, requires = "fix")]
        diff: bool,
        /// Output format: `human` (default, stderr) or `json` (stdout)
        #[arg(long = "format", value_name = "FORMAT", default_value = "human")]
        format: String,
        /// JSON file with runtime variable overrides
        #[arg(long)]
        vars: Option<PathBuf>,
        /// Set a runtime variable (repeatable, e.g. --set name=Alice)
        #[arg(long = "set", value_name = "KEY=VALUE", value_parser = parse_key_value)]
        set_vars: Vec<(String, String)>,
        /// Set a runtime variable as a string (repeatable, no type coercion)
        #[arg(long = "set-string", value_name = "KEY=VALUE", value_parser = parse_key_value)]
        set_string_vars: Vec<(String, String)>,
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
    /// Watch an MDS file (or directory) and recompile on changes
    ///
    /// Single-file mode tracks transitive imports — editing any imported file
    /// triggers a recompile of the entry. Directory mode tracks a reverse-dependency
    /// graph: editing a shared partial recompiles all transitive importers.
    /// `_`-prefixed files are partials (tracked, not emitted to their own output).
    /// Cross-root imports are watched NonRecursively.
    ///
    /// Output mirrors the source subtree under `--out-dir` / `mds.json output_dir`.
    /// The output extension is determined by the compiled kind (.md or .json).
    ///
    /// A liveness-gated reconcile fallback re-arms watches each tick and does a full
    /// rescan only on watch loss/recovery. Use `--poll-interval 0` to disable.
    #[command(
        after_help = "Examples:\n  mds watch template.mds              Watch a single file, write template.md\n  mds watch chat.mds                  Watch @message template, write chat.json\n  mds watch template.mds -o out.md    Watch to a specific output file\n  mds watch template.mds -o -         Watch, stream output to stdout\n  mds watch .                         Watch all .mds files in current directory\n  mds watch src/ --out-dir dist       Watch directory, mirror to dist/ subtree\n  mds watch template.mds --vars v.json  Watch with variable overrides\n  mds watch template.mds --clear      Clear terminal before each rebuild\n  mds watch src/ --poll-interval 500  Self-heal check every 500ms\n  mds watch src/ --poll-interval 0    Disable self-heal (native events only)"
    )]
    Watch {
        /// File or directory to watch. Omit to auto-detect a single .mds file.
        /// Use "-" to read from stdin (not supported — use build instead).
        input: Option<PathBuf>,
        /// Output destination: a file path, or "-" for stdout.
        /// Mutually exclusive with --out-dir. Not allowed in directory mode.
        #[arg(short = 'o', long = "output", conflicts_with = "out_dir")]
        output: Option<String>,
        /// Output directory for compiled files (directory mode).
        /// Output mirrors the source subtree: src/a/b/foo.mds → out/a/b/foo.md.
        /// Mutually exclusive with -o/--output.
        #[arg(long = "out-dir", conflicts_with = "output")]
        out_dir: Option<PathBuf>,
        /// JSON file with runtime variable overrides (reloaded on each rebuild)
        #[arg(long)]
        vars: Option<PathBuf>,
        /// Set a runtime variable (repeatable, e.g. --set name=Alice --set count=3)
        #[arg(long = "set", value_name = "KEY=VALUE", value_parser = parse_key_value)]
        set_vars: Vec<(String, String)>,
        /// Set a runtime variable as a string (repeatable, no type coercion; e.g. --set-string count=3 sets count to the string "3")
        #[arg(long = "set-string", value_name = "KEY=VALUE", value_parser = parse_key_value)]
        set_string_vars: Vec<(String, String)>,
        /// Clear the terminal before each rebuild (only when stderr is a TTY)
        #[arg(long)]
        clear: bool,
        /// Debounce window in milliseconds (default 100; use 0 for immediate rebuilds).
        /// Controls how long to wait for burst coalescing after the first event.
        #[arg(long = "debounce", value_name = "MS", default_value = "100")]
        debounce: u64,
        /// Self-heal poll interval in milliseconds (default 1000).
        /// Each tick re-arms watches and runs a liveness check; a full rescan only
        /// runs on watch loss/recovery. Use 0 to disable (native events only).
        /// Non-zero values are clamped to a 50ms minimum.
        #[arg(long = "poll-interval", value_name = "MS", default_value = "1000")]
        poll_interval: u64,
    },
}

fn main() {
    let cli = Cli::parse();

    let result = run(cli);
    if let Err(e) = result {
        // Sanitize at the last-resort render boundary: every subcommand's error propagates
        // here, and MdsError::Syntax embeds user-controlled source fragments that may contain
        // raw ESC bytes. Guarding here makes the protection hold by construction for any
        // future error path, not just the ones we remember to sanitize individually (PF-004).
        eprintln!("{}", mds::sanitize_control_chars(&format!("{e:?}")));
        process::exit(exit_code(&e));
    }
}

fn run_check(
    input: Option<PathBuf>,
    vars: Option<PathBuf>,
    set_vars: Vec<(String, String)>,
    set_string_vars: Vec<(String, String)>,
    quiet: bool,
) -> Result<()> {
    use build::read_stdin;
    let runtime_vars = build_runtime_vars(RuntimeVarArgs {
        vars,
        set_vars,
        set_string_vars,
    })?;

    // Resolve the input: explicit path/stdin, or auto-detect from cwd.
    // run_check does not print a banner on auto-detect — check is a silent validation.
    let (input, _) = resolve_input(input, "check")?;

    // Directory mode: validate every non-partial .mds file in the tree.
    if input != std::path::Path::new("-") && input.is_dir() {
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
        return run_check_directory(&input, runtime_vars, quiet);
    }

    // Single-file / stdin path.
    if input == std::path::Path::new("-") {
        let (source, cwd) = read_stdin()?;
        let ((), warnings) = mds::check_str_collecting_warnings(&source, Some(&cwd), runtime_vars)
            .map_err(miette::Error::from)?;
        if !quiet {
            for w in &warnings {
                eprintln!("{w}");
            }
            eprintln!("OK: <stdin>");
        }
    } else {
        let ((), warnings) =
            mds::check_collecting_warnings(&input, runtime_vars).map_err(miette::Error::from)?;
        if !quiet {
            for w in &warnings {
                eprintln!("{w}");
            }
            eprintln!("OK: {}", input.display());
        }
    }
    Ok(())
}

/// Validate every non-partial `.mds` file under `dir`.
///
/// Continue-on-error: a per-file error does not abort the run. Prints a summary and
/// returns non-zero if any file fails (AC-FUNC-26).
fn run_check_directory(
    dir: &std::path::Path,
    runtime_vars: Option<std::collections::HashMap<String, mds::Value>>,
    quiet: bool,
) -> Result<()> {
    use output::{collect_mds_files_detailed, is_partial};

    const MAX_DEPTH: usize = 64;

    let walk = collect_mds_files_detailed(dir, MAX_DEPTH, None);
    let files = walk.files;

    if files.is_empty() {
        if walk.excluded_by_default > 0 {
            // Always emit — not suppressed by --quiet (avoids silent CI green pass).
            eprintln!(
                "{} .mds file(s) found but all are under default-excluded directories \
                 (hidden dirs, node_modules); nothing was checked",
                walk.excluded_by_default
            );
            std::process::exit(1);
        }
        if !quiet {
            eprintln!("No .mds files found in {}", dir.display());
        }
        return Ok(());
    }

    let mut ok_count: usize = 0;
    let mut fail_count: usize = 0;

    for file in &files {
        if is_partial(file) {
            continue;
        }
        match mds::check_collecting_warnings(file, runtime_vars.clone())
            .map_err(miette::Error::from)
        {
            Ok(((), warnings)) => {
                if !quiet {
                    for w in &warnings {
                        eprintln!("{w}");
                    }
                }
                ok_count += 1;
            }
            Err(e) => {
                // Sanitize at the render boundary: MdsError::Syntax embeds user-controlled
                // source fragments that may contain raw ESC bytes (PF-004 parallel-path guard).
                eprintln!("{}", mds::sanitize_control_chars(&format!("{e:?}")));
                fail_count += 1;
            }
        }
    }

    if !quiet || fail_count > 0 {
        eprintln!("{ok_count} passed, {fail_count} failed");
    }

    if fail_count > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn run_init(filename: PathBuf, force: bool, quiet: bool) -> Result<()> {
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
<!-- Frontmatter above is emitted to output verbatim; runtime --set/--vars change only the body below -->

Hello {{name}}!

Your items:
@for item in items:
- {{item}}
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

fn run(cli: Cli) -> Result<()> {
    let quiet = cli.quiet;
    match cli.command {
        Commands::Build {
            input,
            output,
            out_dir,
            vars,
            set_vars,
            set_string_vars,
            source_map,
            no_source_map,
            inline,
            embed_sources,
        } => run_build(BuildArgs {
            input,
            output,
            out_dir,
            vars,
            set_vars,
            set_string_vars,
            quiet,
            source_map,
            no_source_map,
            inline,
            embed_sources,
        }),
        Commands::Check {
            input,
            vars,
            set_vars,
            set_string_vars,
        } => run_check(input, vars, set_vars, set_string_vars, quiet),
        Commands::Fmt { input, check, diff } => fmt::run_fmt(fmt::FmtArgs {
            input,
            check,
            diff,
            quiet,
        }),
        Commands::Lint {
            input,
            fix,
            check,
            diff,
            format,
            vars,
            set_vars,
            set_string_vars,
        } => {
            let fmt = match format.as_str() {
                "human" => lint::LintFormat::Human,
                "json" => lint::LintFormat::Json,
                other => {
                    eprintln!(
                        "error: unknown --format value '{other}'; expected 'human' or 'json'"
                    );
                    std::process::exit(2);
                }
            };
            lint::run_lint(lint::LintArgs {
                input,
                fix,
                check,
                diff,
                quiet,
                format: fmt,
                vars,
                set_vars,
                set_string_vars,
            })
        }
        Commands::Init { filename, force } => run_init(filename, force, quiet),
        Commands::Watch {
            input,
            output,
            out_dir,
            vars,
            set_vars,
            set_string_vars,
            clear,
            debounce,
            poll_interval,
        } => watch::run_watch(watch::WatchArgs {
            input,
            output,
            out_dir,
            vars,
            set_vars,
            set_string_vars,
            clear,
            debounce,
            quiet,
            poll_interval,
        }),
    }
}

// This file only contains integration-level wiring that is covered by the integration tests.
