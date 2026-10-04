# Architecture

One library, three frontends, one database.

```
            +----------------------+        +---------------------------+
 CLI  --->  |  Engine (query API)  |  <---  |  RpcServer  (MCP dispatch)|  <--- stdio MCP
            +----------+-----------+        +-------------+-------------+
                       |                              |
                 +-----v-----+                 +------v-------+
                 |   Store    |                 | serve/socket  |  <--- Unix socket
                 |  (SQLite)  |                 +------+--------+
                 +-----^------+                        ^
                       |                               |
                 +-----+-------------------------------+----+
                 |  index: walk -> parse -> resolve -> store |
                 +------------------------------------------+
```

## Layers

- `src/lang.rs` — language registry: extension → grammar + tag query.
- `src/parse.rs` — runs the tag query, produces definitions and references,
  computes nesting (parent/qualified names) and the enclosing definition per
  reference. `src/parse/nix.rs` filters static Nix names/imports, classifies
  bindings and selects conservative lexical targets without evaluation.
- `src/index.rs` — incremental walk (`ignore`), BLAKE3 hash of each candidate
  for change detection, then a single write transaction. Unchanged content is
  not re-parsed.
- `src/resolve.rs` — name-based, precision-first scope ladder.
- `src/store.rs` — SQLite schema and queries, including a recursive CTE for
  `impact`.
- `src/engine.rs` — operations shared by CLI and MCP (outline, search, callers,
  callees, impact, status), plus symbol lookup with ambiguity reporting.
- `src/report.rs` — capped text renderers.
- `src/rpc.rs` — newline-delimited JSON-RPC dispatch (the MCP surface).
- `src/serve.rs` — stdio loop, Unix socket service with an exclusive lock, and
  the stdio↔socket proxy.

## Data model

`files(path, lang, size, mtime_ns, hash)` — one row per indexed file, paths
relative to the root. `symbols(file_id, name, qualified, kind, start_line,
end_line, parent)` — definitions. `refs(file_id, name, path, kind, line,
from_symbol_id, resolved_symbol_id, confidence)` — call sites and imports, each
with the enclosing definition and the resolution result.

`impact` is a recursive CTE over resolved edges (only `kind = 'call'`). Imports
are stored and counted but deliberately excluded from call queries.

## Resolution

Scoped paths (`Type::method`, `crate::f`) are tried against qualified names
first. A bare-name fallback is only allowed when the path head is `crate`,
`self` or `super`; external paths such as `Connection::open` are never linked to
a same-named local definition. Within each ladder the narrowest scope with
exactly one candidate wins; anything else stays unresolved. This trades recall
for precision on purpose: a wrong edge is worse for an agent than a missing one,
and the unresolved cases are visible in `status` and `refs`.

Nix uses a stricter path: the parser selects directly defined functions visible
in enclosing `let` or recursive attribute scopes, stopping at parameters,
inherited values, dynamic bindings and `with`. The resolver only accepts a unique
qualified match in the same file. Nix symbols are excluded from other languages'
directory/global name ladders. Selected calls and cross-file Nix targets stay
unresolved. Reference owners use full qualified names; duplicate owner names
stay unattached instead of selecting an arbitrary symbol.

## Storage and durability

SQLite with `foreign_keys = ON`, `synchronous = NORMAL` and a requested WAL
journal. The journal mode is recorded in `meta`: on filesystems without shared
memory (network shares, some container mounts) SQLite reports the rollback
journal instead and the tool keeps working. One process owns the index; the
socket service and `index` command take an exclusive `flock` on a lock file next
to the database. Readers see a consistent snapshot.

## Transports

The MCP framing is newline-delimited JSON-RPC 2.0, which is what the MCP
specification defines for stdio and recommends reusing for Unix domain sockets.
The dispatcher is revision-tolerant: it answers the `initialize`/`initialized`
lifecycle used up to protocol revision 2025-11-25 and also serves requests that
already carry their protocol version in `params._meta` (the stateless
2026-07-28 revision). The server keeps no session state, which makes both
revisions equivalent to it.

The socket is created with mode 0600 (owner-only) inside a private runtime
directory, so only the service user can connect. That is also what the NixOS
module uses, since service and harness run as the same user. The proxy forwards
bytes unchanged and flushes each read so responses remain interactive. Both
server transports retain at most 1 MiB per request frame and drain larger
frames before serving the next request. Tool result text is capped at 64 KiB.

## Trust boundary

Everything read from source files is untrusted data. It never becomes a path, a
command, a capability or configuration. Tool arguments are validated against a
fixed schema; unknown actions and tools return stable, service-authored errors.
The index is read-only towards the repository and makes no network requests.
