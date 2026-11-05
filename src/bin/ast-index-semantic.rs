//! Explicit producer entry point, separate from the read-only query service.
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Produce private local semantic evidence in an offline sandbox")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Refresh one Rust project using the packaged, execution-disabled producer.
    Rust {
        #[arg(long)]
        root: PathBuf,
        /// Existing private directory or a new directory below an existing parent.
        #[arg(long)]
        state_dir: PathBuf,
    },
    /// Refresh one TypeScript project with the pinned offline native compiler.
    Typescript {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        state_dir: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    let stats = match Cli::parse().command {
        Command::Rust { root, state_dir } => ast_index::producer::rust(&root, &state_dir)?,
        Command::Typescript { root, state_dir } => {
            ast_index::producer::typescript(&root, &state_dir)?
        }
    };
    println!("{}", serde_json::to_string(&stats)?);
    Ok(())
}
