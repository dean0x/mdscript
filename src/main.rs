use std::path::PathBuf;
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
        /// Input .mds file
        input: PathBuf,
        /// Output file (stdout if omitted)
        #[arg(short, long)]
        o: Option<PathBuf>,
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
}

fn main() {
    let cli = Cli::parse();

    let result = run(cli);
    if let Err(e) = result {
        eprintln!("{:?}", e);
        process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), miette::Error> {
    match cli.command {
        Commands::Build { input, o, vars } => {
            let runtime_vars = if let Some(vars_path) = vars {
                Some(mds::load_vars_file(&vars_path).map_err(|e| miette::miette!("{e}"))?)
            } else {
                None
            };

            let output =
                mds::compile(&input, runtime_vars).map_err(|e| miette::Error::from(e))?;

            if let Some(output_path) = o {
                std::fs::write(&output_path, &output)
                    .map_err(|e| miette::miette!("cannot write {}: {e}", output_path.display()))?;
                eprintln!("Compiled to {}", output_path.display());
            } else {
                print!("{output}");
            }
            Ok(())
        }
        Commands::Check { input, vars } => {
            let runtime_vars = if let Some(vars_path) = vars {
                Some(mds::load_vars_file(&vars_path).map_err(|e| miette::miette!("{e}"))?)
            } else {
                None
            };

            mds::check(&input, runtime_vars).map_err(|e| miette::Error::from(e))?;
            eprintln!("OK: {}", input.display());
            Ok(())
        }
    }
}
