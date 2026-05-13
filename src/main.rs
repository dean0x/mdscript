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

fn main() {
    let cli = Cli::parse();

    let result = run(cli);
    if let Err(e) = result {
        eprintln!("{e:?}");
        process::exit(1);
    }
}

fn load_runtime_vars(
    vars: Option<PathBuf>,
) -> Result<Option<HashMap<String, mds::Value>>, miette::Error> {
    vars.map(|path| mds::load_vars_file(&path).map_err(|e| miette::miette!("{e}")))
        .transpose()
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
            let mut runtime_vars = load_runtime_vars(vars)?;
            for (key, val) in set_vars {
                runtime_vars
                    .get_or_insert_with(HashMap::new)
                    .insert(key, mds::Value::String(val));
            }

            if input != Path::new("-") && input.is_dir() {
                eprintln!("error: expected a file, got a directory: {}", input.display());
                process::exit(1);
            }

            let compiled = if input == Path::new("-") {
                // Read from stdin
                let source = std::io::read_to_string(std::io::stdin())
                    .map_err(|e| miette::miette!("cannot read stdin: {e}"))?;
                let cwd = std::env::current_dir()
                    .map_err(|e| miette::miette!("cannot determine current directory: {e}"))?;
                mds::compile_str_with(&source, Some(&cwd), runtime_vars)
                    .map_err(miette::Error::from)?
            } else {
                mds::compile(&input, runtime_vars).map_err(miette::Error::from)?
            };

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
            let mut runtime_vars = load_runtime_vars(vars)?;
            for (key, val) in set_vars {
                runtime_vars
                    .get_or_insert_with(HashMap::new)
                    .insert(key, mds::Value::String(val));
            }

            if input != Path::new("-") && input.is_dir() {
                eprintln!("error: expected a file, got a directory: {}", input.display());
                process::exit(1);
            }

            if input == Path::new("-") {
                let source = std::io::read_to_string(std::io::stdin())
                    .map_err(|e| miette::miette!("cannot read stdin: {e}"))?;
                let cwd = std::env::current_dir()
                    .map_err(|e| miette::miette!("cannot determine current directory: {e}"))?;
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
