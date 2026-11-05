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
    Rust(Arguments),
    /// Refresh one TypeScript project with the pinned offline native compiler.
    Typescript(Arguments),
}

#[derive(clap::Args)]
struct Arguments {
    #[arg(long)]
    root: PathBuf,
    /// Existing private directory or a new directory below an existing parent.
    #[arg(long)]
    state_dir: PathBuf,
    /// Include existing project-local vendor/ (Rust) or node_modules/ (TypeScript).
    /// Requires regular files; never fetches, installs or executes packages.
    #[arg(long)]
    with_dependencies: bool,
}

impl Arguments {
    fn options(&self) -> ast_index::producer::Options {
        ast_index::producer::Options {
            dependencies: self.with_dependencies,
        }
    }
}

fn main() -> anyhow::Result<()> {
    let stats = match Cli::parse().command {
        Command::Rust(args) => {
            ast_index::producer::rust(&args.root, &args.state_dir, &args.options())?
        }
        Command::Typescript(args) => {
            ast_index::producer::typescript(&args.root, &args.state_dir, &args.options())?
        }
    };
    println!("{}", serde_json::to_string(&stats)?);
    Ok(())
}
