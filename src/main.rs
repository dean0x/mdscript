use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process;

use clap::{Parser, Subcommand};
use miette::Result;

#[derive(Parser)]
#[command(
    name = "mds",
    about = "MDS (Markdown Script) compiler",
    long_about = "MDS (Markdown Script) compiler — composable LLM prompt templates\n\nCompile .mds template files into Markdown. Use variables, loops,\nconditionals, functions, and imports to build reusable prompts.\n\nQuick start:\n  mds init                       Create a starter template\n  mds build hello.mds            Compile to stdout\n  mds build hello.mds -o out.md  Compile to file",
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
        after_help = "Examples:\n  mds build template.mds                     Compile to stdout\n  mds build template.mds -o output.md        Compile to file\n  mds build template.mds --vars vars.json    With variable overrides\n  mds build template.mds --set name=Alice    Set a single variable\n  echo \"Hello {name}!\" | mds build -          Compile from stdin"
    )]
    Build {
        /// Input .mds file (use "-" to read from stdin)
        input: PathBuf,
        /// Output file (stdout if omitted)
        #[arg(short = 'o', long = "output")]
        output: Option<PathBuf>,
        /// JSON file with runtime variable overrides
        #[arg(long)]
        vars: Option<PathBuf>,
        /// Set a runtime variable (repeatable, e.g. --set name=Alice --set count=3)
        #[arg(long = "set", value_name = "KEY=VALUE", value_parser = parse_key_value)]
        set_vars: Vec<(String, String)>,
    },
    /// Validate an MDS file without rendering
    Check {
        /// Input .mds file (use "-" to read from stdin)
        input: PathBuf,
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
    if let Ok(f) = val.parse::<f64>() {
        return mds::Value::Number(f);
    }

    // Simple bracket-list: "[a, b, c]" → Array of strings.
    // Only handles flat lists of unquoted tokens; does not recurse.
    if val.starts_with('[') && val.ends_with(']') {
        let inner = &val[1..val.len() - 1];
        let items: Vec<mds::Value> = inner
            .split(',')
            .map(|s| mds::Value::String(s.trim().to_string()))
            .collect();
        return mds::Value::Array(items);
    }

    mds::Value::String(val)
}

fn main() {
    let cli = Cli::parse();

    let result = run(cli);
    if let Err(e) = result {
        eprintln!("{e:?}");
        process::exit(1);
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

/// Exit with an error if the input path is a directory (only file or stdin allowed).
fn reject_directory_input(input: &Path) {
    if input != Path::new("-") && input.is_dir() {
        eprintln!(
            "error: expected a file, got a directory: {}",
            input.display()
        );
        process::exit(1);
    }
}

/// Read from stdin and return the source string along with the current working directory.
fn read_stdin() -> Result<(String, std::path::PathBuf), miette::Error> {
    let source = std::io::read_to_string(std::io::stdin())
        .map_err(|e| miette::miette!("cannot read stdin: {e}"))?;
    let cwd = std::env::current_dir()
        .map_err(|e| miette::miette!("cannot determine current directory: {e}"))?;
    Ok((source, cwd))
}

fn run(cli: Cli) -> Result<(), miette::Error> {
    let quiet = cli.quiet;
    match cli.command {
        Commands::Build {
            input,
            output,
            vars,
            set_vars,
        } => {
            let runtime_vars = build_runtime_vars(vars, set_vars)?;
            reject_directory_input(&input);

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

            if let Some(output_path) = output {
                std::fs::write(&output_path, &compiled)
                    .map_err(|e| miette::miette!("cannot write {}: {e}", output_path.display()))?;
                if !quiet {
                    eprintln!("Compiled to {}", output_path.display());
                }
            } else {
                print!("{compiled}");
            }
            Ok(())
        }
        Commands::Check {
            input,
            vars,
            set_vars,
        } => {
            let runtime_vars = build_runtime_vars(vars, set_vars)?;
            reject_directory_input(&input);

            if input == Path::new("-") {
                let (source, cwd) = read_stdin()?;
                mds::check_str_with(&source, Some(&cwd), runtime_vars)
                    .map_err(miette::Error::from)?;
                if !quiet {
                    eprintln!("OK: <stdin>");
                }
            } else {
                mds::check(&input, runtime_vars).map_err(miette::Error::from)?;
                if !quiet {
                    eprintln!("OK: {}", input.display());
                }
            }
            Ok(())
        }
        Commands::Init { filename, force } => {
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
    }
}
