# ast-index-nix
Format: tabula/1. Source labels are repository-relative; links resolve from this card.
Purpose: bounded local structural navigation with SQLite, one query engine and
CLI, stdio MCP or Unix-socket frontends. No project execution or runtime network.

## Ownership and flow
- [src/lang.rs](../src/lang.rs), [src/parse.rs](../src/parse.rs), [src/parse/scopes.rs](../src/parse/scopes.rs), [src/parse/nix.rs](../src/parse/nix.rs): cached extraction, lexical bindings, signatures, positions and Nix syntax.
- [src/parse/bash.rs](../src/parse/bash.rs), [src/parse/go.rs](../src/parse/go.rs), [src/parse/lua.rs](../src/parse/lua.rs), [src/parse/sql.rs](../src/parse/sql.rs): bounded shell commands, Go declarations, Lua bindings and PostgreSQL objects.
- [src/index.rs](../src/index.rs): bounded walk, content/fingerprint cache, project boundaries, diagnostics, freshness and coordinated writes.
- [src/imports.rs](../src/imports.rs), [src/resolve.rs](../src/resolve.rs), [src/resolve/go.rs](../src/resolve/go.rs): static indexed-file dependencies, Go package lookup and conservative call/SQL object relationships.
- [src/store.rs](../src/store.rs), [src/store/query.rs](../src/store/query.rs), [src/store/history.rs](../src/store/history.rs): additive schema, writer lock, SQL projections, bounded traversal and two structural snapshots.
- [src/query.rs](../src/query.rs), [src/engine.rs](../src/engine.rs), [src/report.rs](../src/report.rs): shared contracts, validation, cursors and text.
- [src/scip.rs](../src/scip.rs), [src/scip/wire.rs](../src/scip/wire.rs): optional local SCIP import with independent provenance and source verification.
- [src/producer.rs](../src/producer.rs), [src/bin/ast-index-semantic.rs](../src/bin/ast-index-semantic.rs), [flake.nix](../flake.nix): explicit private Rust snapshot runner, offline Bubblewrap/tool closure and patched SCIP producer; no invocation from core queries.
- [src/main.rs](../src/main.rs), [src/rpc.rs](../src/rpc.rs), [src/serve.rs](../src/serve.rs): CLI, single-tool MCP and socket/proxy adapters.

## Interfaces and limits
`code_explore` actions: status, freshness, search, outline, describe, callers,
callees, impact, refs, imports, importers, deps, map, changes, diff, semantic.
CLI also owns explicit `index` and `import-scip` mutations. Schema 8 persists
zero-based half-open UTF-8 reference ranges; legacy ranges stay null until
reindexing. Manifest inputs include configuration/lock hashes. Rust generic-call
wrappers/path trivia and JS/TS constructor expressions retain call positions.
Query file/name and
optional line/column select symbols. Lists use generation-bound pages.
Languages: Rust, Python, TypeScript/TSX, JavaScript, Nix, Bash/Shell, Go, Lua and
PostgreSQL SQL. No unrelated bare-name fallback; Go has explicit package scope.
JS/TS named and namespace imports can interoperate; explicit Rust use bindings
expand crate/self/super paths with aliases and grouped imports. Shadowing and
reassignment block targets; globs and inferred receivers remain unsupported.
Nix/Lua calls remain local. Shell source
paths retain unresolved working directories. Go module hashes govern freshness;
Lua requires use project-root layouts. SQL `dependency` refs are separate from
calls/impact and file imports; qualified objects stay within one project and
unqualified objects within one file. Dynamic targets remain unresolved. Astro
and embedded shell/SQL are excluded. `.ast-index`, the active database/sidecars
and `.sql` files with SQLite headers are excluded from source input. Source ranges distinguish
same-named reference owners; ambiguous targets remain unresolved.

Status reports link counts and unresolved reasons by language/reference kind;
imports link to files, calls/dependencies to definitions, not an accuracy score.
Freshness requires an explicit bounded byte
scan. A successful old check does not prove present freshness. Semantic evidence
requires verified embedded source or a producer manifest, and never becomes an
AST call edge. Sources and all returned code content remain untrusted data.

## Change and verify
- [docs/semantic-quality.md](../docs/semantic-quality.md): private semantic adapter/overlay plan; explicit Rust runner implemented, semantic calls and producer recall unverified.
- [Oracle](../tests/quality.rs)/[scorer](../tests/quality/evaluator.rs)/[Rust calls](../tests/quality/fixtures/rust/src/lib.rs)/[targets](../tests/quality/fixtures/rust/src/ops.rs)/[TypeScript calls](../tests/quality/fixtures/typescript/src/main.ts)/[targets](../tests/quality/fixtures/typescript/src/ops.ts): independent synthetic labels; scorer self-tests are not a passed 90% semantic gate.
- [Producer smoke](../tests/producer.rs): real offline SCIP import, build-script canary, temporary artifact cleanup and lockfile invalidation; missing dependencies/config support remain explicit gaps.
- [docs/requirements.md](../docs/requirements.md): scope and evidence.
- [docs/architecture.md](../docs/architecture.md), [docs/decisions.md](../docs/decisions.md): contracts and rationale.
- [docs/development.md](../docs/development.md): current checkpoint and workflow.
- [tests/integration.rs](../tests/integration.rs): CLI/MCP/socket behavior and failure regressions; parser, resolver, SCIP and query plans also have unit tests.
- Fast gate: `nix develop --command bash scripts/check fast`. No host activation, package builds or external publication is implied.
