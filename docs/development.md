# Development

## Implementation checkpoint

The five-stage navigation expansion is implemented and verified. Changed layers:
extraction/scopes; indexing/health/fingerprints; database locks, SQL queries and
snapshots; shared CLI/MCP contracts; imports, repository maps, changed-impact and
local SCIP ingestion. Source/state boundaries remain local.

Final verification: `nix develop --command bash scripts/check fast` passed with
29 unit and 35 integration tests, formatting, Clippy (`-D warnings`), deadnix,
statix, Nix/Shell checks and both Linux output evaluations. Process tests exercise
CLI, stdio MCP, separate socket readers, writer contention and proxy framing.
Exact/prefix SQL plans use indexes; substring matching scans under a tested VM
budget. Migration, stale semantic artifacts, special files, root ownership,
cache invalidation and deleted-symbol callers have focused regressions.

The final requirements audit and local documentation-link checks passed. Remaining
coverage limits are recorded in [requirements.md](requirements.md#evidence-limits).
Review regressions cover hoisted/reassigned bindings, exception aliases, named
exports, member/module boundaries, callee filters and SCIP range-validation work.
No implementation step remains pending. Nix package builds and VM tests were not
run; no host activation occurred.
The module evaluations retain the existing unset `system.stateVersion` warning.

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
