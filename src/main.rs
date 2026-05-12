use std::path::{Path, PathBuf};
use std::process;

use clap::{Parser, Subcommand};
use miette::Result;

#[derive(Parser)]
#[command(name = "mds", version, about = "MDS (Markdown Script) compiler")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Compile an MDS file to Markdown
    Build {
        /// Input .mds file (use "-" to read from stdin)
        input: PathBuf,
        /// Output file (stdout if omitted)
        #[arg(short = 'o', long = "output")]
        output: Option<PathBuf>,
        /// JSON file with runtime variable overrides
        #[arg(long)]
        vars: Option<PathBuf>,
    },
    /// Validate an MDS file without rendering
    Check {
        /// Input .mds file
        input: PathBuf,
        /// JSON file with runtime variable overrides
        #[arg(long)]
        vars: Option<PathBuf>,
    },
    /// Create a starter MDS file
    Init {
        /// Output filename
        #[arg(default_value = "hello.mds")]
        filename: PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();

    let result = run(cli);
    if let Err(e) = result {
        eprintln!("{:?}", e);
        process::exit(1);
    }
}

fn load_runtime_vars(
    vars: Option<PathBuf>,
) -> Result<Option<std::collections::HashMap<String, mds::value::Value>>, miette::Error> {
    vars.map(|path| mds::load_vars_file(&path).map_err(|e| miette::miette!("{e}")))
        .transpose()
}

fn run(cli: Cli) -> Result<(), miette::Error> {
    match cli.command {
        Commands::Build { input, output, vars } => {
            let runtime_vars = load_runtime_vars(vars)?;

            let compiled = if input == Path::new("-") {
                // Read from stdin
                let source = std::io::read_to_string(std::io::stdin())
                    .map_err(|e| miette::miette!("cannot read stdin: {e}"))?;
                let cwd = std::env::current_dir()
                    .map_err(|e| miette::miette!("cannot determine current directory: {e}"))?;
                mds::compile_str(&source, Some(&cwd), runtime_vars)
                    .map_err(miette::Error::from)?
            } else {
                mds::compile(&input, runtime_vars).map_err(miette::Error::from)?
            };

            if let Some(output_path) = output {
                std::fs::write(&output_path, &compiled)
                    .map_err(|e| miette::miette!("cannot write {}: {e}", output_path.display()))?;
                eprintln!("Compiled to {}", output_path.display());
            } else {
                print!("{compiled}");
            }
            Ok(())
        }
        Commands::Check { input, vars } => {
            let runtime_vars = load_runtime_vars(vars)?;
            mds::check(&input, runtime_vars).map_err(miette::Error::from)?;
            eprintln!("OK: {}", input.display());
            Ok(())
        }
        Commands::Init { filename } => {
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
            eprintln!("Created {}", filename.display());
            Ok(())
        }
    }
}
