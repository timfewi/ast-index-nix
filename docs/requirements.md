# Requirements and evidence

Scope for version 0.1.0. "Evidence" names the check that demonstrates the row;
a row is only complete when that check runs in `scripts/check fast`.

| ID | Requirement | Status | Evidence |
| --- | --- | --- | --- |
| R01 | Index definitions, call sites and imports for Rust, Python, TypeScript, TSX and JavaScript | done | `parse::tests::*`, `every_tag_query_compiles_for_its_grammar` |
| R02 | Definitions carry kind, line range, parent and qualified name | done | `parse::tests::rust_extracts_definitions_calls_and_imports` |
| R03 | References carry the enclosing definition and the call line | done | `parse::tests::python_extracts_classes_methods_and_calls` |
| R04 | Incremental indexing: unchanged files are not parsed | done | `integration::reindexing_is_incremental_and_removes_deleted_files` |
| R05 | mtime-only drift with identical content is not reparsed | done | same test (BLAKE3 hash branch) |
| R06 | Deleted files leave the index | done | same test |
| R07 | Ambiguous names stay unresolved instead of guessing | done | `resolve::tests::ambiguous_names_stay_unresolved`, `integration::ambiguous_names_stay_unresolved` |
| R08 | Scoped calls resolve qualified-first | done | `resolve::tests::scoped_references_prefer_qualified_names` |
| R09 | External scoped paths do not fall back to local names | done | `resolve::tests::external_scoped_paths_do_not_fall_back_to_local_names` |
| R10 | Callers, callees and transitive impact queries | done | `integration::indexes_and_answers_structural_queries` |
| R11 | CLI query surface with text and JSON output | done | same test |
| R12 | stdio MCP surface with exactly one tool | done | `rpc::tests::tools_list_exposes_exactly_one_read_only_tool`, `integration::mcp_stdio_adapter_serves_the_single_tool` |
| R13 | Protocol negotiation for older and newer MCP revisions | done | `rpc::tests::initialize_negotiates_known_and_unknown_versions` |
| R14 | Not-indexed is guidance, not an error | done | `rpc::tests::unindexed_status_is_guidance_not_an_error`, `integration::reports_not_indexed_with_exit_code_two` |
| R15 | Notifications produce no response; parse errors use null id | done | `rpc::tests::notifications_get_no_response`, `rpc::tests::parse_errors_are_reported_with_null_id` |
| R16 | Unknown tool is an invalid-params protocol error | done | `rpc::tests::unknown_tool_is_invalid_params` |
| R17 | Persistent Unix socket service with an exclusive writer lock | done | `integration::socket_service_and_stdio_proxy_share_one_index` |
| R18 | stdio↔socket proxy as the only harness-facing process | done | same test |
| R19 | Output is capped so one call cannot flood the context | done | `report::MAX_LIST_LINES` used by every list renderer |
| R20 | Index state is one SQLite file, WAL requested with rollback fallback | done | `Store::open` records `journal_mode` in `meta` |
| R21 | Nix package and NixOS module with socket service | done | `checks.module-eval`, `packages.default` build |
| R22 | No network access and no LLM dependency at runtime | done | dependency review; only `tree-sitter`, `rusqlite`, `clap`, `serde`, `nix`, `ignore`, `blake3` |
| R23 | Documented fast gate runs formatting, clippy, tests and Nix/Shell checks | done | `scripts/check fast` |
| R24 | gitignore-style path exclusions keep configured subtrees out of the index | done | `integration::exclude_patterns_skip_subtrees` |

## Deliberately deferred

| ID | Item | Reason |
| --- | --- | --- |
| D-01 | FTS5 ranking | Depends on the SQLite build option; `LIKE` over an indexed column is sufficient for name search (D04) |
| D-02 | File watcher | Correctness must not depend on inotify; a watcher adds an accelerator only (D06) |
| D-03 | Go, Java, C/C++, Ruby, Swift, Elixir | Each needs a grammar crate and a tag query; the registry is one entry plus one query |
| D-04 | `deps`/import graph queries | Imports are stored and counted; exposing them is a new action with its own tests |
| D-05 | Rename/codemod or any write action | The server is read-only towards the repository by design |
| D-06 | Multi-root indexes in one database | One index per root keeps the writer lock and path semantics simple |

## Known limitations

- Calls through a value only expose the method name and can link to a unique
  same-named method in the same directory (documented in D05).
- Dynamic dispatch, reflection, macros, generated code and decorators are not
  resolved.
- TypeScript/JavaScript extraction is structural: it does not follow re-exports,
  path aliases or `require()` indirection.
- Python decorators are not represented as definitions; the decorated function
  is.
