//! `pares-agens` CLI binary.
//!
//! # Usage
//!
//! ```text
//! pares-agens migrate [--from ~/.openclaw] [--output ./migration] [--dry-run]
//! ```

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use pares_agens_migrate::{migrate, openclaw};

#[derive(Debug, Parser)]
#[command(
    name = "pares-agens",
    version,
    about = "Pares Agens agent runtime CLI",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Migrate data from an existing OpenClaw installation.
    Migrate {
        /// Path to the OpenClaw installation directory.
        ///
        /// When omitted, the tool auto-detects the default location
        /// (`~/.openclaw`).
        #[arg(long, value_name = "PATH")]
        from: Option<PathBuf>,

        /// Directory to write migrated output files.
        ///
        /// Defaults to `./migration` in the current working directory.
        #[arg(long, value_name = "PATH", default_value = "migration")]
        output: PathBuf,

        /// Simulate the migration without writing any files.
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Migrate { from, output, dry_run } => {
            let source = match from.or_else(openclaw::auto_detect) {
                Some(p) => p,
                None => {
                    eprintln!(
                        "No OpenClaw installation found. \
                         Use --from <PATH> to specify one."
                    );
                    std::process::exit(1);
                }
            };
            match migrate::run(&source, &output, dry_run) {
                Ok(report) => {
                    report.print();
                }
                Err(e) => {
                    eprintln!("Migration failed: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
}
