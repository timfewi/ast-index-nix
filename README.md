# ast-index

A lightweight, local-first AST code index for agent harnesses. It extracts
definitions, call sites, imports and SQL object dependencies with tree-sitter,
stores them in one SQLite file and exposes the result through three frontends that share one library:

- `ast-index` — a CLI with `--json` output for scripts and non-MCP harnesses,
- `ast-index mcp` — a stdio MCP adapter with **exactly one tool** (`code_explore`),
- `ast-index serve --socket <path>` plus `ast-index mcp --socket <path>` — a
  persistent Unix socket service and a byte-level proxy as the only harness-facing
  process.

For shared use, an optional socket service keeps one warm index available to
multiple harnesses through a tiny proxy. In-process CLI and stdio use remain available.
There is no telemetry, no network access and no LLM dependency.

The Nix package, development shell, formatter and module checks export
`x86_64-linux` and `aarch64-linux`. The service module selects the native package
for the consuming host.

## Quick start

```bash
nix develop --command cargo build --release

# Build or update the index for the current repository.
target/release/ast-index index

# Keep a subtree out of the index (gitignore-style, repeatable).
target/release/ast-index index --exclude secrets/

# Ask questions.
ast-index status
ast-index status --nice   # human-readable: dates, grouped counts, sorted tables
ast-index search parse
ast-index outline src/parse.rs
ast-index describe ScopeMaps::build
ast-index callers resolve_all
ast-index impact resolve_all --depth 3
```

The human-readable `status --nice` format cannot be combined with `--json`.

By default the index lives in `<root>/.ast-index/index.sqlite` and is gitignored.
Use global `--state-dir <directory>` (or `AST_INDEX_STATE_DIR`) for a dedicated
state directory, independent of the source root:

```bash
ast-index --root /srv/project --state-dir /var/lib/ast-index/instance-a index
ast-index --root /srv/project --state-dir /var/lib/ast-index/instance-a search parse
ast-index --root /srv/project --state-dir /var/lib/ast-index/instance-b index
```

Each directory contains its own `index.sqlite`, retained snapshots, semantic
imports, freshness metadata, SQLite sidecars and `index.writer.lock`. Every
in-process CLI operation, stdio MCP and `serve --index` honors the selection;
use the same arguments for later refreshes and queries. Relative state paths
are relative to the process working directory, not `--root`. `--state-dir`
conflicts with the existing `--db` / `AST_INDEX_DB` file override. Empty,
unusable or symlinked state directories/databases fail rather than falling
back to shared state. Parent directories must be trusted.

The source walker always excludes `.ast-index/`, the entire explicitly selected
state directory, the active database and its sidecars, independently of ignore
files. SQL support reads `.sql` source files, never embedded SQL strings; SQLite
headers are rejected even under a `.sql` filename.

## MCP integration

All harnesses use the same server. Only the configuration file differs.

OpenCode 2 (`opencode.json` / `opencode.jsonc`):

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "servers": {
      "ast_index": {
        "type": "local",
        "command": ["ast-index", "mcp"],
        "disabled": false,
        "codemode": false
      }
    }
  }
}
```

`codemode: false` keeps the single tool visible when OpenCode 2 runs in Code
Mode. For the socket service, point the command at the proxy instead:
`["ast-index", "mcp", "--socket", "/run/ast-index/socket"]`.

Codex CLI (`~/.codex/config.toml` or project `.codex/config.toml`):

```toml
[mcp_servers.ast_index]
command = "ast-index"
args = ["mcp"]
```

Claude Code (`.mcp.json`):

```json
{
  "mcpServers": {
    "ast_index": { "command": "ast-index", "args": ["mcp"] }
  }
}
```

The server returns setup guidance instead of an error when a repository is not
indexed yet, so an agent does not conclude that the tool is broken. Indexing is
explicit: run `ast-index index` once (or `serve --index`).

## Queries

CLI and the single MCP tool `code_explore` share these actions:

| Action | Purpose |
| --- | --- |
| `status` / `freshness` | Snapshot coverage / explicitly compare current bytes |
| `search` / `outline` | Literal name search / definitions in a file |
| `describe` | One selected symbol and both edge directions |
| `callers` / `callees` / `impact` | Incoming, outgoing and unique transitive callers |
| `refs` | References with resolution evidence or unresolved reasons |
| `imports` / `importers` / `deps` | Import records, reverse importers and resolved file dependencies |
| `map` | Heuristically ranked signatures within a byte budget |
| `changes` / `diff` | Explicit changed-range impact / retained snapshot differences |
| `semantic` | Separately identified SCIP definitions, references or implementations |

```bash
ast-index describe helper --file src/lib.rs --line 12
ast-index describe same --file src/main.js --line 1 --column 20
ast-index search parse --subtree src --language rust --kind function --match-mode prefix
ast-index refs src/main.py --reason shadowed
ast-index refs schema.sql --kind dependency --json
ast-index imports flake.nix
ast-index importers modules/service.nix
ast-index map resolve_all --budget 8192
ast-index freshness --json
ast-index changes '[{"file":"src/lib.rs","start_line":10,"end_line":20}]' --json
ast-index diff --json
```

File and subtree paths are relative to the indexed root. Symbol selection is
exact and case-sensitive; ambiguous responses contain candidate locations.
Lines and AST columns are one-based; columns count UTF-8 bytes. `search` defaults
to case-normalized literal substring matching, with `exact` and `prefix` modes;
`%` and `_` are literal characters, not user wildcards.

`--json` and MCP `format: "json"` return the same payloads. Lists now return
`{items, generation, truncated, next_cursor}`, replacing the old bare arrays.
Use `--limit` (1–100, default 25) and repeat the same query with `--cursor` to
continue. Cursors belong to that database and query and expire after a committed
index generation changes. Text output also reports continuation information.
`describe` contains `symbol`, `callers` and `callees`; its two nested pages can be
continued independently using their respective cursors with the same request.
Row ids are snapshot-local, not permanent symbol identifiers.

File/subtree/language/kind filters apply to structural lists. For graph queries,
file and position select the seed; subtree/language/kind restrict returned rows.
Reference `kind` is `call`, `import` or `dependency`. SQL object dependencies
appear through `refs --kind dependency`; they do not become call/impact edges or
file imports in `deps`. Snapshot diffs support file/subtree;
changed-impact accepts explicit ranges rather than symbol filters. Semantic
queries support an exact file filter and an optional symbol or display name.

## Coverage and freshness

`refs` includes `range: [start_line, start_column, end_line, end_column]` for
the referenced syntax. Ranges are zero-based, half-open and use UTF-8 byte
columns; the existing `line` field stays one-based. Legacy database rows return
`range: null` until reindexing, never a position guessed from a line number.
Rust calls with explicit type arguments are included, and path comments do not
change their literal module identity.

`status` reports `reference_coverage` separately by language and reference kind,
including linked counts and unresolved reasons. `status --nice` displays these
rates and the three largest unresolved categories per row. Imports link to
indexed files, while calls and dependencies link to definitions; combining
their denominators hides this distinction. Link coverage is not semantic
accuracy or recall. Dynamic/external cases are not removed to inflate a score.
Use `refs --language <language> --kind call --reason <reason>` to inspect a gap.

Explicit Rust `use crate::...`, `use self::...` and `use super::...` bindings
support grouped imports and aliases. JS/TS namespace imports such as
`import * as helpers from './helpers'` resolve direct calls to named exports.
Shadowing, reassignment, type-only imports, globs and unknown receivers never
gain targets through a bare-name fallback.

Status reports index age, generation, failures, skipped files, parse-error
coverage, resolution reasons and indexed/current extraction and resolution
versions. Ordinary status says `unknown` for present freshness unless there is
known stale or incomplete evidence. A `freshness` scan hashes source bytes,
including same-size/same-mtime edits, and reports `fresh_at_check`, `stale` or
`incomplete`. A past successful scan never makes a later status claim perpetual
freshness. Indexing and freshness store derived metadata, never source edits.

Failed directory scans retain absent old rows. Parse errors and retained failed
files remain incomplete coverage. Parser/query changes invalidate cached
extraction; resolver changes or project-boundary changes invalidate call edges.
An unchanged pass reuses resolution. No watcher is required for correctness.

Definitions cover Rust, Python, TypeScript/TSX, JavaScript, Nix, Bash/Shell, Go,
Lua and PostgreSQL SQL. Call targets need supported lexical bindings, literal
module/package paths or supported named imports. JS/TS named imports can interoperate across indexed files with direct named exports;
default exports and class members are not treated as named module exports. There is no
fallback to an unrelated same-named symbol in another file or project. Parameters,
assignments, ambiguous names, inferred receiver types, re-exports and unsupported
indirection remain unresolved. Confidence labels describe scope (`same_file`,
`same_directory`, `same_project`, `none`); `reason` describes evidence. They are
navigation hints, not compiler proofs.

Nix static bindings, nested attributes, inherited names and named lambdas remain
indexed. Nix call edges stay within statically visible same-file lexical scopes.
Literal imports can link already indexed files; dynamic imports are explicitly
unresolved. No Nix expression or imported path is evaluated or followed.

| Added language | Static coverage and limits |
| --- | --- |
| Bash/Shell (`bash`) | `.sh`, `.bash` and extensionless `sh`/`bash`/`dash` shebang scripts, including `env -S`; functions, command sites and literal `source`/`.` records, with conservative definition order and namespace mutation checks. Source targets remain unresolved because runtime working directories are unknown. |
| Go (`go`) | `.go` packages, functions, methods, types, structs, interfaces and imports; unique direct functions resolve across files of the same package and supported local module imports. Parameters and local bindings block fallback; receiver types, interface dispatch, complex callee expressions and dot imports are not inferred. |
| Lua (`lua`) | `.lua` local/global functions, assigned functions, static table methods and literal `require`; direct unmodified same-file bindings resolve, while aliases, mutations, metatables and dynamic receivers stay unresolved. Imports use the project-root `module.lua` / `module/init.lua` layout and are blocked when loader changes are visible. |
| SQL (`sql`) | `.sql` PostgreSQL tables, views/materialized views, functions, relation/foreign-key dependencies and separate function calls; quoted names and CTE scopes are respected. Qualified objects may resolve within one project; unqualified objects require a unique same-file definition. No runtime `search_path` is assumed. |

Go module declarations are read and hashed within a bounded metadata budget;
changing `go.mod` invalidates freshness and resolution without requiring unchanged
Go files to be reparsed. Test-file definitions do not supply production targets;
Go filenames beginning with `_` or `.` never supply package evidence.
Build tags, workspaces, replacements and downloaded dependencies are not evaluated.
Lua import layout is structural evidence, not a guarantee about the runtime loader.
PostgreSQL procedural or quoted function bodies remain explicitly unresolved;
supported parsed SQL bodies retain static references. Other SQL dialects, Astro,
embedded shell fragments and Zsh-specific syntax are outside this expansion.

`map` uses static graph counts and optional focus, with bounded signatures and a
1024–32768 byte budget. Token counts are explicitly approximate. `diff` retains
only two bounded structural snapshots, reports unique symbol/content and call
relationship changes, and flags duplicate comparison identities. `changes` also
consults the retained baseline so deleted symbols remain visible. Supplied line
ranges are applied to both snapshots; coordinates can differ after line shifts.
Missing baselines, incomplete snapshots and unsupported ranges are reported.

## Optional precomputed SCIP data

```bash
ast-index import-scip ./index.scip --manifest ./index.sources.json
ast-index semantic --relation definition --json
ast-index semantic 'producer package symbol' --relation reference --json
ast-index semantic 'producer package Base#' --relation implementation --json
```

Import is CLI-only and explicit. It never launches an indexer or compiler. The
supported subset is binary SCIP protocol version 0, UTF-8 source, definitions,
references and explicitly flagged implementation relationships. Both deprecated
and typed ranges are supported; typed ranges take precedence. Position encoding
must be explicit UTF-8, UTF-16 or UTF-32. Semantic ranges are zero-based and
half-open, with the encoding returned alongside results.

Documents must already be indexed and must match current source bytes. Embedded
`Document.text` can establish the source snapshot. Otherwise the **artifact
producer** must supply a manifest generated alongside the artifact:

```json
{
  "artifact_blake3": "BLAKE3 hex digest of the .scip file",
  "files": { "src/lib.rs": "BLAKE3 hex digest of source bytes at generation time" }
}
```

The importer never manufactures freshness evidence from today's files. Paths
must be canonical relative paths without symlinks. Artifact root metadata never
grants filesystem access. All supplied manifest inputs, including configuration
and lockfiles, are validated at import and rechecked together with source during
semantic queries; any mismatch excludes the artifact's results. Manifests
from the explicit runner additionally include a versioned input inventory, so
newly added selected sources or dependency files also invalidate the snapshot.
Older listed-file manifests retain their listed-file freshness scope. Provenance
includes producer name/version and artifact digest. External-symbol metadata,
hover documentation, producer arguments and unsupported relationship kinds are
not imported. Raw semantic references never overwrite AST call edges.

An explicit Linux Rust producer is available separately:

```bash
# Select one project; the new state directory needs an existing parent.
nix run .#semantic-runner -- rust --root ./project --state-dir ./private-state
ast-index --root ./project --state-dir ./private-state semantic --json
```

The runner requires private state (0700), a Cargo manifest and existing lockfile.
It copies only selected Rust/Cargo inputs to temporary private state and invokes
a pinned rust-analyzer inside Bubblewrap without network or user-home access.
Only the tool closure and snapshot are mounted. Build-script and proc-macro
loading are disabled in the producer's SCIP implementation. Each producer stage
has a 120-second timeout; missing offline dependencies or custom Cargo config
cause an explicit failure. No dependency installation or network fallback occurs.
`--with-dependencies` explicitly includes an existing project-local `vendor/`
directory for crates.io dependencies. The runner supplies fixed source-replacement
configuration only inside its disposable snapshot; the checkout is unchanged.
Git/private registries, custom Cargo configuration and global dependency caches
remain unsupported. Dependencies must already exist as regular files.
Snapshot/SCIP files are removed after import, including failure paths; producer
diagnostics are not retained. Existing semantic data is replaced only after a
successful validated import.

The Rust profile can also supply validated static call targets:

```bash
ast-index --root ./project --state-dir ./private-state callees caller --evidence combined --json
ast-index --root ./project --state-dir ./private-state impact Counter::get --evidence combined --json
ast-index --root ./project --state-dir ./private-state refs --evidence ast --json
```

`combined` is available for refs, describe, callers, callees and impact in CLI/MCP.
It requires a validated complete input inventory, fresh evidence and the same
AST generation/profile; listed-only or embedded-source artifacts remain raw
navigation evidence. Reindexing or
changing source/configuration requires another explicit producer refresh. AST is
the default view. Semantic edges include producer/version/artifact provenance;
unresolved refs expose conflicts, ambiguity and bounded possible-target evidence.
Calls in the profile's source inputs without semantic confirmation remain
`semantic_unverified`; cfg-disabled targets cannot reappear through AST fallback.
Only exact call-token matches and uniquely validated static declarations are
admitted. Trait dispatch and attribute macros remain unsupported. Imported
producer metadata describes evidence; it is not authentication of an artifact.
The [frozen synthetic Rust corpus](docs/semantic-quality.md#executed-rust-acceptance)
passes with 101/101 correct targets, zero wrong targets and two dynamic abstentions.
This is a corpus result, not a blanket accuracy claim for real projects or languages.

TypeScript evidence has a separate explicit adapter:

```bash
ast-index-semantic typescript --root ./project --state-dir ./private-state
ast-index --root ./project --state-dir ./private-state semantic --relation definition --json
ast-index --root ./project --state-dir ./private-state callees caller --evidence combined --json
```

It uses pinned TypeScript 7.0.2's native LSP with UTF-8 positions, after a successful
`--noEmit` check. Its bounded file list selects only compiler-loaded sources for
LSP requests, so excluded files cannot re-enter through inferred projects.
Only TypeScript/TSX sources, `tsconfig*.json`, package manifests
and recognized dependency locks enter the private snapshot. Runtime JavaScript,
arbitrary JSON and user-home files do not. `node_modules/` is excluded by default;
`--with-dependencies` explicitly includes its TypeScript/type sources and selected
metadata. Dependency targets remain external to the project call graph. Automatic type
acquisition and telemetry are disabled; the sandbox has no network or package
manager on its executable search path. Protocol responses, candidate counts and
runtime are bounded. Missing dependencies/configuration fail explicitly, without
retaining diagnostics. The synthetic corpus yields 100/100 correct definition
locations and, separately, 100/100 correct combined call targets, with all three
dynamic cases unresolved. The validator admits stable functions and methods on
verified simple classes; instance calls need a fresh, nonescaping receiver.
Structural receiver types, mutation, inheritance, decorators and constructor
effects abstain. Additional isolated safety tests cover these limits. This is a
[named corpus/profile result](docs/semantic-quality.md#executed-typescript-acceptance),
not a guarantee for arbitrary projects. Old definition-only TypeScript artifacts
require an explicit refresh before `combined` is available.

For already prepared local dependencies, opt in on the relevant runner command:

```bash
ast-index-semantic rust --root ./project --state-dir ./private-rust-state --with-dependencies
ast-index-semantic typescript --root ./project --state-dir ./private-ts-state --with-dependencies
```

Project and dependency inputs share one bound: 2,000 files, 128 MiB total,
2 MiB per file and a 50,000-entry/30-second traversal. Symlinked dependency trees
are rejected; no home cache is discovered, dependency downloaded or package
script executed. Files/configuration/locks are checked before import and at query
time, including additions/deletions. Legacy manifests without an inventory only
check their declared files. These checks are point-in-time observations.

## Resource limits and local state

Managed semantic refresh keeps each canonical project and language profile in
separate private state. Put only explicitly approved relative project selections
in a private runtime file (mode `0600`), outside Git and the Nix store:

```json
{"version":1,"projects":[{"project":"example","profile":"rust","dependencies":false}]}
```

```bash
ast-index-semantic refresh --root ./workspace --state-dir ./private-profiles --projects-file ./private-projects.json
ast-index --root ./workspace --semantic-state-dir ./private-profiles --project example --profile rust callees caller --evidence combined --json
```

The state base must be separate from each selected project. At most eight
project/profile pairs are accepted; paths, symlinks, duplicates and the complete
plan are validated before producer work. New work stops starting after five
minutes; each producer retains its own stage limits. Unchanged, fully validated
inputs skip the compiler. Changed inputs build a disposable candidate, retain
monotonic generations and atomically replace the database only after validation.
Failed refreshes leave the previous snapshot intact; stale evidence still cannot
answer combined queries. Published databases are opened read-only. A base-wide
lock prevents concurrent refreshes. State directories/files use `0700`/`0600`.
Runtime reports contain entry numbers/profile/status, not project paths or source
diagnostics. Hash-based directory names are private metadata, not anonymization.

Configure `--semantic-state-dir` on the CLI, stdio server or socket server.
MCP queries supply both `project` and `profile`; file paths then refer to that
project. Missing profiles return an error without creating state or running a
producer. Other queries continue to use the ordinary workspace AST index.
Managed `freshness` is also read-only: it adds `semantic_current` to the usual
AST comparison and reports stale when configuration/profile inputs no longer
match, even with zero changed AST files. It does not persist a check timestamp.

Normal indexing caps source files at 2 MiB each, scans at 100,000 source files and
256 MiB, and stored symbols/references at 200,000/500,000. Traversal errors or scan
caps produce incomplete coverage; storage caps abort the update. Graph work is
bounded to 20,000 inspected incoming ids and ten hops. Queries also have a SQLite
VM instruction budget. Exact/prefix search uses indexes; substring search scans.

MCP request frames are capped at 1 MiB; tool text at 64 KiB. SQL pages and byte
budgets apply before responses are rendered. SCIP artifacts are capped at 4 MiB,
2,000 documents, 100,000 occurrences and 50,000 symbol records. Semantic source
verification has a 128 MiB aggregate budget; Unicode range validation has a
separate 128 MiB line-scan work budget. Oversized or unsupported work returns
an actionable error or explicit incomplete state.

Each indexing/import operation owns a lock beside the canonical database.
Separate sockets may share readers; a competing writer gets a busy error. The
socket lock owns only the listener. Index paths are root-bound; after relocating
a checkout, use a new local database. The default index directory/database cannot
be redirected through a symlink. Local derived state contains source signatures
and should remain gitignored with the same privacy treatment as source code.

Separate state directories prevent accidental sharing, not access by other
processes under the same Unix UID. Enforce access boundaries with separate
OS identities/sandboxes and directory permissions. Socket paths and their
listener locks are independent of database state: use distinct sockets for
independent instances. A socket proxy uses only its server's state; passing
`--state-dir` to `mcp --socket` is rejected instead of silently ignored.
The same applies to `--semantic-state-dir`: configure the server, not its proxy.

## NixOS service

Declare and lock the input in the consuming flake:

```nix
inputs.ast-index.url = "github:timfewi/ast-index-nix";
```

Pass `inputs` through the consuming configuration's `specialArgs`, then import
a host module such as:

```nix
{ inputs, ... }:
{
  imports = [ inputs.ast-index.nixosModules.default ];
  services.astIndex = {
    enable = true;
    root = "/srv/ast-index/project";
    user = "coding-agent";
    stateDir = "/var/lib/ast-index/instance-a";
    socket = "/run/ast-index/socket";
  };
}
```

The consuming host creates the service account and supplies an existing root
directory readable by that account. With `stateDir`, systemd creates the
directory with mode `0700` owned by the service user/group (`StateDirectory`);
only state and the socket directory are writable through the service sandbox.
The option accepts a normalized path below `/var/lib` with letters, digits,
dots, underscores, hyphens and slashes (no empty, `.` or `..` components).
The default `stateDir = null` retains standalone `<root>/.ast-index` storage
and requires a writable root. No network access is added.

Harnesses then run `ast-index mcp --socket /run/ast-index/socket`. Each mutation
holds the database writer lock; socket ownership is separate. The
socket is created with mode `0600` inside a `0750` runtime directory, so only the
service user can connect.

To opt in to the bounded offline runner, set
`services.astIndex.semantic.enable = true` and
`services.astIndex.semantic.projectsFile = "/var/lib/ast-index/private-projects.json"`
alongside a dedicated `stateDir`. The owner supplies that private runtime file;
Nix never embeds its contents. The runner skips an absent file, runs at most one
batch at a time, starts three minutes after boot and waits five minutes after
each completion (configurable, minimum one minute). The unit has a ten-minute
timeout, private network/tmp, read-only source and private writable semantic
state. Hosts supply resource ceilings and filesystem restrictions. Configuration
evaluation and local runner tests do not establish that a host unit is activated.

## Development

Design notes live in [docs/architecture.md](docs/architecture.md), decisions in
[docs/decisions.md](docs/decisions.md), harness wiring in
[docs/integration.md](docs/integration.md) and the scope/evidence table in
[docs/requirements.md](docs/requirements.md).

```bash
devenv shell -- cargo fetch      # once, to allow --offline checks
devenv shell -- bash scripts/check fast
```

`scripts/check fast` runs `cargo fmt --check`, `clippy -D warnings`, the full
test suite (unit and integration), deadnix/statix, `nixfmt --check`, shellcheck,
and all-system flake evaluation without building the package or system closure.

Source, comments and documentation are in English. Runtime state and
credentials never enter the repository; the index is local and gitignored.
