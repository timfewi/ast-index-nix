# ast-index

A lightweight, local-first AST code index for agent harnesses. It extracts
definitions, call sites and imports with tree-sitter, stores them in one SQLite
file and exposes the result through three frontends that share one library:

- `ast-index` — a CLI with `--json` output for scripts and non-MCP harnesses,
- `ast-index mcp` — a stdio MCP adapter with **exactly one tool** (`code_explore`),
- `ast-index serve --socket <path>` plus `ast-index mcp --socket <path>` — a
  persistent Unix socket service and a byte-level proxy as the only harness-facing
  process.

The design mirrors a local research service: the index lives in a daemon behind a
socket, and every harness talks to the same warm index through a tiny proxy.
There is no telemetry, no network access and no LLM dependency.

## Quick start

```bash
nix develop --command cargo build --release

# Build or update the index for the current repository.
target/release/ast-index index

# Ask questions.
ast-index status
ast-index search parse
ast-index outline src/parse.rs
ast-index describe ScopeMaps::build
ast-index callers resolve_all
ast-index impact resolve_all --depth 3
```

The index lives in `<root>/.ast-index/index.sqlite` and is gitignored. Add
`.ast-index/` to your global ignore list if you index many repositories.

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

## The single tool

`code_explore` takes an `action`:

| action | meaning |
| --- | --- |
| `status` | index coverage and counts |
| `search` | find definitions by name or qualified name |
| `outline` | list the definitions of one file |
| `describe` | one symbol with callers and callees |
| `callers` / `callees` | one edge direction |
| `impact` | transitive callers up to `depth` |

`format: "json"` switches to JSON; `text` is the default because it is more
token-efficient. Output is capped so one call cannot flood the model context.

## What it does and does not resolve

Extraction uses tree-sitter tag queries and covers Rust, Python, TypeScript,
TSX and JavaScript. Call edges are resolved by name with a precision-first scope
ladder:

1. exactly one match in the same file → `exact`,
2. exactly one match in the same directory → `high`,
3. exactly one match in the whole index → `low`,
4. ambiguous or unknown → unresolved, reported as a plain reference.

Scoped calls such as `Store::open` are matched against qualified names first, so
they never collapse onto a same-named local method. External paths such as
`Connection::open` stay unresolved on purpose: linking them to a local `open`
would be a guess. Dynamic dispatch, macros, reflection, generated code and
same-named methods reached through a value are **not** resolved. Treat every edge
as a hint and read the file before editing.

## NixOS service

```nix
{
  inputs.ast-index.url = "git+https://github.com/timfewi/ast-index-nix.git?ref=main";
  # ...
  imports = [ inputs.ast-index.nixosModules.default ];
  services.astIndex = {
    enable = true;
    root = "/home/agent/project";
    user = "coding-agent";
    socket = "/run/ast-index/socket";
  };
}
```

Harnesses then run `ast-index mcp --socket /run/ast-index/socket`. The service
holds an exclusive lock, so a second writer cannot open the same index, and the
socket is created with mode `0660` inside a `0750` runtime directory.

## Development

Design notes live in [docs/architecture.md](docs/architecture.md), decisions in
[docs/decisions.md](docs/decisions.md), harness wiring in
[docs/integration.md](docs/integration.md) and the scope/evidence table in
[docs/requirements.md](docs/requirements.md).

```bash
nix develop --command cargo fetch      # once, to allow --offline checks
nix develop --command bash scripts/check fast
```

`scripts/check fast` runs `cargo fmt --check`, `clippy -D warnings`, the full
test suite (unit and integration) plus `nixfmt --check` and `shellcheck`.

Source, comments and documentation are in English. Runtime state and
credentials never enter the repository; the index is local and gitignored.
