# Handoff

Date: 2026-09-12

## 0.1.0 baseline

Slop Gate is a deterministic, repository-aware Rust code-quality gate. It
analyzes immutable Git revisions and does not inspect dirty working-tree edits.

The CLI provides:

- `index --ref <commit> --output <artifact>` to create a baseline artifact;
- `check --base <commit> --head <commit> --index <artifact>` to evaluate
  introduced findings;
- `scan --ref <commit>` to audit one revision for near-clones.

Reports support `human`, `json`, and `sarif` formats. Exit status `0` means no
error-severity findings, `1` means at least one error-severity finding, and `2`
means an operational failure.

The supported language is Rust. The baseline rules are:

- `function-mass` for oversized new or materially expanded functions;
- `near-clone` for bounded exact-Jaccard structural duplicates;
- `lint-suppression-growth` for newly added or broadened `allow` and `expect`;
- `unsafe-surface-growth` for newly introduced unsafe constructs;
- `dependency-surface-growth` for new or expanded direct production/build
  dependencies.

The missing `.slop-gate.toml` policy uses warning-only defaults. Unknown keys
and invalid values are operational errors. Findings can be disabled, reported
as warnings, or promoted to errors. Exact path and optional line suppressions
require a reason.

Artifacts are JSON, contain analysis facts only, and are bound to the exact
repository commit, analyzer rules, and active policy. Rebuild an artifact when
any of those inputs changes. Artifacts are stored under `.slop-gate/`, which is
ignored by Git.

An agent adoption guide is available at `.skills/slop-gate/SKILL.md`.

## Validation

The current baseline passes:

1. `cargo check`
2. `cargo fmt --check`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test` — 42 tests
5. `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps`
6. `cargo audit`
7. `git diff --check`

The manifest boundary also rejects structurally invalid dependency tables and
handles valid TOML dependency declarations with optional whitespace.

## 0.2 next phase

The next planned work is normalized AST structural matching:

1. Add versioned normalized AST-shape facts and hashes to function artifacts.
2. Retain token shingles for bounded candidate retrieval.
3. Add AST-shingle similarity as a second signal.
4. Preserve statement order and avoid semantic-equivalence claims.
5. Calibrate with at least 30 findings and a false-positive rate at or below 5%.

Type-aware matching, control-flow graph similarity, arbitrary statement
reordering, and cross-language matching remain deferred.

## Release state

The Cargo package is `slop-gate` version `0.1.0`. The local `main` history has a
Slop Gate 0.1.0 root commit followed by the roadmap and hardening follow-up.
The remote state must be checked before any force-push or tag operation.
