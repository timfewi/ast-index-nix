//! `ast-index` command line interface: indexing, queries, the stdio MCP adapter
//! and the persistent socket service.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde::Serialize;

use ast_index::engine::Engine;
use ast_index::error::Error;
use ast_index::index::IndexOptions;
use ast_index::report;
use ast_index::rpc::RpcServer;
use ast_index::serve::{self, ServeOptions};

/// Lightweight AST index for local agent harnesses.
#[derive(Debug, Parser)]
#[command(name = "ast-index", version, about, long_about = None)]
struct Cli {
    /// Indexed root directory (default: current directory).
    #[arg(long, global = true, env = "AST_INDEX_ROOT", value_name = "PATH")]
    root: Option<PathBuf>,

    /// Database file (default: <root>/.ast-index/index.sqlite).
    #[arg(long, global = true, env = "AST_INDEX_DB", value_name = "PATH")]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Build or update the index.
    Index {
        /// Re-parse every file, ignoring size and mtime.
        #[arg(long)]
        force: bool,
        /// gitignore-style path patterns to exclude (repeatable).
        #[arg(long = "exclude", value_name = "GLOB")]
        exclude: Vec<String>,
        /// Print the summary as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show index coverage.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// List the definitions of one file.
    Outline {
        file: String,
        #[arg(long)]
        json: bool,
    },
    /// Search definitions by name or qualified name.
    Search {
        query: String,
        #[arg(long, default_value_t = 25)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Show one symbol with its callers and callees.
    Describe {
        symbol: String,
        #[arg(long)]
        json: bool,
    },
    /// Show incoming call edges for a symbol.
    Callers {
        symbol: String,
        #[arg(long)]
        json: bool,
    },
    /// Show outgoing call edges for a symbol.
    Callees {
        symbol: String,
        #[arg(long)]
        json: bool,
    },
    /// Show transitive callers of a symbol.
    Impact {
        symbol: String,
        #[arg(long, default_value_t = 3)]
        depth: u32,
        #[arg(long)]
        json: bool,
    },
    /// Show stored references of one file.
    Refs {
        file: String,
        #[arg(long)]
        json: bool,
    },
    /// Serve the MCP surface on stdio, or proxy to a socket service.
    Mcp {
        /// Proxy stdio to an already running socket service.
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Run the persistent Unix socket service.
    Serve {
        /// Socket path, for example /run/user/1000/ast-index.sock.
        #[arg(long)]
        socket: PathBuf,
        /// Lock file (default: the socket path with a .lock suffix).
        #[arg(long)]
        lock: Option<PathBuf>,
        /// gitignore-style path patterns to exclude when indexing at startup.
        #[arg(long = "exclude", value_name = "GLOB")]
        exclude: Vec<String>,
        /// Index once at startup before serving.
        #[arg(long)]
        index: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Error::NotIndexed(message)) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> ast_index::Result<()> {
    let root = match &cli.root {
        Some(root) => root.clone(),
        None => std::env::current_dir().map_err(|error| Error::io(".", error))?,
    };

    match cli.command {
        Command::Serve {
            socket,
            lock,
            exclude,
            index,
        } => {
            let engine = Engine::open(&root, cli.db)?;
            if index {
                let stats = engine.index(&IndexOptions {
                    exclude,
                    ..IndexOptions::default()
                })?;
                eprintln!("{}", report::index_text(&stats));
            }
            let server = RpcServer::new(engine);
            serve::run_socket(server, &ServeOptions { socket, lock })
        }
        Command::Mcp { socket } => {
            if let Some(socket) = socket {
                serve::run_proxy(&socket)
            } else {
                let engine = Engine::open(&root, cli.db)?;
                serve::run_stdio(&RpcServer::new(engine))
            }
        }
        command => {
            let engine = Engine::open(&root, cli.db)?;
            run_query(&engine, command)
        }
    }
}

fn run_query(engine: &Engine, command: Command) -> ast_index::Result<()> {
    match command {
        Command::Index {
            force,
            exclude,
            json,
        } => {
            let stats = engine.index(&IndexOptions {
                force,
                exclude,
                ..IndexOptions::default()
            })?;
            print_json(&stats, json, || report::index_text(&stats))?;
        }
        Command::Status { json } => {
            let status = engine.status()?;
            print_json(&status, json, || report::status_text(&status))?;
        }
        Command::Outline { file, json } => {
            let symbols = engine.outline(&file)?;
            print_json(&symbols, json, || report::symbols_text(&symbols))?;
        }
        Command::Search { query, limit, json } => {
            let symbols = engine.search(&query, limit.clamp(1, 100))?;
            print_json(&symbols, json, || report::symbols_text(&symbols))?;
        }
        Command::Describe { symbol, json } => {
            let (found, relations) = engine.relations(&symbol)?;
            if json {
                let payload = serde_json::json!({
                    "symbol": found,
                    "callers": relations.callers,
                    "callees": relations.callees,
                });
                println!("{}", serde_json::to_string_pretty(&payload)?);
            } else {
                println!(
                    "{}  {}  {}:{}",
                    found.qualified,
                    found.kind.as_str(),
                    found.file,
                    found.start_line
                );
                println!("{}", report::edges_text("callers", &relations.callers));
                println!("{}", report::edges_text("callees", &relations.callees));
            }
        }
        Command::Callers { symbol, json } => {
            let (_, edges) = engine.callers(&symbol)?;
            print_json(&edges, json, || report::edges_text("callers", &edges))?;
        }
        Command::Callees { symbol, json } => {
            let (_, edges) = engine.callees(&symbol)?;
            print_json(&edges, json, || report::edges_text("callees", &edges))?;
        }
        Command::Impact {
            symbol,
            depth,
            json,
        } => {
            let (found, rows) = engine.impact(&symbol, depth.clamp(1, 10))?;
            print_json(&rows, json, || report::impact_text(&found, &rows))?;
        }
        Command::Refs { file, json } => {
            let references = engine.store().references_in_file(&engine.relative(&file))?;
            print_json(&references, json, || {
                references
                    .iter()
                    .map(|reference| {
                        format!(
                            "{}:{}  {}  {}\n",
                            reference.file,
                            reference.line,
                            reference.kind.as_str(),
                            reference.name
                        )
                    })
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })?;
        }
        Command::Mcp { .. } | Command::Serve { .. } => unreachable!("handled before queries"),
    }
    Ok(())
}

fn print_json<T: Serialize>(
    value: &T,
    json: bool,
    text: impl FnOnce() -> String,
) -> ast_index::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        println!("{}", text());
    }
    Ok(())
}
