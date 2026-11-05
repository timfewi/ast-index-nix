# Private semantic evidence and the 90% target

Exact positions, independent Rust/TypeScript corpora and an explicit offline
Rust producer are implemented. Semantic call overlays, the TypeScript producer
and the recall/precision acceptance gate remain planned.
This is not a claim that the target has been achieved.
Keep the existing offline AST engine; add locally produced semantic evidence
through an explicit adapter. Do not train a model, upload source, or copy a
user's repositories into a public test corpus to improve resolution.

## Define the result before optimizing

For each supported language/profile, aim for at least 90% recall of labelled
internal, statically determinable call targets, with at least 99% precision of
returned targets. A wrong target is both a missed expected target and a false
positive. An unanswered eligible case is a miss, including an unsupported
construct or an unavailable producer. Report ambiguous/runtime-dependent and
external cases separately; do not silently reclassify difficult eligible cases.

Definition navigation, import-file links and call targets are separate metrics.
A semantic reference to a type, a method declaration, or a possible runtime
implementation is not automatically a proven direct call. Report the existing
all-reference link rate alongside the labelled results; it will not become
90% merely because a semantic profile passes its corpus.

Freeze a versioned corpus before measuring changes. Start with at least 100
distinct eligible call sites per initial language, covering local/cross-file
functions, aliases, re-exports, concrete receivers, generics and same-line calls.
Include negative cases for shadowing, reassignment, ambiguous dispatch, missing
dependencies, malformed input, stale source and project-boundary escapes.
Use independently reviewed expected target ranges; never generate the gold
answers from the producer being scored. Keep a held-out family of fixtures and
publish per-family failures as well as aggregate counts. Passing this corpus
does not establish the same recall on arbitrary projects or languages.

## Verified starting point

- [The importer](../src/scip.rs) accepts local SCIP definitions, references and
  implementations with producer provenance, bounded decoding, relative-path
  validation and source-snapshot checks. Producer arguments and hover text are
  not persisted. Semantic evidence currently stays separate from AST edges.
- [Reference storage](../src/store.rs) persists zero-based half-open UTF-8
  syntax ranges since schema 7. Schema 8 also retains manifest input hashes.
  All extractors supply ranges; legacy rows retain
  null until re-extraction. CLI/MCP tests cover Unicode, CRLF, nested and
  same-line calls across eight languages. Rust generic-call extraction is
  included; range storage alone does not establish a semantic target.
- [The Rust oracle](../tests/quality.rs) has 101 eligible labelled call sites
  across ten families, including a fixed control-flow family, and two explicit
  abstention cases. Expected identities are hand-authored source labels in
  synthetic code. The evaluator tests failed producer, recall, precision and
  safety outcomes independently. This validates the oracle, not a producer's
  recall. Producer answers must never replace expected labels to make it pass.
- The TypeScript oracle freezes 100 eligible calls across ten families and
  three abstention cases, including typed-but-overridable receivers. It
  typechecks with the pinned TypeScript compiler. JS/TS constructor expressions
  now have exact AST call ranges; this fixes a demonstrated extraction gap.
- [The Rust runner](../src/producer.rs) runs the pinned, patched rust-analyzer
  2026-09-21 in Bubblewrap with only its tool closure, a read-only selected source
  snapshot and private output. Network and user-home access are absent. Its SCIP
  entrypoint has build-script and proc-macro loading disabled in source; ordinary
  LSP settings do not disable those hardcoded CLI paths in this producer version.
  An executable synthetic build-script canary, real SCIP import, temporary-output
  cleanup and lockfile invalidation pass. Custom Cargo configuration and missing
  offline dependencies fail explicitly. Dependency-cache provisioning and further
  producer profiles remain unimplemented. This is not an accuracy result.
- Import replaces the previous semantic artifact. Queries check all imported
  documents and supplied configuration/lock inputs, with a combined 2,000-file
  bound; artifacts are capped at 4 MiB. This
  is not yet an incremental semantic service for an entire multi-repo workspace.
- A local synthetic Rust smoke using rust-analyzer 2026-09-21 produced SCIP for
  `value.tick()` where `value: &Counter`. AST resolution abstained; SCIP attached
  the same symbol identity to the method token and its definition. The importer
  required an artifact/source BLAKE3 manifest because this producer omitted
  embedded source text. Changing the source made semantic queries exclude the
  stale artifact. This proves that narrow data path, not a recall percentage.

## Implementation order and exit criteria

1. **Public synthetic corpus and evaluator.** Author small standalone projects
   with neutral names and no user-derived code. Store expected source/target
   ranges and exclusions with rationale. Score AST-only and AST-plus-semantic
   profiles against identical inputs. Add a deliberately wrong target and a
   missing producer to prove the gate fails rather than skips. Keep this
   deterministic evaluator separate from performance benchmarks.
2. **Exact AST call positions.** Persist callee-token start/end positions, with
   one documented canonical encoding. Update all language extractors, storage,
   query projections, schema migration and extraction fingerprints together.
   Re-extract old snapshots; do not synthesize exact positions from a line.
   Test two calls on one line, Unicode, CRLF and nested call expressions.
3. **Explicit local producer adapters.** Begin with Rust, then TypeScript.
   Pin and test producer versions independently of the protocol. A separate
   runner creates artifacts and a source-hash manifest from one stable snapshot;
   changes during generation invalidate the result. The core importer must not
   launch a compiler, install a package or fetch dependencies. Unavailable
   dependencies, unsupported producer flags and unresolved project configuration
   are visible gaps. Add Python/Go profiles only after equivalent validation;
   make no blanket 90% promise for Nix, shell, SQL or dynamic language behavior.
4. **Provenance-preserving call overlay.** Join a known AST callee-token range to
   a fresh semantic occurrence, then to a unique internal definition. Match
   project, source snapshot and position encoding, not symbol spelling. An
   AST call plus a definition reference alone does not prove concrete dispatch:
   admit only validated static-dispatch cases or explicit call evidence from a
   producer. Preserve possible-target relationships separately. Conflicting
   AST/semantic results and multiple targets remain explicit, not first-match
   wins. Expose evidence origin, producer version, snapshot and ambiguity in
   callers/callees/impact without overwriting the AST-only result. This changes
   the current no-semantic-call-edge contract and requires a reviewed decision,
   schema/query updates and negative tests before it ships.
5. **Bounded per-project refresh.** Store artifacts under project/profile
   identity; invalidate semantic results when source, module configuration,
   dependency locks or producer settings change. Increase no existing limits
   without bounded decoding/query tests. Never serve a stale edge as current.
6. **Integration acceptance.** Require the recall/precision gates per enabled
   profile, zero false targets in safety fixtures, CLI/MCP parity, deterministic
   fresh/changed-source tests and a privacy check. Report which languages,
   configuration and corpus revision passed. Add an opt-in host runner only
   after these checks; leave owner activation separate.

## Privacy boundary

| Data | Location and handling |
| --- | --- |
| Implementation and authored synthetic fixtures | Public repository; review source and staged outgoing changes before publication |
| Optional public evaluation projects | Separately reviewed revision/license/provenance; public availability is not proof of absent personal data |
| User source, symbol names and paths | Approved local project and private runtime index only; never copied into fixtures, docs, issues or CI artifacts |
| Real SCIP, manifests, index databases and diagnostic logs | Private runtime state, outside source and Nix build inputs; never uploaded automatically |
| Public CI reports | Synthetic case IDs and expected/observed synthetic results; no real project names, paths or snapshots |

No telemetry, cloud semantic service, embeddings or remote model is needed for
this design. A semantic artifact can itself contain source text, internal
symbols, package names and producer metadata: treat it as sensitive even when
it has no passwords. Hashes are integrity evidence, not anonymization. Do not
publish real-project manifests or hashes as a substitute for source.

The host runner must enforce its own filesystem/process/network boundary;
ordinary flags and same-UID execution are not isolation. It should receive only
an approved project snapshot and runtime output directory, no personal home or
credential channels, and no network. Pre-stage approved dependencies. Disable
project build scripts, plugins and procedural macros by default where supported;
if required for coverage, report the gap or require a separately authorized
isolated workload. Do not hide project execution inside a routine refresh.

Keep output private (owner-only directory/files) and out of Git/Nix inputs by
construction. Gitignore and scanners are additional checks, not disclosure
boundaries. Before release, place synthetic privacy canaries in ignored runtime
files and ensure they appear in neither tracked output, package sources nor CI
reports. Persist no diagnostics containing source snippets by default.

The existing core remains local, but a coding agent may send a requested tool
result to its configured model provider. A local index alone does not make the
whole agent session offline or authorize that transmission. This plan makes no
claim of a newly enforced sandbox or a completed publication/privacy audit of
all existing history.

## Primary references

Reviewed for feasibility, not as evidence of a 90% result:

- [SCIP protocol and producer catalogue](https://github.com/scip-code/scip):
  definitions/references/implementations; lists rust-analyzer, scip-typescript
  and scip-python as producers.
- [rust-analyzer navigation features](https://rust-analyzer.github.io/book/features.html#go-to-definition):
  semantic definition and reference navigation. The installed producer's
  `--help` supplied the tested SCIP CLI contract; its subcommands are unstable.
- [LSP 3.17 call hierarchy](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#textDocument_prepareCallHierarchy):
  separate incoming/outgoing calls with source ranges. This is a candidate
  adapter interface, not a guarantee that every language server implements it
  or identifies every runtime dispatch target.
