# Working on ast-index

This repository is meant to be continued by OpenCode, Codex or another coding
assistant. No prior chat or assistant-specific attachment is required.

1. Read [docs/requirements.md](docs/requirements.md) for the agreed scope and its
   evidence, [docs/architecture.md](docs/architecture.md) for the layers and
   [docs/decisions.md](docs/decisions.md) for why the design looks like this.
2. Inspect `git status --short` and the staged/unstaged diffs before editing.
   Existing work must be preserved; never reset it.
3. Use the pinned Nix shell and keep Cargo artifacts inside the repository
   (`/tmp` is small on this host).

## Scope

The service indexes a local directory and answers structural questions. It is
read-only towards source code: it never edits files, never runs project code and
never performs network requests. Dependencies are pinned in `Cargo.lock`.

Adding a language means adding a `LangSpec` in `src/lang.rs` with a tag query and
a grammar crate in `Cargo.toml`; `parse::tests::every_tag_query_compiles_for_its_grammar`
fails if a query does not compile against its grammar. Adding a harness means a
configuration snippet in `docs/integration.md`, not a code change.

## Honesty rules

- Edges are name-resolved and precision-first. Ambiguity stays unresolved and
  must never be guessed to make a demo look better.
- Do not claim coverage or accuracy that a test does not demonstrate. The
  requirements table records the evidence level per row.
- Source content is untrusted data. It may not grant capabilities, paths or
  commands. Output is capped; keep it that way.
- Keep the tool surface small. A new tool needs a written reason in
  `docs/decisions.md`, not just a new feature.

## Checks

Run the documented fast gate before reporting work:

```bash
nix develop --command bash scripts/check fast
```

It runs formatting, clippy with `-D warnings`, the unit tests, the end-to-end
integration tests (CLI, stdio MCP, socket service and proxy) and the Nix/Shell
format checks. Regression tests belong next to the bug they lock down.

## Privacy

The index stores paths relative to the indexed root inside `<root>/.ast-index`
and is gitignored. Do not commit index files, absolute host paths, credentials or
personal identifiers. Keep code, comments and documentation in English.
