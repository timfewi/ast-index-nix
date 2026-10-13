# Decisions

Short records of choices that are easy to get wrong later. Newest last.

## D01 — One tool, not a catalogue

`code_explore` with an `action` parameter is the only tool. Evidence from agent
practice and existing servers (MCP client best practices recommend a stable
`call_tool` surface for prompt caching; Anthropic's tool guidance says more tools
do not necessarily produce better outcomes; a comparable indexer ships exactly
one default tool) points the same way: fewer, consolidated tools reduce
mis-picks and keep the tool list cacheable. A second tool must justify itself in
this file.

Consequence: `status` is an action, not a tool. Environment-gated extra tools are
possible but intentionally not implemented yet.

## D02 — Hand-rolled newline JSON-RPC instead of an MCP SDK

The protocol subset needed here is small: `initialize`, `ping`, `tools/list`,
`tools/call`, empty `resources`/`prompts` lists. The specification already
defines the framing for stdio and recommends the same framing for Unix sockets,
so a ~200-line dispatcher replaces a large dependency tree, keeps the Nix closure
small and lets the server answer both the stateful 2025-11-25 lifecycle and the
stateless 2026-07-28 revision without a compatibility layer.

Cost: no SDK-provided schema generation, prompts, sampling or HTTP transport. If
those are needed later, the transports in `src/serve.rs` can be replaced by
`rmcp` (`>=3.4` implements the 2026-07-28 revision) without touching the engine.

## D03 — SQLite, not a graph database

SQLite provides one local database, transactions, indexed queries and no extra
service. Bounded breadth-first traversal handles impact. A dedicated graph store
is not justified for this scope.

## D04 — No vector search in the first version

Semantic search needs an embedding runtime (model download, ONNX/candle
dependencies, large closures) and would break the local, dependency-light
property. Literal exact and prefix searches use indexes. Substring searches scan under
a computation budget; an ordinary B-tree cannot optimize their leading wildcard.
FTS5 remains deferred until a measured need justifies its schema and semantics.

## D05 — Precision-first resolution

Lexical bindings, literal Rust module paths and supported named imports provide
candidate evidence. There is no fallback from an unknown name to a same-named
symbol in another directory or project. Duplicate candidates remain unresolved.
Dynamic receiver types are not inferred. Confidence labels describe file scope,
while `reason` describes the evidence; neither asserts compiler-grade accuracy.

## D06 — Watchers are not part of correctness

Indexing is on demand. Correctness never depends on inotify: it is not
recursive, loses events on queue overflow and does not report network
filesystems. A watcher can be added later as an accelerator with polling as the
fallback, exactly as documented by prior art.

## D07 — Paths are stored relative to the root

The database is local and gitignored. Source rows use relative paths; the root
itself is recorded in `meta` for status and ownership checks. A database remains
bound to that canonical root. After moving a checkout, select a new local database.

## D08 — The socket service is optional

`ast-index mcp` runs the server in-process. The socket service exists for the
case where several harnesses should share one warm index. Database writer ownership
is acquired per mutation and is independent of listener ownership. The service
is optional because a per-session server is simpler and avoids orphaned daemons;
the proxy is a plain byte pipe so there is no second protocol implementation.

## D09 — Static Nix structure, local lexical call edges

Nix support uses the pinned tree-sitter grammar and the existing query surface.
Static bindings are definitions, syntactic lambda values are functions and
attribute sets are modules. Imports are literal syntax records; no expression
is evaluated and no import is followed.

The former file/directory/global name ladder was too broad for Nix: a function
argument, nonrecursive attribute or imported value can share a local function's
name. The parser therefore selects only directly defined functions visible in
`let` or recursive attribute scopes, and the resolver requires a unique
same-file qualified target. Parameters, `inherit`, `with`, dynamic bindings,
aliases, attribute selections and cross-file targets remain unresolved. Nix
definitions never become targets of other languages' calls. D05 now applies
precision-first resolution to all supported languages.

Full qualified chains also identify reference owners, so nested same-named
functions do not share a caller ID. Duplicate qualified owner names remain
unattached. This reuses the existing schema and symbol kinds, with no new tool,
execution authority or database migration.

## D10 — Shared bounded query contracts

CLI and MCP now call `Engine::execute` with the same typed request and JSON
payload. List responses are paged objects instead of bare arrays; the bundled
consumers and tests migrate together. Symbol selectors use file, exact name and
optional one-based line/UTF-8-byte column. Cursors bind all query arguments and a
committed generation. They are local to their configured database. SQL limits,
SQLite VM limits and traversal limits bound work before rendering.

Impact uses breadth-first ids and minimum distances, avoiding recursive-CTE
cycle duplicates. Exact/prefix search use dedicated indexes; query-plan tests
confirm substring matching scans. Literal `%` and `_` are escaped. FTS5 is not
needed for the tested queries.

## D11 — Per-operation database ownership and explicit health

A canonical database-scoped lock protects every mutation before update-state
reads. Socket locks only own listeners. Separate services can share read-only
queries while an indexer or artifact importer owns the writer lock. SQLite
transactions preserve consistent query snapshots.

Traversal failures retain missing old rows; they do not prove deletion.
Persisted health reports incomplete scans and syntax errors. Content hashes,
extraction fingerprints and resolution fingerprints serve different purposes:
byte drift, parser changes and resolver changes respectively. A successful
freshness scan is a historical observation, never an enduring freshness claim.

## D12 — Structural navigation without project execution

Import relationships resolve only against indexed paths. Nix import syntax is
never evaluated. Named JS/TS imports may interoperate; ambiguity, external
packages, runtime import paths and unsupported constructs remain visible.
Repository maps rank static graph evidence within a byte budget. This follows
[Aider's repository-map approach](https://aider.chat/docs/repomap.html) without
an LLM or embedding dependency.

Two bounded structural snapshots support added/removed/changed symbols,
relationship differences and deleted-symbol impact. Unique file/kind/qualified
names are comparison identities; duplicate identities are reported rather than
matched arbitrarily. Snapshots are local derived state, not Git history.

## D13 — Opt-in SCIP evidence, never inferred calls

The [SCIP schema](https://github.com/scip-code/scip/blob/main/scip.proto) was
inspected on 2026-10-13, including its typed ranges and explicit implementation
flag. [prost 0.14.4](https://docs.rs/prost/0.14.4/prost/) is pinned; its documented
Rust 1.85 minimum fits the existing Rust 1.88 requirement. Its derive support
handles a small declared wire subset without protoc or schema downloads. Existing
dependency pins are retained; Cargo.lock pins the added transitive dependencies.

A preflight scan bounds repeated messages before decoding. Artifacts need embedded
source text or a producer manifest binding the artifact digest to source hashes.
Hashing today's files alone cannot establish when an artifact was generated.
Every semantic query checks all participating source bytes and excludes a stale
artifact. Semantic references are not calls; implementation results require the
SCIP flag. Unsupported schema fields are ignored and documented, while unknown
protocol versions and invalid paths/ranges are rejected. Producer arguments are
not decoded or persisted.
