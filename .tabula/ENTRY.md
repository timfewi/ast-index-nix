# ast-index-nix
Format: tabula/1. Source labels are repository-relative; links resolve from this card.
Purpose: bounded local structural navigation with SQLite, one query engine and
CLI, stdio MCP or Unix-socket frontends. No project execution or runtime network.

## Ownership and flow
- [src/lang.rs](../src/lang.rs), [src/parse.rs](../src/parse.rs),
  [src/parse/scopes.rs](../src/parse/scopes.rs), [src/parse/nix.rs](../src/parse/nix.rs):
  cached extraction, lexical bindings, signatures, positions and Nix syntax.
- [src/index.rs](../src/index.rs): bounded walk, content/fingerprint cache,
  project boundaries, diagnostics, freshness and coordinated writes.
- [src/imports.rs](../src/imports.rs), [src/resolve.rs](../src/resolve.rs):
  static indexed-file dependencies and conservative call relationships.
- [src/store.rs](../src/store.rs), [src/store/query.rs](../src/store/query.rs),
  [src/store/history.rs](../src/store/history.rs): additive schema, writer lock,
  SQL projections, bounded traversal and two structural snapshots.
- [src/query.rs](../src/query.rs), [src/engine.rs](../src/engine.rs),
  [src/report.rs](../src/report.rs): shared contracts, validation, cursors and text.
- [src/scip.rs](../src/scip.rs), [src/scip/wire.rs](../src/scip/wire.rs): optional
  local SCIP import with independent provenance and source verification.
- [src/main.rs](../src/main.rs), [src/rpc.rs](../src/rpc.rs),
  [src/serve.rs](../src/serve.rs): CLI, single-tool MCP and socket/proxy adapters.

## Interfaces and limits
`code_explore` actions: status, freshness, search, outline, describe, callers,
callees, impact, refs, imports, importers, deps, map, changes, diff, semantic.
CLI also owns explicit `index` and `import-scip` mutations. Query file/name and
optional line/column select symbols. Lists use generation-bound pages.
Languages: Rust, Python, TypeScript/TSX, JavaScript and Nix. No bare-name fallback
to other files. JS/TS named imports can interoperate. Unsupported lexical,
dynamic, imported and external targets remain unresolved. Nix calls remain local.

Status reports snapshot coverage; freshness requires an explicit bounded byte
scan. A successful old check does not prove present freshness. Semantic evidence
requires verified embedded source or a producer manifest, and never becomes an
AST call edge. Sources and all returned code content remain untrusted data.

## Change and verify
- [docs/requirements.md](../docs/requirements.md): scope and evidence.
- [docs/architecture.md](../docs/architecture.md),
  [docs/decisions.md](../docs/decisions.md): contracts and rationale.
- [docs/development.md](../docs/development.md): current checkpoint and workflow.
- [tests/integration.rs](../tests/integration.rs): CLI/MCP/socket behavior and
  failure regressions; parser, resolver, SCIP and query plans also have unit tests.
- Fast gate: `nix develop --command bash scripts/check fast`. No host activation,
  package builds or external publication is implied.
