# Requirements and evidence

Scope for the local navigation expansion. "Evidence" names the check that demonstrates the row;
the expansion was verified with `scripts/check fast` (29 unit and 35 integration tests).
Source-review evidence and evaluation-only checks are identified explicitly.

| ID | Requirement | Status | Evidence |
| --- | --- | --- | --- |
| R01 | Index definitions, call sites and imports for Rust, Python, TypeScript, TSX, JavaScript and Nix | done | `parse::tests::*`, `every_tag_query_compiles_for_its_grammar`, `integration::nix_index_answers_queries_without_guessing_dynamic_or_cross_file_targets` |
| R02 | Definitions carry kind, line/column range, parent and qualified name | done | `parse::tests::rust_extracts_definitions_calls_and_imports`, `columns_disambiguate_definitions_on_the_same_line` |
| R03 | References carry the enclosing definition and the call line | done | `parse::tests::python_extracts_classes_methods_and_calls` |
| R04 | Incremental indexing: unchanged files are not parsed, including after mtime drift; changed bytes are detected even with unchanged size and mtime | done | `integration::reindexing_is_incremental_and_removes_deleted_files`, `integration::reindexing_detects_content_changes_with_unchanged_size_and_mtime` |
| R05 | mtime-only drift with identical content is not reparsed | done | same test (BLAKE3 hash branch) |
| R06 | Deleted files leave the index | done | same test |
| R07 | Ambiguous names stay unresolved instead of guessing | done | `resolve::tests::ambiguous_names_stay_unresolved`, `integration::ambiguous_names_stay_unresolved` |
| R08 | Scoped calls resolve qualified-first | done | `resolve::tests::scoped_references_follow_literal_module_paths` |
| R09 | External scoped paths do not fall back to local names | done | `resolve::tests::external_scoped_paths_do_not_fall_back_to_local_names` |
| R10 | Callers, callees and transitive impact queries | done | `integration::indexes_and_answers_structural_queries` |
| R11 | CLI query surface with text and JSON output | done | same test |
| R12 | stdio MCP surface with exactly one tool | done | `rpc::tests::tools_list_exposes_exactly_one_read_only_tool`, `integration::mcp_stdio_adapter_serves_the_single_tool` |
| R13 | Protocol negotiation for older and newer MCP revisions | done | `rpc::tests::initialize_negotiates_known_and_unknown_versions` |
| R14 | Not-indexed is guidance, not an error | done | `rpc::tests::unindexed_status_is_guidance_not_an_error`, `integration::reports_not_indexed_with_exit_code_two` |
| R15 | Notifications produce no response; parse errors use null id | done | `rpc::tests::notifications_get_no_response`, `rpc::tests::parse_errors_are_reported_with_null_id` |
| R16 | Unknown tool is an invalid-params protocol error | done | `rpc::tests::unknown_tool_is_invalid_params` |
| R17 | Persistent Unix socket services with separate socket and database writer coordination | done | `integration::separate_socket_services_share_readers_and_database_writer_coordination` |
| R18 | Byte-preserving stdio↔socket proxy as the only harness-facing process | done | `integration::socket_service_and_stdio_proxy_share_one_index`, `integration::socket_proxy_preserves_crlf_bytes` |
| R19 | MCP frames are bounded at 1 MiB and tool text at 64 KiB | done | `integration::mcp_stdio_rejects_oversized_frame_and_serves_the_next_request`, `integration::socket_service_and_stdio_proxy_share_one_index`, `rpc::tests::tool_text_is_bounded_even_when_arguments_are_large` |
| R20 | Index state is one SQLite file, WAL requested with rollback fallback | done | `Store::open` records `journal_mode` in `meta` |
| R21 | Nix package and NixOS module with socket service | done | both-system package/module evaluation in the fast gate; native Rust compilation and process tests (no Nix package build in this expansion) |
| R22 | No network access and no LLM dependency at runtime | done | source and runtime dependency review, including local-only `prost` decoding |
| R23 | Documented fast gate runs formatting, clippy, tests and Nix/Shell checks | done | `scripts/check fast` |
| R24 | gitignore-style path exclusions keep configured subtrees out of the index | done | `integration::exclude_patterns_skip_subtrees` |
| R25 | Service group is optional; an unset group uses the user's primary group | done | `checks.module-eval` (explicit group passed through, `Group=` absent when unset) |
| R26 | Nix static bindings and inherited attributes, nested qualified names, named lambdas, curried call sites and literal module imports | done | `parse::tests::nix_extracts_bindings_functions_and_literal_imports`, `lang::tests::detects_nix_files_case_insensitively` |
| R27 | Nix calls resolve only to unique, statically visible same-file functions; shadowed, dynamic, selected and cross-file calls remain unresolved | done | `parse::tests::nix_call_targets_respect_recursive_and_nested_let_scopes`, `parse::tests::nix_dynamic_shadowed_and_external_calls_stay_unresolved`, `parse::tests::nix_dynamic_imports_are_not_reported_as_literal_paths`, `integration::nix_index_answers_queries_without_guessing_dynamic_or_cross_file_targets`, `integration::nix_ambiguous_qualified_targets_stay_unresolved` |
| R28 | Same-named nested definitions keep distinct reference owners through full qualified chains | done | `integration::same_named_nested_functions_keep_distinct_callers`, `integration::nix_index_answers_queries_without_guessing_dynamic_or_cross_file_targets` |

## Expansion evidence

| ID | Requirement | Status | Evidence |
| --- | --- | --- | --- |
| R29 | Language/project separation, lexical shadowing, module paths and supported JS/TS imports | done | `resolution_respects_language_projects_and_shadowing`, `imports_are_static_bounded_and_do_not_cross_roots`, resolver module-path negative cases, `lexical_calls_skip_python_class_names_and_respect_comprehension_and_catch_bindings`, `hoisted_reassigned_and_aliased_values_never_resolve_to_outer_functions`, `member_namespaces_and_rust_modules_do_not_leak_bare_bindings`, `named_imports_require_named_top_level_exports` |
| R30 | Database-scoped mutation lock, canonical aliases and concurrent socket readers | done | `writer_lock_blocks_indexing_but_allows_queries_and_canonical_aliases`, `separate_socket_services_share_readers_and_database_writer_coordination`, `writer_rechecks_root_ownership_and_version_drift_invalidates_caches` |
| R31 | File/name/line/column selection and structured ambiguity | done | `selectors_pagination_and_frontend_contracts`, `columns_disambiguate_definitions_on_the_same_line` |
| R32 | Shared CLI/MCP JSON contracts, including describe relationships | done | `selectors_pagination_and_frontend_contracts`, `cli_scip_import_and_mcp_semantic_queries_share_a_contract` |
| R33 | Unique shortest-distance impact, cycle handling and work budgets | done | `impact_cycles_are_unique_and_exclude_the_seed`, bounded traversal in `Store::impact_ids` |
| R34 | Persisted coverage, explicit byte freshness, parser/resolver fingerprints and incomplete-walk retention | done | `freshness_checks_bytes_and_reports_incomplete_coverage`, `incomplete_walk_retains_previously_indexed_files`, `writer_rechecks_root_ownership_and_version_drift_invalidates_caches` |
| R35 | SQL-bounded pages, filters, literal matching and generation/query-bound cursors | done | `selectors_pagination_and_frontend_contracts`, `list_actions_page_consistently_and_references_explain_dynamic_imports`, query-plan test |
| R36 | Imports/reverse importers/static dependencies with unresolved reasons | done | `imports_are_static_bounded_and_do_not_cross_roots`, Nix dynamic-import parser regression |
| R37 | MCP references, resolution provenance/reasons and coverage counts | done | `list_actions_page_consistently_and_references_explain_dynamic_imports`, shared `Engine::execute` projections |
| R38 | Deterministic byte-budgeted repository maps with signatures and heuristic labels | done | `maps_and_snapshots_report_actual_structural_changes` |
| R39 | Explicit changed ranges, two retained snapshots, deleted-symbol impact and unavailable-baseline errors | done | `maps_and_snapshots_report_actual_structural_changes` |
| R40 | Local opt-in SCIP ingestion, separate provenance and no inferred call edges | done | `scip::tests::semantic_evidence_is_separate_and_excluded_when_source_changes`, CLI/MCP semantic parity |
| R41 | SCIP source/manifest validation, ranges, paths, versions, symlinks, bounded decoding and atomic rejection | done | remaining `scip::tests::*` |
| R42 | Parser/query reuse, unchanged-resolution reuse, conservative invalidation and indexed exact/prefix queries | done | `unchanged_scans_skip_resolution_but_project_boundaries_invalidate_it`, `exact_and_prefix_use_indexes_while_contains_is_a_bounded_scan`; parser cache source review |
| R43 | Default derived-state paths cannot follow source-controlled symlinks | done | `source_controlled_symlinks_cannot_redirect_default_index_writes` |
| R44 | Legacy schema preservation, bounded special-file reads and interrupted-query recovery | done | `migration_preserves_legacy_rows_and_requires_new_extraction`, `special_files_are_rejected_without_waiting_for_a_writer`, `interrupted_query_releases_snapshot_and_resets_the_budget` |

## Deliberately deferred

- Watcher acceleration: explicit byte scans remain the correctness mechanism.
- FTS5/vector search: literal indexed searches and bounded substring scans cover
  the present interface; no embedding runtime or model downloads.
- New grammar languages beyond Rust, Python, TS/TSX, JS and Nix.
- Rename/codemods, source write actions, project execution and runtime networking.
- Multiple roots in one database; one root keeps writer and path semantics clear.
- Starting SCIP producers, language servers or build tools.

## Evidence limits

The fixtures demonstrate specific structural cases, not semantic accuracy across
arbitrary programs. Dynamic dispatch, reflection, macros, generated code,
re-exports, complex import aliases, custom module layouts and runtime loaders are
not fully resolved. Named JS/TS imports require a directly exported indexed symbol;
Nix calls stay local and lexical. Unsupported bindings remain unresolved.

Snapshots match unique structural identities, not semantic identity through moves
or renames. Line ranges applied to old and new snapshots can refer to different
text after shifts. SCIP evidence is trusted only as producer-supplied semantic
data tied to verified source bytes; it is not independently type-checked. All
navigation results require confirmation in current source for consequential work.
