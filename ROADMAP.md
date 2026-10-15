# Slop Gate roadmap

This roadmap describes the current release and open product work. Completed
plans from earlier versions are summarized under [Delivered work](#delivered-work).

## Release 0.7.0

Status: **release preparation**. The release branch contains the version bump
and documentation updates. Do not publish until the branch is merged to
`main`; the release workflow requires the tag to point to a commit reachable
from `main`.

### Release contents

The main addition since 0.6.0 is the typed snapshot analysis API. Callers can
parse policy bytes, build and reuse an in-memory index from supplied snapshots,
evaluate an uncommitted candidate, and inspect findings and per-rule coverage.
The API accepts cancellation and resource limits, reports incomplete analysis,
and does not discover a repository or perform network or process I/O.

The release also includes an example that evaluates caller-supplied source
bytes. Existing command-line analysis and artifact behavior remain available.
The API surface was added without changing the existing serialized baseline
artifact contract.

### Release checks

1. Confirm the version is `0.7.0` in `Cargo.toml`, `Cargo.lock`, and the site.
2. Run formatting, lint, test, and example checks from `AGENTS.md`.
3. Build and inspect the crate package with `cargo package --locked`.
4. Merge the release commit to `main` before creating tag `v0.7.0`.
5. Confirm the release workflow builds all six supported target archives and
   their checksums before publication.

## Open work

### Snapshot similarity queries

The snapshot API supports source records and change evaluation. A follow-up
could expose bounded structural similarity queries for an existing function or
a supplied source fragment. Results must include source identity and similarity
evidence. They must not claim semantic equivalence. Keep this work separate
from core snapshot evaluation.

### Calibration

Python and TypeScript extraction, fixtures, and same-language clone matching
are implemented. Keep their findings at warning severity until calibration on
real repositories supports stronger defaults. The structural-erosion report
also records preliminary evidence rather than a threshold decision.

### Macro-aware clone mass

An adopter reported that moving repeated match arms into a macro reduced the
reported recoverable mass without removing the duplicated logic. Add a fixture
for this case and evaluate macro definitions and repeated arms as clone-mass
candidates. Preserve current findings and severities until evidence supports a
behavior change.

### Policy and workflow candidates

The earlier 0.4 plan is no longer a release plan. Reassess these candidates
before scheduling implementation:

- Exclude test code from exploratory near-clone scans through an explicit,
  fingerprinted policy option.
- Support validated directory patterns in finding suppressions.
- Evaluate a maintained GitHub Action after the copyable workflow and installer
  contracts have remained stable for a release cycle.

These changes must not weaken default diff-aware checks or alter unrelated rule
populations.

## Delivered work

| Version | Delivered work |
| --- | --- |
| 0.2 | Versioned normalized AST facts and bounded structural clone scoring. |
| 0.3 | Exploratory scans, clone-family summaries, working-tree inputs, and report controls. |
| 0.3.1–0.3.3 | Consumer installation guidance, release targets, `init`, optional base blobs, and moved-function clone handling. |
| 0.4.0 | Structural-erosion analysis. |
| 0.4.1 | Rust parser maintenance. |
| 0.5.0 | Block-level clone detection. |
| 0.6.0 | Python and TypeScript analysis. |
| 0.7.0 | Typed snapshot analysis API with explicit coverage and resource controls. |

Earlier calibration reports, feature plans, and acceptance records remain in
`docs/`. Their measurements and implementation details remain useful, but the
plans in those documents do not define the current release scope.

## Deferred candidates

| Candidate | Admission condition |
| --- | --- |
| Type-aware clone matching | Define a semantic model and a measured false-positive corpus. |
| Control-flow graph matching | Demonstrate value beyond the current syntax signals and bound its cost. |
| Cross-language clone matching | Define a useful representation and establish precision on real repositories. |
| `policy-drift` | Specify repository contracts and data-flow semantics separately from default code gates. |
| `error-context-loss` | Define reusable boundary annotations and collect findings across unrelated projects. |
| Clippy baseline adapter | Specify stable rule identity and toolchain compatibility for Clippy JSON input. |
| Public API contract gaps | Define supported API surfaces and measurable documentation requirements. |
| CI exit-contract gaps | Select supported CI providers and a portable executable contract format. |
