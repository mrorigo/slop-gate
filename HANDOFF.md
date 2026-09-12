# Handoff

## P0 — Clean product transformation (complete)

Date: 2026-09-12

### Completed

1. Renamed the Cargo package, library, binary help surface, and project
   metadata from `semble-rs`/`semble` to `slop-gate`.
2. Replaced the search-first CLI with the product commands `index`, `check`,
   and `scan`. They intentionally return exit code 2 until their implementation
   phases; no legacy search or MCP operation remains reachable.
3. Removed model, embedding, BM25, MCP, HTTP, async, cache, benchmark, and
   search source dependencies from the default build. The dependency graph is
   now only Clap.
4. Rewrote the root README as Slop Gate documentation and added a project-local
   artifact ignore path, `/.slop-gate/`.

### Validation

Passed on 2026-09-12:

1. `cargo check`
2. `cargo fmt --check`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test`
5. `cargo run -- --help` (only `index`, `check`, and `scan` are exposed)

### Next phase: P1

1. Add the deterministic analysis layer and the Rust Tree-sitter dependency.
2. Implement Rust function extraction, SLOC/CC, normalized fingerprints, and
   strict versioned artifact serialization.
3. Add fixture-driven tests before implementing Git delta evaluation.

### Deliberate decisions

1. The legacy Semble source is removed rather than retained as a hidden module
   or default dependency. Git history remains the recovery path.
2. Search, MCP, Model2Vec, and semantic embeddings are not part of the Slop
   Gate product baseline.

## P1 — Deterministic Rust analysis core (complete)

Date: 2026-09-12

### Completed

1. Added a Rust Tree-sitter analyzer that extracts named functions and records
   lexical module, trait, and implementation scopes in the function identity.
2. Defined deterministic Rust metrics: non-blank/non-comment-only physical
   SLOC, versioned AST-node cyclomatic complexity, `cc * sqrt(sloc)` mass,
   identifier/literal-normalized hashes, and sorted five-token shingle hashes.
3. Rejects syntax-error trees rather than producing incomplete facts. Nested
   named functions are isolated from a parent function's metrics.
4. Added `IndexArtifact` schema v1 with stable ordering, BLAKE3 analyzer
   fingerprinting, exact Git-object-ID validation, relative-path validation,
   and JSON round-trip validation.
5. Added direct metric tests, malformed-source coverage, fingerprint invariance
   coverage, and a checked-in Rust fixture covering module/implementation scope.

### Validation

Passed on 2026-09-12:

1. `cargo check`
2. `cargo fmt --check`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test`
5. `git diff --check`

### Next phase: P2

1. Add the narrow Git-command boundary and CLI arguments for `index` and
   `check`.
2. Build an artifact from a revision, compare changed Rust files against it,
   and implement function-mass findings plus human/JSON output.
3. Add temporary-Git-repository integration tests for new, changed, renamed,
   deleted, missing-base, stale-artifact, and dirty-tree cases.

### Constraints carried forward

1. The artifact is intentionally not a search cache: it contains only analysis
   facts and no source text or model data.
2. Rust is the only gate-supported language until another extractor has its own
   metric and normalization fixtures.

## P2 — Git delta evaluator and function-mass gate (complete)

Date: 2026-09-12

### Completed

1. Added a narrow, fallible local-Git boundary that resolves commits, reads
   immutable revision blobs, lists revision-tracked Rust files, and detects
   modified/added/copied/renamed head files.
2. Implemented `slop-gate index --ref <commit> --output <artifact>` to create
   an exact-commit baseline artifact.
3. Implemented `slop-gate check --base <commit> --head <commit> --index
   <artifact> --format human|json` with strict artifact/base validation.
4. Implemented `function-mass` findings for new functions above 80.0 mass and
   materially changed functions whose mass increases by more than 20.0.
   Findings include locations, base/head mass, delta, and threshold; errors
   return exit code 1, operational failures return 2.
5. A syntactically invalid changed Rust file is emitted as an `analysis-error`
   warning and skipped. `index` still fails on invalid baseline source.
6. Rename matching maps the head path back to its base path, so an unchanged
   renamed function is not treated as newly introduced code.

### Validation

Passed on 2026-09-12:

1. `cargo check`
2. `cargo fmt --check`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test` (14 tests)
5. End-to-end `index` then `check --base HEAD --head HEAD --format json`
6. `git diff --check`

### Next phase: P3

1. Add artifact shingle lookup and bounded exact-Jaccard clone evaluation for
   base functions and earlier changed head functions.
2. Add a SARIF 2.1.0 projection from the existing finding model.
3. Implement `scan` with the same evaluator over all functions in one revision.

### Policy debt intentionally left for configuration work

1. P2 uses the plan's starter mass thresholds as hardcoded error gates so its
   CI exit contract can be tested. P4 must replace them with `.slop-gate.toml`
   severity and threshold configuration; the documented shipped default is
   warning-only until calibration is complete.
2. `check` evaluates commit revisions only. An explicit working-tree head mode
   remains deferred rather than silently including dirty edits.

## P3 — Near-clone gate, SARIF, and scan (complete)

Date: 2026-09-12

### Completed

1. Added bounded same-language clone candidate retrieval from sorted normalized
   five-token shingles, followed by exact Jaccard scoring. Candidates below 8
   SLOC or 40 normalized tokens are excluded; each subject scores at most 64
   candidates and must meet 0.85 similarity.
2. `check` compares new/materially changed functions to base-artifact functions
   and earlier changed head functions. Exact normalized hashes produce 1.0
   similarity, and a function is never compared to its own base predecessor.
3. Implemented `slop-gate scan --ref <commit> --format human|json|sarif` for
   repository-wide near-clone auditing.
4. Added SARIF 2.1.0 rendering from the shared finding model. Rule IDs,
   locations, related locations, severity, thresholds, and metric properties
   remain consistent across human, JSON, and SARIF outputs.
5. Added clone tests for an identifier-renamed base clone and a same-revision
   scan pair, plus a SARIF structural test.

### Validation

Passed on 2026-09-12:

1. `cargo check`
2. `cargo fmt --check`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test` (17 tests)
5. End-to-end `index` and `scan --format sarif`
6. `git diff --check`

### Next phase: P4

1. Add `.slop-gate.toml` validation and move P2/P3 hardcoded severity and
   thresholds into repository configuration with warning-only defaults.
2. Add Python and TypeScript only with independent extraction/metric fixtures.
3. Publish CI workflow examples and a reproducible benchmark/calibration report.

### Calibration observation

Scanning this repository found two exact `is_relative_path` helpers. This is a
valid structural duplicate but may be a deliberate internal utility. It is the
reason configuration and an explicit suppression policy must precede broad
error-level rollout.

## P4 — Configuration and rollout hardening (in progress)

Date: 2026-09-12

### Completed so far

1. Added strict `.slop-gate.toml` schema-v1 parsing with unknown-key rejection,
   finite threshold validation, and validated exact-location suppressions.
2. Every CLI command now loads and validates repository policy before analysis.
   A missing file yields warning-only defaults, covered by tests.

### Remaining before P4 is complete

1. Thread parsed severity, thresholds, suppressions, and policy fingerprint
   into artifact creation and gate evaluation.
2. Add Python and TypeScript only with equivalent fixture-driven extractors.
3. Add CI workflow examples and a reproducible calibration/benchmark report.

### Rollout assets added

1. `.github/workflows/slop-gate.yml` demonstrates main-branch artifact creation
   and pull-request SARIF evaluation.
2. `docs/CALIBRATION.md` defines the required warning-to-error promotion audit.

### Deliberate scope decision

Python and TypeScript remain unsupported in this phase. Their function and
complexity semantics require language-specific fixtures and rule maps; adding
them without those would violate the tool's deterministic-gate contract.

### Completed policy binding

1. `index` fingerprints the active validated policy into the artifact; `check`
   rejects an artifact built with different policy values.
2. Mass and clone findings now honor `off`, `warn`, and `error` plus exact
   suppressions. Warning-only defaults therefore report without failing CI.
3. Clone findings from both `check` and `scan` use the shared policy path.

## Error migration — typed recoverable failures (complete)

Date: 2026-09-12

### Completed

1. Added a small `thiserror`-based `AppError`/`AppResult` layer and removed
   every production `Result<T, String>` boundary.
2. Converted configuration, Git, Tree-sitter analysis, artifact validation,
   gate evaluation, and CLI orchestration to propagate typed errors rather
   than reparsing display text.
3. Preserved filesystem sources and retained contextual operation, resource,
   and invariant labels for Git, UTF-8, serialization, configuration, and
   analysis failures. The Git runner remains injected for deterministic tests.
4. Kept syntax errors in changed head files as advisory `analysis-error`
   findings; malformed baseline input and operational failures still exit 2.

### Scope decision

The migration deliberately uses one compact application error enum instead of
the plan's nested per-module error hierarchy. Slop Gate is currently a CLI
product with no stable library consumers, so separate public error families
would add conversion code without improving the user-facing diagnostic or CI
contract.

### Validation

Passed on 2026-09-12:

1. `cargo fmt --check`
2. `cargo check`
3. `cargo clippy -- -D warnings`
4. `cargo test` (21 tests)
5. `git diff --check`

### P4 completion boundary

The Rust CI product, policy configuration, SARIF workflow example, and
calibration protocol are complete. Python/TypeScript support is deferred to a
dedicated language-expansion phase with separate specifications and fixtures.

## Pragmatic Rust compliance audit (complete)

Date: 2026-09-12

### Completed

1. Made missing public documentation and unsafe code compile-time violations.
2. Replaced infallible standard-output report emission with fallible locked
   writes that surface broken-pipe and output failures as typed errors.
3. Audited production code for panic macros, `unwrap`, `expect`, unsafe code,
   and unstructured print macros. The remaining `unwrap` calls are limited to
   test setup and assertions.
4. Marked every Rust source file as compliant only after the static and test
   checks passed.

### Validation

Passed on 2026-09-12:

1. `cargo fmt --check`
2. `cargo check`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test` (22 tests)
5. `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps`
6. `cargo audit`
7. `git diff --check`

## P5 — Introduced-debt rules (planned)

Date: 2026-09-12

### Planned sequence

1. Add a tested Git added-line model so new rules report only debt introduced
   by the head revision, never legacy matches in a touched file.
2. Add `lint-suppression-growth` for newly introduced or broadened Rust lint
   exemptions.
3. Add `unsafe-surface-growth` for newly introduced unsafe constructs.
4. Add `dependency-surface-growth` for new direct Cargo dependency edges and
   enabled dependency feature surface.
5. Calibrate each enabled rule before promoting it to an error gate.

`policy-drift` and `error-context-loss` are deferred repository-contract
features, not default gates. Clippy remains the owner of local lint semantics.

The executable specification, acceptance criteria, deferred candidates, and
open product decisions are in `ROADMAP.md`.

## P5.0 — Added-line model (complete)

Date: 2026-09-12

### Completed

1. Added `GitRepository::added_lines(base, head, path)` using a strict
   zero-context Git diff boundary and one-based head line sets.
2. Added parsing for new files, modified files, deletions, unchanged renames,
   content-changing renames, implicit one-line hunk counts, and zero-length
   hunks. Malformed hunk headers fail without returning partial results.
3. Extended changed-path metadata to include head `Cargo.toml` files and
   rename mappings, with parsed added lines retained for Rust and manifest
   paths.
4. Added unit coverage for the parser and exact Git command invocation.

### Validation

Passed on 2026-09-12:

1. `cargo fmt --check`
2. `cargo check`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test` (28 tests)
5. `git diff --check`

### Next phase: P5.1

1. Implement `lint-suppression-growth` extraction, policy, and renderers using
   the added-line facts.

## P5.1 — Lint-suppression growth (complete)

Date: 2026-09-12

### Completed

1. Extracted normalized outer, inner, and conditional `allow`/`expect`
   suppressions from Rust syntax trees.
2. Reports newly introduced suppressions and strict lint-path expansions only
   when the attribute token is on a head added line. Reformatting and relocation
   of an unchanged suppression are ignored.
3. Added warning-only defaults, configurable error severity, exact path/line
   suppression, and stable finding properties for JSON and SARIF output.
4. Added unit, integration, policy, JSON, and SARIF coverage.

### Validation

Passed on 2026-09-12:

1. `cargo fmt --check`
2. `cargo check`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test` (34 tests)
5. `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps`
6. `git diff --check`

### Next phase: P5.2

1. Implement `unsafe-surface-growth` extraction and policy.

## P5.2 — Unsafe-surface growth (complete)

Date: 2026-09-12

### Completed

1. Extracted unsafe blocks, unsafe functions, unsafe traits, unsafe
   implementations, and unsafe extern blocks from Rust syntax trees.
2. Reports only newly introduced unsafe constructs using added-line facts and
   base/head surface counts, so edits inside existing unsafe blocks are ignored.
3. Added warning-only defaults, configurable error severity, exact path/line
   suppression, and stable finding properties for JSON and SARIF output.
4. Added extractor fixtures and end-to-end tests for all five forms and the
   existing-surface regression case.

### Validation

Passed on 2026-09-12:

1. `cargo fmt --check`
2. `cargo check`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test` (36 tests)
5. `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps`
6. `git diff --check`

### Next phase: P5.3

1. Implement `dependency-surface-growth` manifest parsing and policy.

## P5.3 — Dependency-surface growth (complete)

Date: 2026-09-12

### Completed

1. Extracted direct production and build dependency edges from root and
   target-specific manifest tables, excluding dev and workspace declarations.
2. Normalized package aliases, registry/Git/path sources, default features,
   explicit features, and version requirements.
3. Reports new edges and expanded dependency surface while ignoring
   version-only changes, including unchanged manifest renames.
4. Added policy dispatch and finding properties shared by JSON and SARIF.
5. Added registry, Git, path, target-specific, workspace-inherited,
   malformed-manifest, rename, version-only, feature-expansion, severity, and
   suppression coverage.

### Validation

Passed on 2026-09-12:

1. `cargo fmt --check`
2. `cargo check`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test` (40 tests)
5. `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps`
6. `git diff --check`

### Next phase: P5.4

1. Produce the calibration report for each enabled introduced-debt rule.

## P5.4 — Introduced-debt calibration report (complete)

Date: 2026-09-12

### Completed

1. Added [`docs/CALIBRATION-REPORT.md`](docs/CALIBRATION-REPORT.md) with
   pinned tool/repository metadata, 30-sample mechanical calibration sets for
   each P5 rule, classification counts, reproduction commands, and promotion
   criteria.
2. Kept all introduced-debt rules at warning severity. The report explicitly
   distinguishes synthetic fixture rates from the real-repository evidence
   required before error promotion.

### Validation

Passed on 2026-09-12:

1. `cargo fmt --check`
2. `cargo check`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test` (40 tests)
5. `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps`
6. `git diff --check`
