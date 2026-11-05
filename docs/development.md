# Development

## Implementation checkpoint

Bash/Shell, Go, Lua and PostgreSQL-oriented SQL support is implemented through
the existing CLI/MCP query engine. Language adapters live in
`src/parse/{bash,go,lua,sql}.rs`; Go package resolution is in `src/resolve/go.rs`.
Shared changes cover shebang detection, bounded and hashed `go.mod` metadata,
imports, extraction/resolution fingerprints, schema version 9 and SQL
`dependency` references separate from call edges. Astro and embedded shell/SQL
remain out of scope.

Final verification: `nix develop --command bash scripts/check fast` passed with
79 unit, 58 integration, 6 quality tests and three real sandboxed producer
tests, formatting, Clippy (`-D warnings`), TypeScript corpus typechecking, deadnix,
statix, Nix/Shell checks and both Linux output evaluations. Regressions cover
CLI/MCP parity, freshness, package boundaries, ignored Go files, shell execution
order/builtin shadowing, Lua mutation, SQL namespaces/CTEs/owners, shared read
budgets and exclusion of internal/custom/renamed SQLite databases. All added tag
queries compile against their pinned grammars and Tree-sitter 0.25.10.

Explicit Rust use bindings and JS/TS namespace-import calls now use lexical
import identity. The regression fails with the previous extractor and checks
exact targets and conservative negative cases with the new one. Status separates
link coverage by language and reference kind, including unresolved reasons;
CLI and MCP JSON agree. These counts are not semantic recall or accuracy.

Exact reference ranges re-extract legacy rows. The Rust corpus freezes 101
eligible calls plus two abstention cases; TypeScript freezes 100 plus three.
Oracle self-tests verify accounting and failure thresholds; a separate real Rust
acceptance test passes with 101 correct targets, no wrong targets and two dynamic
abstentions. TypeScript definition navigation passes all 100 labelled locations;
its separate static-dispatch acceptance remains open. Both corpora typecheck
without project dependencies. Generic-call,
constructor and commented-path regressions cover extraction and lexical scope.
Manifest inputs make configuration/lock changes exclude stale evidence. Schema 9
stores the independent static-call overlay. `--evidence combined` preserves the
AST default and fails on obsolete generation/profile or stale input. Conflict,
ambiguity, Unicode position conversion, malformed source and project-boundary
regressions pass, as does CLI/MCP parity across all supported combined actions.
The separate Rust producer uses a private snapshot and an offline
Bubblewrap process; its build-script/proc-macro CLI paths are disabled in the
pinned producer source. A real canary test verifies build scripts do not execute.
The TypeScript adapter uses the pinned native compiler LSP, with a separate tool
closure and no package manager/network access. It disables automatic type
acquisition and telemetry, requires a no-emit check, caps protocol data and checks
all input bytes before import. UTF-8/CRLF, same-line locations, config freshness,
missing dependencies and private temporary-state cleanup have real runtime tests.

Pre-publication review reproduced and fixed generic Go receiver parameters
falling back to package functions, wrapped Lua table/loader aliases retaining
static targets, and status counts omitting resolved SQL dependencies. Each
regression failed before its fix and passed in the final fast gate.

Requirements and remaining static-analysis limits are recorded in
[requirements.md](requirements.md). Local documentation links and final diffs
were checked. The patched Rust producer Nix package was built with its existing
upstream checks; the x86_64-linux semantic runner package was built and exercised
on the synthetic Rust corpus. No VM tests or host activation were performed.
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
