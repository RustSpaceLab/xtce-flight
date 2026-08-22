//! `xtce-flight` — compile an XTCE definition into on-board Rust.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use xtce_model::XtceDb;

#[derive(Parser)]
#[command(name = "xtce-flight", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate a `no_std` encoder and decoder.
    Generate {
        /// XTCE definition.
        definition: PathBuf,

        /// Container to start from. Defaults to the definition's own root.
        #[arg(long)]
        root: Option<String>,

        /// Write here instead of standard output.
        #[arg(long, short)]
        out: Option<PathBuf>,
    },

    /// Report what would be generated, without generating it.
    Plan {
        /// XTCE definition.
        definition: PathBuf,

        /// Container to start from.
        #[arg(long)]
        root: Option<String>,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            let mut source = std::error::Error::source(&*error);
            while let Some(cause) = source {
                eprintln!("  caused by: {cause}");
                source = cause.source();
            }
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Command::Generate {
            definition,
            root,
            out,
        } => {
            let db = XtceDb::from_path(&definition)?;
            let options = xtce_flight::Options {
                root,
                source_label: Some(definition.display().to_string()),
            };
            let source = xtce_flight::generate(&db, &options)?;
            match out {
                Some(path) => {
                    std::fs::write(&path, source)?;
                    eprintln!("wrote {}", path.display());
                    eprintln!(
                        "include it inside a module carrying the lint allowances generated \
                         code needs; the file's header shows how"
                    );
                }
                None => print!("{source}"),
            }
            Ok(())
        }

        Command::Plan { definition, root } => {
            let db = XtceDb::from_path(&definition)?;
            let options = xtce_flight::Options {
                root,
                source_label: None,
            };
            let layout = xtce_flight::layout(&db, &options)?;

            println!("{}", definition.display());
            println!(
                "  rooted at {}: {} container(s), {} enumeration(s)",
                layout.root_name,
                layout.containers.len(),
                layout.enums.len()
            );
            for container in &layout.containers {
                println!(
                    "  {:<30} {:>5} byte(s)  {:>3} field(s)  {:>2} criterion(s)",
                    container.xtce_name,
                    container.len_bytes,
                    container.fields.len(),
                    container.constants.len(),
                );
            }
            Ok(())
        }
    }
}
