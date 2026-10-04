# ast-index-nix
Format: tabula/1. Source labels are repository-relative; links resolve from this card.
Purpose: local structural code navigation through one library, SQLite, and
CLI, stdio MCP or Unix-socket frontends. No network or model dependency.

## Ownership and flow
- [src/main.rs](../src/main.rs): CLI argument parsing and command dispatch.
- [src/lang.rs](../src/lang.rs): supported language registry and tree-sitter tag queries.
- [src/index.rs](../src/index.rs) + [src/parse.rs](../src/parse.rs): enumerate source, parse definitions, references
  and imports; reuse unchanged content and remove deleted files.
- [src/parse/nix.rs](../src/parse/nix.rs): static Nix names/imports, binding kinds
  and conservative same-file lexical call targets; no evaluation or import following.
- [src/resolve.rs](../src/resolve.rs): resolve call edges by name and scope. Ambiguous matches stay
  unresolved; an external qualified path must not fall back to a local name.
- [src/store.rs](../src/store.rs): SQLite persistence under .ast-index/index.sqlite.
- [src/engine.rs](../src/engine.rs): shared query API; [src/report.rs](../src/report.rs) renders bounded results.
- [src/rpc.rs](../src/rpc.rs): JSON-RPC/MCP dispatch for code_explore.
- [src/serve.rs](../src/serve.rs): socket server and proxy; one writer owns the index.
Flow: files -> parser -> stored symbols/references -> resolver -> engine ->
CLI or MCP response. Source content never supplies execution authority.

## Interfaces and coverage
CLI: status, search, outline, describe, callers, callees, impact, refs.
MCP: one code_explore tool with status, search, outline, describe, callers,
callees and impact actions. CLI refs is not a separate MCP action.
Index refresh is explicit: `ast-index index` or the workspace index command.
Query roots and databases can be selected with --root and --db.

Languages: Rust, Python, TypeScript/TSX, JavaScript and Nix.
Nix indexes static bindings and inherited attributes, named lambdas, nested
attribute sets and literal imports. Call edges require unique, visible same-file
functions in `let` or recursive attribute scopes. Selected, dynamic, inherited
and cross-file targets stay unresolved; use focused source reads for relationships
beyond that coverage.
Edges are navigation hints: dynamic dispatch, reflection, macros, generated
code, re-exports and language-specific indirection are not fully resolved.
Confirm consequential findings in current source, especially after edits.

## Change and verify
- Grammar or tags: [src/lang.rs](../src/lang.rs) and parser tests.
- Resolution rules: [src/resolve.rs](../src/resolve.rs) and its ambiguity/scoped-name cases.
- Storage or freshness: [src/store.rs](../src/store.rs), [src/index.rs](../src/index.rs), [tests/integration.rs](../tests/integration.rs).
- Protocol or transport: [src/rpc.rs](../src/rpc.rs), [src/serve.rs](../src/serve.rs), [tests/integration.rs](../tests/integration.rs).
- Fast gate: `bash scripts/check fast` using [scripts/check](../scripts/check) from the pinned development shell;
  [.project-checks.json](../.project-checks.json) exposes it through project-check fast.
- Nix integration is exported by [flake.nix](../flake.nix); consuming hosts select it.

## Boundaries
An absent index needs setup; it does not prove absence of a symbol.
Status reports coverage, not a semantic proof that every current file is fresh.
Index data is derived and local. Never commit it or automatically rebuild it
just to answer a query. Preserve bounded output and explicit unresolved results.
Retain a single implementation behind the frontends; migrate known callers
when changing an internal interface. Update this card with architecture changes.
