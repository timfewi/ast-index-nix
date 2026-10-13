# Architecture

One library owns indexing and querying; CLI, stdio MCP and Unix-socket MCP use
it. `code_explore` remains the only MCP tool. Runtime code performs no network
requests, project execution, package discovery commands or Nix evaluation.

## Ownership

- `lang.rs`, `parse.rs`, `parse/scopes.rs`, `parse/nix.rs`: grammar registry,
  cached parsers/tag queries, bounded extraction, lexical bindings and Nix rules.
- `index.rs`: bounded ignored-file walk, byte hashing, extraction fingerprints,
  project boundaries, persisted health and explicit freshness scans.
- `imports.rs`, `resolve.rs`: literal indexed-file dependencies and conservative
  call resolution. These layers never open import targets.
- `store.rs`, `store/query.rs`, `store/history.rs`: additive schema migration,
  database writer lock, bounded SQL projections, graph traversal and two retained
  structural snapshots.
- `query.rs`, `engine.rs`, `report.rs`: shared request/response contracts,
  validation, snapshot-consistent queries and compact text output.
- `scip.rs`, `scip/wire.rs`: bounded, opt-in local semantic-artifact ingestion;
  semantic evidence remains separate from AST call edges.
- `main.rs`, `rpc.rs`, `serve.rs`: transport adaptation, protocol negotiation,
  bounded framing, socket ownership and byte-preserving proxy.

## State and coordination

Files record relative paths, language, project boundary, byte hash, extraction
fingerprint and parse-error coverage. Symbols record names, kinds, line/column
ranges, bounded signatures and content hashes. References record owners, lexical
blocking reasons, resolved targets and resolution provenance. Literal file import
targets are separate from symbol call targets.

Schema version 5 upgrades older derived state additively. Extraction fingerprint
changes force reparsing even if file bytes have not changed. Existing incomplete
snapshots that lack position information are marked unsuitable for comparison.
Unknown newer schemas are rejected. An existing database is bound to its indexed
root; selecting another root requires a separate database.

Each mutation acquires an exclusive nonblocking lock beside the canonical database
before reading update state. SQLite commits indexing atomically. Schema migration,
indexing, explicit freshness metadata updates and SCIP ingestion use the writer
lock. Socket ownership has its own lock: multiple query services may share a
database through different sockets. Readers do not hold the writer lock and use
consistent SQLite transactions. WAL is requested; SQLite errors remain visible.

## Resolution and confidence

Bare names must have supported lexical bindings. Parameters, assignments and
unsupported imports block fallback. JS/TS reassignment invalidates the affected
lexical binding, and `var` uses function scope. Rust module scopes and class/impl
member namespaces do not provide implicit bare-name access. Dynamic receivers remain unresolved; explicit
Rust `self` method calls inside an impl can use its structural owner. Rust literal
module paths use file/module layout within the nearest project boundary. Named
Python imports and JavaScript/TypeScript imports can resolve to indexed exported
symbols. Re-exports, runtime loaders, build-system module remapping and inferred
receiver types are not implemented.

Nearest `Cargo.toml`, `package.json`, `pyproject.toml`, `setup.py` or `.git` markers
partition projects without executing or interpreting those files. A boundary
change invalidates resolution even when source bytes are unchanged. Nix retains
its stricter same-file lexical resolution; selected and cross-file calls remain
unresolved. Same-named ambiguous bindings are not selected arbitrarily.

Confidence strings (`same_file`, `same_directory`, `same_project`, `none`) describe
location, not type-checked certainty. `reason` records evidence or an unresolved
reason. Symbol lookup is case-sensitive and exact; search explicitly offers
case-normalized literal exact, prefix and substring matching.

## Queries and budgets

All list queries return bounded pages with `items`, `generation`, `truncated` and
`next_cursor`. Cursors bind query arguments and the committed generation; changing
an index or query invalidates them. SQL limits precede row materialization.
Impact uses breadth-first traversal of ids, deduplicates cycles, excludes the seed,
and materializes only a requested page of symbols. SQLite VM work and graph work
have separate caps. Exact and prefix search have query-plan regression checks;
substring search remains a bounded scan.

Status describes an indexed snapshot, never perpetual freshness. Explicit
freshness reads compare current bytes, including same-size/same-mtime edits.
Traversal errors retain absent indexed paths instead of asserting deletion.
Known failed reads and parse errors produce incomplete coverage. Historical
successful freshness checks do not make later status calls claim current freshness.

Repository maps rank bounded signatures using static graph counts and optional
focus. Ranking is explicitly heuristic and byte-budgeted. Structural snapshots
retain at most two generations with size caps. Comparisons match unique
file/kind/qualified identities and report ambiguous identities. Changed ranges
query both current and retained baseline callers, including removed symbols;
missing baselines and unsupported ranges are explicit.

## Semantic artifacts and trust

SCIP decoding is optional. A preflight wire scan caps repeated-message allocation;
a separate work budget bounds repeated Unicode range validation on long lines.
Only protocol version 0, UTF-8 source and explicit supported position encodings
are accepted. Paths must be canonical relative paths to already indexed regular
files; symlink components and root escapes are rejected. The artifact's root URI
is validated as local metadata and is never followed.

Embedded text or an artifact-bound producer manifest must establish source hashes.
All participating source bytes are rechecked on semantic queries; a changed or
missing document excludes the artifact's results. Producer names, versions and
artifact digests remain attached to semantic evidence. Arguments and documentation
from producer metadata are not persisted. References never become inferred call
edges, and implementation relationships require the explicit SCIP flag.

These checks constrain the service's behavior, not other programs running under
the same Unix account. File mutations racing a scan can still make its observations
immediately old. Consequential navigation must be confirmed in current source.
