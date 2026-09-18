# Harness integration

Every harness runs the same binary; only the configuration file differs. Use the
in-process server (`ast-index mcp`) for a single session, or the socket proxy
(`ast-index mcp --socket <path>`) when several harnesses should share one index.

## OpenCode 2

`opencode.json` or `opencode.jsonc`, either global (`~/.config/opencode/`) or in
the project root:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "servers": {
      "ast_index": {
        "type": "local",
        "command": ["ast-index", "mcp"],
        "disabled": false,
        // Keep the single tool visible when OpenCode 2 runs in Code Mode.
        "codemode": false
      }
    }
  }
}
```

Tools are namespaced by server, so permissions can be set with a glob:
`"ast_index_*": "ask"`.

## Codex CLI

`~/.codex/config.toml` or a trusted project's `.codex/config.toml`:

```toml
[mcp_servers.ast_index]
command = "ast-index"
args = ["mcp"]
```

Codex reads the server `instructions` field from the MCP handshake, which is why
the guidance string stays short and self-contained.

## Claude Code

`.mcp.json` in the project (or the user scope):

```json
{
  "mcpServers": {
    "ast_index": { "command": "ast-index", "args": ["mcp"] }
  }
}
```

Claude exposes the tool as `mcp__ast_index__code_explore`. Server instructions
are truncated at 2 KB, which the current string is far below.

## Shared socket service

```bash
# Once per machine or user.
ast-index --root ~/project serve --socket /run/user/1000/ast-index.sock --index

# Every harness.
ast-index mcp --socket /run/user/1000/ast-index.sock
```

The service takes an exclusive lock on `<index>/.lock`-style lock file; a second
writer exits instead of corrupting the database. The socket is mode 0600 inside a
private runtime directory. `ast-index serve` prints its endpoint to stderr; stdout
carries protocol bytes only.

With the NixOS module the same is configured declaratively:

```nix
services.astIndex = {
  enable = true;
  root = "/home/agent/project";
  user = "coding-agent";
  socket = "/run/ast-index/socket";
  indexOnStart = true;
};
```

## Suggested agent guidance

The server's `instructions` field already states the workflow. If a harness
supports additional guidance (AGENTS.md, a skill), the useful addition is a
pointer, not a copy:

> For structural questions (who calls this, what does this file contain, what
> breaks if this changes) call `code_explore` before grepping. Edges are
> name-resolved; verify with a file read before editing.
