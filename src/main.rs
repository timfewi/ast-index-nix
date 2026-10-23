//! CLI transport. Query behavior and output contracts live in Engine.
use ast_index::{
    engine::Engine,
    error::Error,
    index::IndexOptions,
    query::Query,
    report,
    rpc::RpcServer,
    serve::{self, ServeOptions},
};
use clap::{Args, Parser, Subcommand};
use std::{path::PathBuf, process::ExitCode};

#[derive(Debug, Parser)]
#[command(
    name = "ast-index",
    version,
    about = "Local structural code navigation"
)]
struct Cli {
    #[arg(long, global = true, env = "AST_INDEX_ROOT")]
    root: Option<PathBuf>,
    #[arg(long, global = true, env = "AST_INDEX_DB")]
    db: Option<PathBuf>,
    #[command(flatten)]
    options: QueryOptions,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Default, Args)]
struct QueryOptions {
    #[arg(long, global = true)]
    json: bool,
    /// Select the file containing a symbol, or filter list results.
    #[arg(long, global = true)]
    file: Option<String>,
    #[arg(long, global = true)]
    subtree: Option<String>,
    #[arg(long, global = true)]
    language: Option<String>,
    #[arg(long, global = true)]
    kind: Option<String>,
    #[arg(long, global = true)]
    line: Option<u32>,
    #[arg(long, global = true)]
    column: Option<u32>,
    #[arg(long, global = true)]
    depth: Option<u32>,
    #[arg(long, global = true)]
    limit: Option<usize>,
    #[arg(long, global = true)]
    cursor: Option<String>,
    #[arg(long, global = true, value_parser = ["exact", "prefix", "contains"])]
    match_mode: Option<String>,
    #[arg(long, global = true)]
    reason: Option<String>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Build/update the index; does not execute project code.
    Index {
        #[arg(long)]
        force: bool,
        #[arg(long = "exclude")]
        exclude: Vec<String>,
    },
    Imports {
        path: Option<String>,
    },
    Importers {
        path: String,
    },
    Deps {
        path: Option<String>,
    },
    /// Rank files/symbols using the static graph, within a byte budget.
    Map {
        query: Option<String>,
        #[arg(long, default_value_t = 8192)]
        budget: usize,
    },
    /// Explicit JSON array of {file,start_line,end_line} changes.
    Changes {
        ranges: String,
        #[arg(long)]
        baseline: Option<i64>,
    },
    Diff {
        #[arg(long)]
        baseline: Option<i64>,
    },
    /// Import precomputed local SCIP data; never runs an indexer.
    ImportScip {
        artifact: PathBuf,
        #[arg(long)]
        manifest: Option<PathBuf>,
    },
    Semantic {
        query: Option<String>,
        #[arg(long,default_value="definition",value_parser=["definition","reference","implementation"])]
        relation: String,
    },
    Status {
        /// Human-friendly layout: plain-language state, dates, sorted tables.
        #[arg(long)]
        nice: bool,
    },
    /// Explicitly compare bounded current content with the indexed snapshot.
    Freshness,
    Outline {
        path: String,
    },
    Search {
        query: String,
    },
    Describe {
        query: String,
    },
    Callers {
        query: String,
    },
    Callees {
        query: String,
    },
    Impact {
        query: String,
    },
    Refs {
        path: Option<String>,
    },
    Mcp {
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    Serve {
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        lock: Option<PathBuf>,
        #[arg(long = "exclude")]
        exclude: Vec<String>,
        #[arg(long)]
        index: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.options.json && matches!(cli.command, Command::Status { nice: true }) {
        clap::Error::raw(
            clap::error::ErrorKind::ArgumentConflict,
            "the argument '--nice' cannot be used with '--json'",
        )
        .exit();
    }
    let json = cli.options.json;
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if json {
                let payload = error.payload();
                println!("{payload}");
            }
            eprintln!("error: {error}");
            ExitCode::from(if matches!(error, Error::NotIndexed(_)) {
                2
            } else {
                1
            })
        }
    }
}

fn run(cli: Cli) -> ast_index::Result<()> {
    if let Command::Mcp {
        socket: Some(socket),
    } = &cli.command
    {
        return serve::run_proxy(socket);
    }
    let root = cli
        .root
        .unwrap_or(std::env::current_dir().map_err(|e| Error::io(".", e))?);
    let engine = Engine::open(root, cli.db)?;
    match cli.command {
        Command::ImportScip { artifact, manifest } => {
            let result = ast_index::scip::import(
                engine.root(),
                engine.store(),
                &artifact,
                manifest.as_deref(),
            )?;
            println!("{}", serde_json::to_string(&result)?);
            Ok(())
        }
        Command::Index { force, exclude } => {
            let stats = engine.index(&IndexOptions {
                force,
                exclude,
                ..IndexOptions::default()
            })?;
            println!(
                "{}",
                if cli.options.json {
                    serde_json::to_string(&stats)?
                } else {
                    report::index_text(&stats)
                }
            );
            Ok(())
        }
        Command::Serve {
            socket,
            lock,
            exclude,
            index,
        } => {
            if index {
                eprintln!(
                    "{}",
                    report::index_text(&engine.index(&IndexOptions {
                        exclude,
                        ..IndexOptions::default()
                    })?)
                );
            }
            serve::run_socket(RpcServer::new(engine), &ServeOptions { socket, lock })
        }
        Command::Mcp { .. } => serve::run_stdio(&RpcServer::new(engine)),
        command => {
            let o = cli.options;
            let mut request = Query {
                file: o.file,
                subtree: o.subtree,
                language: o.language,
                kind: o.kind,
                line: o.line,
                column: o.column,
                depth: o.depth,
                limit: o.limit,
                cursor: o.cursor,
                match_mode: o.match_mode,
                reason: o.reason,
                ..Query::default()
            };
            let mut nice = false;
            let (action, query) = match command {
                Command::Imports { path } => {
                    if path.is_some() {
                        request.file = path;
                    }
                    ("imports", None)
                }
                Command::Importers { path } => {
                    request.file = Some(path);
                    ("importers", None)
                }
                Command::Deps { path } => {
                    if path.is_some() {
                        request.file = path;
                    }
                    ("deps", None)
                }
                Command::Map { query, budget } => {
                    request.budget = Some(budget);
                    ("map", query)
                }
                Command::Changes { ranges, baseline } => {
                    request.changes = serde_json::from_str(&ranges)?;
                    request.baseline = baseline;
                    ("changes", None)
                }
                Command::Diff { baseline } => {
                    request.baseline = baseline;
                    ("diff", None)
                }
                Command::Semantic { query, relation } => {
                    request.relation = Some(relation);
                    ("semantic", query)
                }
                Command::Status { nice: human } => {
                    nice = human;
                    ("status", None)
                }
                Command::Freshness => ("freshness", None),
                Command::Outline { path } => {
                    request.file = Some(path);
                    ("outline", None)
                }
                Command::Refs { path } => {
                    if path.is_some() {
                        request.file = path;
                    }
                    ("refs", None)
                }
                Command::Search { query } => ("search", Some(query)),
                Command::Describe { query } => ("describe", Some(query)),
                Command::Callers { query } => ("callers", Some(query)),
                Command::Callees { query } => ("callees", Some(query)),
                Command::Impact { query } => ("impact", Some(query)),
                _ => unreachable!("handled transport/index command"),
            };
            request.action = action.into();
            request.query = query;
            let value = engine.execute(&request)?;
            let text = if o.json {
                serde_json::to_string(&value)?
            } else if nice {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |elapsed| elapsed.as_secs() as i64);
                report::status_nice(&serde_json::from_value(value)?, now)
            } else {
                report::query_text(&value)
            };
            println!("{text}");
            Ok(())
        }
    }
}
