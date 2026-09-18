# Development

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
`-D warnings`, all unit and integration tests, `nixfmt --check` and
`shellcheck`.

## Layout

```
src/lang.rs      language registry: extension -> grammar + tag query
src/parse.rs     tag query execution, nesting, enclosing definition
src/index.rs     walk, prefilter, hash, transaction
src/resolve.rs   precision-first scope ladder
src/store.rs     SQLite schema and queries
src/engine.rs    shared query operations
src/report.rs    capped text renderers
src/rpc.rs       JSON-RPC / MCP dispatch
src/serve.rs     stdio, socket service, proxy
src/main.rs      CLI
tests/           end-to-end tests through the real binary
nix/module.nix   NixOS service module
```

## Manual checks

```bash
# Dogfood the repository itself.
cargo run -- index
cargo run -- status
cargo run -- describe ScopeMaps::build
cargo run -- impact resolve_all --depth 3

# Serve on a socket and talk to it through the proxy.
cargo run -- serve --socket /tmp/ast-index.sock --index &
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' \
  | cargo run -- mcp --socket /tmp/ast-index.sock
```

## Adding a language

1. Add the grammar crate to `Cargo.toml`.
2. Add a `LangSpec` entry and a tag query in `src/lang.rs`.
3. Run the tests: `every_tag_query_compiles_for_its_grammar` fails on a bad query
   and the language-specific test shows what is extracted.
4. Update the requirements table and README coverage, and add a fixture assertion
   if the language has a distinctive construct.

## Index state

The index is `<root>/.ast-index/index.sqlite` plus its journal files. Deleting
the directory is always safe; `ast-index index` rebuilds it. `--force` re-parses
everything. A stale lock file is only a problem while a service holds it; the
lock is released when the holder exits.
