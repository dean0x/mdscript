use std::path::PathBuf;
use std::process;

use clap::{Parser, Subcommand};
use miette::Result;

mod build;
mod watch;

use build::{
    build_runtime_vars, exit_code, parse_key_value, reject_directory_input, resolve_input,
    run_build, BuildArgs, OutputFormat,
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
    /// Suppress status messages
    #[arg(long, short = 'q', global = true)]
    quiet: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Compile an MDS file to Markdown
    #[command(
        after_help = "Examples:\n  mds build                                  Auto-detect the .mds file in current dir\n  mds build template.mds                     Compile to template.md (next to source)\n  mds build template.mds -o -               Compile to stdout\n  mds build template.mds -o output.md       Compile to specific file\n  mds build template.mds --out-dir dist     Compile to dist/template.md\n  mds build template.mds --vars vars.json   With variable overrides\n  mds build template.mds --set name=Alice   Set a single variable\n  mds build template.mds --format messages  Compile @message blocks to JSON\n  echo \"Hello {name}!\" | mds build -         Compile from stdin (writes to stdout)"
    )]
    Build {
        /// Input .mds file (use "-" for stdin; omit to auto-detect in current directory)
        input: Option<PathBuf>,
        /// Output destination: a file path, or "-" for stdout.
        /// Defaults to `<name>.md` next to the source file.
        /// Mutually exclusive with --out-dir.
        #[arg(short = 'o', long = "output", conflicts_with = "out_dir")]
        output: Option<String>,
        /// Output directory. The output file is named `<input-stem>.md` inside this directory.
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
        /// Output format: "markdown" (default) or "messages" (JSON array of chat messages)
        #[arg(long = "format", value_name = "FORMAT", default_value = "markdown")]
        format: OutputFormat,
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
    /// Watch an MDS file (or directory) and recompile on changes
    ///
    /// Single-file mode tracks transitive imports — editing any imported file
    /// triggers a recompile of the entry. Directory mode tracks a reverse-dependency
    /// graph: editing a shared partial recompiles all transitive importers.
    /// `_`-prefixed files are partials (tracked, not emitted to their own output).
    /// Cross-root imports are watched NonRecursively.
    ///
    /// Output mirrors the source subtree under `--out-dir` / `mds.json output_dir`.
    ///
    /// A liveness-gated reconcile fallback re-arms watches each tick and does a full
    /// rescan only on watch loss/recovery. Use `--poll-interval 0` to disable.
    #[command(
        after_help = "Examples:\n  mds watch template.mds              Watch a single file, write template.md\n  mds watch template.mds -o out.md    Watch to a specific output file\n  mds watch template.mds -o -         Watch, stream output to stdout\n  mds watch .                         Watch all .mds files in current directory\n  mds watch src/ --out-dir dist       Watch directory, mirror to dist/ subtree\n  mds watch template.mds --vars v.json  Watch with variable overrides\n  mds watch template.mds --clear      Clear terminal before each rebuild\n  mds watch src/ --poll-interval 500  Self-heal check every 500ms\n  mds watch src/ --poll-interval 0    Disable self-heal (native events only)"
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
        /// Output format: "markdown" (default) or "messages".
        /// "messages" is only valid in single-file mode.
        #[arg(long = "format", value_name = "FORMAT", default_value = "markdown")]
        format: OutputFormat,
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
        eprintln!("{e:?}");
        process::exit(exit_code(&e));
    }
}

fn run_check(
    input: Option<PathBuf>,
    vars: Option<PathBuf>,
    set_vars: Vec<(String, String)>,
    quiet: bool,
) -> Result<()> {
    use build::read_stdin;
    let runtime_vars = build_runtime_vars(vars, set_vars)?;

    // Resolve the input: explicit path/stdin, or auto-detect from cwd.
    // run_check does not print a banner on auto-detect — check is a silent validation.
    let (input, _) = resolve_input(input)?;

    reject_directory_input(&input)?;

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

fn run(cli: Cli) -> Result<()> {
    let quiet = cli.quiet;
    match cli.command {
        Commands::Build {
            input,
            output,
            out_dir,
            vars,
            set_vars,
            format,
        } => run_build(BuildArgs {
            input,
            output,
            out_dir,
            vars,
            set_vars,
            quiet,
            format,
        }),
        Commands::Check {
            input,
            vars,
            set_vars,
        } => run_check(input, vars, set_vars, quiet),
        Commands::Init { filename, force } => run_init(filename, force, quiet),
        Commands::Watch {
            input,
            output,
            out_dir,
            vars,
            set_vars,
            format,
            clear,
            debounce,
            poll_interval,
        } => watch::run_watch(watch::WatchArgs {
            input,
            output,
            out_dir,
            vars,
            set_vars,
            format,
            clear,
            debounce,
            quiet,
            poll_interval,
        }),
    }
}

// The unit tests that were in main.rs have moved to build.rs.
// This file only contains integration-level wiring that is covered by the integration tests.
