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

Every comparable lightweight indexer stores into SQLite and answers graph
questions with recursive CTEs; the embedded graph database in the same niche was
archived. SQLite gives one file, transactions, a stable query language and no
extra service. A dedicated graph store is not justified at this size.

## D04 — No vector search in the first version

Semantic search needs an embedding runtime (model download, ONNX/candle
dependencies, large closures) and would break the local, dependency-light
property. Name search with `LIKE` over an indexed column is enough for the
questions this tool answers. FTS5 is the next cheap step if ranking quality
becomes the bottleneck; it is deliberately not in the schema yet because its
availability depends on how SQLite was compiled.

## D05 — Precision-first resolution

Ambiguous names stay unresolved. The alternative (pick the first candidate)
produces confident-looking wrong edges, which is worse than no edge for an agent
that trusts the answer. Scoped paths are matched qualified-first, and the
bare-name fallback is restricted to relative paths (`crate`, `self`, `super`) so
external calls cannot collapse onto local same-named definitions.

Known limitation: calls through a value (`store.callers()`) only see the method
name, so a unique same-named method in the same directory can be linked. This is
documented in the README and visible in the `confidence` field.

## D06 — Watchers are not part of correctness

Indexing is on demand. Correctness never depends on inotify: it is not
recursive, loses events on queue overflow and does not report network
filesystems. A watcher can be added later as an accelerator with polling as the
fallback, exactly as documented by prior art.

## D07 — Paths are stored relative to the root

The database is local and gitignored, but relative paths keep it portable, keep
absolute host paths out of query output and make the index independent of where
the checkout lives. The root itself is recorded in `meta` for `status`.

## D08 — The socket service is optional

`ast-index mcp` runs the server in-process. The socket service exists for the
case where several harnesses should share one warm index and one writer lock. It
is optional because a per-session server is simpler and avoids orphaned daemons;
the proxy is a plain byte pipe so there is no second protocol implementation.
