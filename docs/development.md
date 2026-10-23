# Development

## Implementation checkpoint

Bash/Shell, Go, Lua and PostgreSQL-oriented SQL support is implemented through
the existing CLI/MCP query engine. Language adapters live in
`src/parse/{bash,go,lua,sql}.rs`; Go package resolution is in `src/resolve/go.rs`.
Shared changes cover shebang detection, bounded and hashed `go.mod` metadata,
imports, extraction/resolution fingerprints, schema version 6 and SQL
`dependency` references separate from call edges. Astro and embedded shell/SQL
remain out of scope.

Final verification: `nix develop --command bash scripts/check fast` passed with
66 unit and 47 integration tests, formatting, Clippy (`-D warnings`), deadnix,
statix, Nix/Shell checks and both Linux output evaluations. Regressions cover
CLI/MCP parity, freshness, package boundaries, ignored Go files, shell execution
order/builtin shadowing, Lua mutation, SQL namespaces/CTEs/owners, shared read
budgets and exclusion of internal/custom/renamed SQLite databases. All added tag
queries compile against their pinned grammars and Tree-sitter 0.25.10.

Pre-publication review reproduced and fixed generic Go receiver parameters
falling back to package functions, wrapped Lua table/loader aliases retaining
static targets, and status counts omitting resolved SQL dependencies. Each
regression failed before its fix and passed in the final fast gate.

Requirements and remaining static-analysis limits are recorded in
[requirements.md](requirements.md). Local documentation links and final diffs
were checked. No Nix package builds, VM tests or host activation were performed.
Module evaluation fixtures explicitly set `system.stateVersion` to avoid implicit defaults.

## Environment

The pinned Nix flake provides the toolchain. Cargo artifacts must stay inside the
repository because `/tmp` is small on this host.

```bash
nix develop
```

## Commands

```bash
cargo fetch                     # once per machine, enables --offline checks
cargo build
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all
bash scripts/check fast
```

`scripts/check fast` is the documented gate: formatting, clippy with
`-D warnings`, all unit and integration tests, deadnix/statix, `nixfmt --check`,
`shellcheck`, and both Linux output evaluations with `nix flake check --no-build`.

## Layout

See [.tabula/ENTRY.md](../.tabula/ENTRY.md) for current source ownership.

## Manual checks

```bash
# Dogfood the repository itself.
cargo run -- index
cargo run -- status
cargo run -- describe ScopeMaps::build
cargo run -- impact resolve_all --depth 3

# Serve on a socket and talk to it through the proxy.
cargo run -- serve --socket /tmp/ast-index.sock --index
# In a second terminal:
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' \
  | cargo run -- mcp --socket /tmp/ast-index.sock
```

## Adding a language

1. Add the grammar crate to `Cargo.toml`.
2. Add a `LangSpec` entry and a tag query in `src/lang.rs`.
3. Run the tests: `every_tag_query_compiles_for_its_grammar` fails on a bad query
   and the language-specific test shows what is extracted.
4. Update the requirements table, README and `.tabula/ENTRY.md` coverage, and add
   a fixture assertion if the language has a distinctive construct.

## Index state

The index is `<root>/.ast-index/index.sqlite` plus its journal files. `ast-index
index` rebuilds AST data; `--force` re-parses everything. Recreating the database
also discards retained snapshots and requires re-importing semantic artifacts.
Database locks cover mutations and are released on exit. Listener locks are
separate; query services do not monopolize the writer. Schema migration preserves
derived rows and fingerprints invalidate stale extraction. Stored SCIP data and
structural snapshots are also derived state.
