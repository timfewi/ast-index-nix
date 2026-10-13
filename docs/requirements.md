# Requirements and evidence

Scope for local structural navigation. "Evidence" names targeted checks;
source-review evidence and evaluation-only checks are identified explicitly.
The current fast gate and remaining coverage limits are recorded in
[development.md](development.md).

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
| R28 | Same-named definitions keep distinct reference owners through qualified chains and containing source positions | done | `integration::same_named_nested_functions_keep_distinct_callers`, `integration::nix_index_answers_queries_without_guessing_dynamic_or_cross_file_targets` |

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

## Added language evidence

| ID | Requirement | Status | Evidence |
| --- | --- | --- | --- |
| R45 | Bash function definitions, static commands/source records, definition order and dynamic namespace barriers | done | `parse::bash::tests::*`, `shell_shebangs_and_incremental_detection_are_bounded_and_static` |
| R46 | Go declarations, package-wide direct calls, local module imports, lexical shadows and test-file isolation | done | `parse::go::tests::*`, `resolve::go::tests::*`, `go_packages_imports_module_changes_and_shadowing_share_cli_mcp_contracts` |
| R47 | Lua lexical/global functions, static table methods, literal requires, shadowing, reassignment and loader barriers | done | `parse::lua::tests::*`, `new_languages_filter_and_outline_identically_over_cli_and_mcp`, `lua_static_module_layouts_and_loader_mutations_remain_explicit` |
| R48 | PostgreSQL tables/views/functions, static dependencies separate from calls, quoted identifiers, CTEs and unresolved dynamic namespaces/bodies | done | `parse::sql::tests::*`, `sql_dependencies_never_become_calls_or_index_internal_database_files`, `custom_sql_database_and_renamed_sqlite_are_excluded_from_source_indexing`, SQL namespace/CTE/quoted-identifier integration tests |
| R49 | Added language filters, positions/owners and CLI/MCP output contracts use the existing tool | done | `new_languages_filter_and_outline_identically_over_cli_and_mcp`, `every_tag_query_compiles_for_its_grammar`, `sql_reference_owners_use_ranges_when_table_and_function_names_match`, language parser tests |
| R50 | Extensionless shebang detection, module metadata freshness, repeated indexing, binary SQLite header rejection and unconditional exclusion of internal index files | done | `shell_shebangs_and_incremental_detection_are_bounded_and_static`, `go_packages_imports_module_changes_and_shadowing_share_cli_mcp_contracts`, `sql_dependencies_never_become_calls_or_index_internal_database_files`, `custom_sql_database_and_renamed_sqlite_are_excluded_from_source_indexing`, SQL namespace/CTE/quoted-identifier integration tests |
| R51 | Header, Go metadata and source reads share a byte budget, including failures; external module metadata is rejected | done | `failed_reads_consume_the_shared_budget`, `new_language_metadata_reads_share_scan_budget_and_reject_root_escape` |

## Deliberately deferred

- Watcher acceleration: explicit byte scans remain the correctness mechanism.
- FTS5/vector search: literal indexed searches and bounded substring scans cover
  the present interface; no embedding runtime or model downloads.
- Astro, other SQL dialects, Zsh-specific syntax and embedded shell/SQL fragments.
- Go build tags, workspace/replacement evaluation and external dependency loading.
- Runtime shell working directories, Lua module loaders and SQL search paths.
- Rename/codemods, source write actions, project execution and runtime networking.
- Multiple roots in one database; one root keeps writer and path semantics clear.
- Starting SCIP producers, language servers or build tools.

## Evidence limits

The fixtures demonstrate specific structural cases, not semantic accuracy across
arbitrary programs. Dynamic dispatch, reflection, macros, generated code,
re-exports, complex import aliases, custom module layouts and runtime loaders are
not fully resolved. Named JS/TS imports require a directly exported indexed symbol;
Nix and Lua calls stay local and lexical; Bash calls use supported static execution
order. Go package relationships do not establish build-configuration correctness.
Lua imports use a documented project-root layout, not runtime loader discovery.
SQL support is a PostgreSQL source subset: it does not reconstruct a database or
migration history, infer overloaded function signatures or inspect procedural
bodies. Schema-qualified dependencies may cross indexed files within one project;
unqualified relationships are restricted to unique same-file objects. Unsupported
bindings remain unresolved.

Snapshots match unique structural identities, not semantic identity through moves
or renames. Line ranges applied to old and new snapshots can refer to different
text after shifts. SCIP evidence is trusted only as producer-supplied semantic
data tied to verified source bytes; it is not independently type-checked. All
navigation results require confirmation in current source for consequential work.
