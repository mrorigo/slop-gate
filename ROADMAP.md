# Slop Gate roadmap

## 0.2 enhancement plan: structural clone matching

Status: **complete**. This phase improves same-language near-clone matching
without claiming semantic equivalence. The implementation remains Rust-only and
uses Tree-sitter syntax trees without type resolution, name resolution, macro
expansion, or control-flow graph construction.

### Product decision

Keep token shingles as the bounded candidate-retrieval index. Add a normalized
AST representation as a second structural signal. Preserve child and statement
order in version 1 of this representation. Reordering arbitrary statements is
not equivalent because Rust expressions can mutate state, move values, run
destructors, or affect borrowing.

Basic-block control-flow graph similarity, type-aware matching, and
order-insensitive statement normalization are deferred until a semantic model
and a false-positive corpus exist.

### P6.0: normalized AST facts

Status: **complete**. Function artifacts now contain versioned normalized
AST-shape hashes, node counts, and five-node AST shingles.

For each extracted Rust function, compute a deterministic AST-shape stream and
its BLAKE3 hash. The stream must:

- preserve Tree-sitter node kind and child order;
- normalize identifier and literal leaves to the existing `ID` and `LIT` classes;
- omit comments, attributes, and nested named function bodies;
- retain syntactic operators, control-flow nodes, scopes, and pattern structure;
- use a versioned representation label in the analyzer fingerprint.

Add the AST hash and node count to the artifact schema. Bump the artifact schema
version when the serialized representation changes. Older artifacts must fail
with an actionable version error; no implicit migration is required.

### P6.1: bounded structural scoring

Status: **complete**. Token shingles still retrieve at most `64` candidates.
Candidates must pass both token and AST similarity thresholds.

Retain the existing sorted token-shingle index and candidate limit of `64`.
Compute normalized AST five-node shingle sets only for retrieved candidates.
For each candidate, record both token-shingle Jaccard and AST-shingle Jaccard.
An exact normalized AST hash is a structural match with similarity `1.0`.

Do not change the default gate decision until calibration demonstrates that the
AST signal improves recall without exceeding the project false-positive limit.
The selected policy must define whether a finding requires the token score, the
AST score, or both. That decision must be represented in the policy fingerprint.

### P6.2: fixtures and rollout

Status: **complete**. Fixture coverage and calibration evidence are recorded in
[`docs/AST-CALIBRATION-REPORT.md`](docs/AST-CALIBRATION-REPORT.md).

Add fixture-driven coverage for:

- identifier and literal renames that preserve AST shape;
- equivalent syntax with different formatting and comments;
- changed nesting, match patterns, and control-flow structure;
- reordered statements that must not be treated as equivalent by default;
- binding-scope changes that alter AST scope structure;
- nested functions, closures, macros, and attributes;
- deterministic artifact and report output across repeated runs.

Run the AST matcher in warning mode during calibration. Record at least `30`
sampled findings and their false-positive classification. Promote the signal to
an error-capable gate only when the measured false-positive rate is at most
`5%` and the benchmark remains within the performance budget.

### 0.2 acceptance criteria

| Constraint | Criterion |
| --- | --- |
| Compatibility | P5 artifacts are rejected clearly; P6 artifacts identify the AST representation version. |
| Determinism | Identical revisions, policy, and artifact produce byte-identical JSON and SARIF. |
| Safety | No AST rule treats arbitrary statement reordering as equivalent by default. |
| Bounded work | Candidate retrieval remains limited to `64` functions per subject; unchanged P5 indexing limits remain intact. |
| Performance | P6 adds no more than `100 ms` p95 to the fixed P5 benchmark in `docs/SLOP-GATE-PLAN.md`. |
| Rollout quality | At least `30` sampled warnings and a false-positive rate at or below `5%` are documented before error-level promotion. |

### Deferred from 0.2

1. Type-aware clone matching that requires compiler metadata or name resolution.
2. Basic-block control-flow graph similarity.
3. General commutativity or statement-reordering rules.
4. Cross-language structural matching.

## 0.3 enhancement plan: exploratory repository scanning

Status: **complete**. This phase makes `scan` useful for local code-health
exploration while preserving immutable revision semantics for `check` and
artifact compatibility.

### Product decisions

1. `scan` defaults to `HEAD`; `--ref <commit>` remains available.
2. `--working-tree` includes untracked Rust files and respects `.gitignore` by
   default. `--no-ignore` includes ignored files.
3. `--path` accepts files and directories and may be repeated.
4. Exploratory thresholds do not affect artifact or policy fingerprints.
5. Clone-pair findings are accompanied by deterministic clone-family summaries.
6. Warning findings exit `0`; unsuppressed error findings exit `1`; operational
   failures exit `2`, including for SARIF output.
7. Duplicate mass counts each function once per clone family.

### P7.0: scan controls

Status: **complete**. Add default revision selection, working-tree source
collection, path filters, threshold overrides, and top-N pair limiting.

### P7.1: clone-family reporting

Emit pair findings with stable `clone_family_id` properties and one
`finding_kind = "family-summary"` finding per connected family. Summary
properties include `member_count` and `duplicate_mass`. Keep the existing
`near-clone` rule ID.

### P7.2: local report contract

Document human, JSON, and SARIF exploratory output. Record effective scan
thresholds in machine-readable output and preserve byte-identical output for
identical inputs.

### 0.3 acceptance criteria

| Constraint | Criterion |
| --- | --- |
| Default use | `slop-gate scan` analyzes `HEAD`. |
| Working tree | Untracked non-ignored Rust files are included; ignored files require `--no-ignore`. |
| Filtering | Files and directories produce the same path-normalized selection. |
| Ranking | `--top N` limits pair findings to exactly N while retaining relevant family summaries. |
| Determinism | Identical inputs and options produce byte-identical JSON and SARIF. |
| Compatibility | P5/P6 `check` behavior and artifact fingerprints remain unchanged. |

## 0.3.2 maintenance fix: optional base blobs

Status: **implementation complete; release pending**.

When `check` compares a changed head path with its base revision, an absent
base path is a normal state for newly added Rust files and manifests. The Git
boundary must return `None` for Git's explicit missing-path diagnostics. It
must preserve operational errors for invalid revisions and unrelated Git
failures.

### Acceptance criteria

- A new Rust file produces no optional-blob operational error.
- A new `Cargo.toml` produces `new-dependency` findings when it adds a direct
  dependency.
- A missing path in a valid revision returns `None` from the optional-blob
  boundary.
- Invalid revisions and unrelated Git failures remain errors with exit code
  `2`.
- The exact Warmplane scenario has an automated temporary-repository test.
- Existing rename, delete, malformed-source, and artifact tests remain green.

## 0.3.1 enhancement plan: adopter experience

Status: **implementation complete; release verification pending**. This
maintenance release addresses the integration
friction reported by an external Rust project. It must not change the default
analysis rules or the `check` artifact contract.

### Product decisions

1. Consumer workflows must invoke an installed `slop-gate` binary. They must
   not use `cargo run` unless Slop Gate is part of the consumer workspace.
2. Release archives must use one documented naming scheme for every supported
   target. Unsupported targets must be documented rather than implied.
3. `cargo-binstall` metadata may point to GitHub release archives. It must use
   the same target names and version tags as the release workflow.
4. `init` writes only the policy file. It must not overwrite an existing file
   unless the user passes an explicit force option.
5. Existing exact-location suppressions remain valid when broader suppression
   matching is added.

### P8.0: consumer workflow and installation guidance

Status: **complete locally**.

Publish a ready-to-copy workflow for repositories that install Slop Gate from
GitHub Releases or crates.io. The workflow must create an index from the base
revision, run `check` against the head revision, and upload SARIF without
assuming the consumer repository contains Slop Gate source code.

Add installation guidance for `cargo install --locked`, prebuilt release
archives, and `cargo-binstall` when metadata is available. Include the required
permissions, artifact directory creation, and the behavior for missing base
artifacts.

Acceptance criteria:

- A workflow copied into a repository without a Slop Gate dependency can run
  `index` and `check` successfully.
- The workflow does not invoke `cargo run` for Slop Gate.
- The documented download URL is tested against a published release archive.
- SARIF upload remains optional and does not hide a gate failure.

### P8.1: release target and installer contract

Status: **implementation complete; release verification pending**.

Make the release matrix and installer metadata agree on supported targets and
archive names. Version 0.3.x supports x86_64 and aarch64 Linux `gnu`, x86_64
and aarch64 Linux `musl`, macOS ARM, and Windows x86_64. Do not advertise an
archive that the workflow does not produce.

Add `[package.metadata.binstall]` only after the archive URL and target mapping
are tested. Document the minimum supported host targets and the fallback to
Cargo compilation.

Acceptance criteria:

- Every documented archive URL resolves to an archive containing the
  `slop-gate` executable.
- Each archive name contains the release version and Rust target triple.
- `cargo binstall slop-gate` resolves the expected archive for each supported
  host target.
- A release with a missing matrix artifact fails before publication.

### P8.2: policy scaffolding command

Status: **complete locally**.

Add `slop-gate init` to create a commented, schema-valid `.slop-gate.toml`.
The generated file must show the default severity, thresholds, and suppression
syntax without enabling stricter policy values.

Acceptance criteria:

- `init` creates the file in the repository root.
- Running `init` again fails without changing the existing file.
- An explicit force option is required to replace an existing file.
- The generated file passes the same validation used by analysis commands.
- The command reports the output path and returns exit code `2` for operational
  failures.

## 0.4 enhancement plan: configurable exploration and policy scope

Status: **planned**. These features reduce low-value findings in intentional
test and generated-code regions without weakening diff-aware default gates.

### P9.0: test-aware scan policy

Support an explicit near-clone test policy. The first version should provide a
single documented choice: exclude test code from exploratory near-clone scans.
Test code includes paths selected by a configured test path pattern and Rust
items under `#[cfg(test)]`. The default remains unchanged until calibration
shows that exclusion improves review value.

Do not silently exclude tests from `check`, mass findings, unsafe findings, or
dependency findings. If separate test thresholds are later needed, specify
them as independent policy fields with independent fingerprints.

Acceptance criteria:

- A test path and a `#[cfg(test)]` module can be excluded independently.
- The default configuration produces the current result set.
- Excluded functions do not contribute to clone families or duplicate mass.
- The effective test policy appears in JSON and SARIF properties.
- Policy changes invalidate incompatible index artifacts.

### P9.1: directory-level suppression patterns

Allow validated glob patterns in suppression paths, such as `tests/**`, while
retaining exact path and line suppression. Define matching against normalized
repository-relative paths. Reject malformed patterns and patterns that escape
the repository path model.

Acceptance criteria:

- A matching directory pattern suppresses findings below that directory.
- An exact path suppression continues to take precedence without changing its
  behavior.
- Pattern matching is deterministic across platforms.
- Suppression patterns are included in the policy fingerprint.
- Human, JSON, and SARIF reports identify that a finding was suppressed only
  through the existing suppression accounting contract.

### P9.2: official GitHub Action evaluation

Evaluate a maintained composite action only after the workflow and installer
contracts stabilize. The action would select or download a compatible binary,
create the base artifact, run the requested gate, and optionally upload SARIF.
It must not conceal the underlying command, revision, policy, or exit status.

Admission criteria:

- The copy-and-run workflow from P8.0 is stable for one release cycle.
- The action has integration tests for installation, cache misses, cache hits,
  missing artifacts, and SARIF upload failures.
- Maintenance ownership and supported action runtime versions are documented.

### 0.3.1 and 0.4 validation

| Constraint | Criterion |
| --- | --- |
| Consumer compatibility | A clean consumer repository can install and run Slop Gate without adding it to its Cargo workspace. |
| Release integrity | Published archive URLs, checksums, target triples, and installer metadata agree byte-for-byte where applicable; the release version is present in the URL or archive name. |
| Safe initialization | `init` never overwrites policy without an explicit force option. |
| Scope precision | Test and glob exclusions affect only the documented finding populations. |
| Compatibility | Existing Rust analysis, `check`, artifact validation, and default scan behavior remain unchanged unless policy opts in. |
| Determinism | Repeated runs with identical revisions, policy, paths, and options produce byte-identical machine-readable output. |

## Later candidates

These items remain outside the current 0.3.2 and 0.4 scope.

| Candidate | Why it is deferred | Admission condition |
| --- | --- | --- |
| `policy-drift` | It requires a repository-specific configuration/data-flow contract and is not broadly useful as a default rule. | Define an explicit repository-contract feature separate from default code gates. |
| `error-context-loss` | Error transport conventions vary by architecture; a generic matcher would be noisy. | Define a reusable boundary-annotation contract and collect a 30-finding corpus across unrelated projects. |
| Clippy baseline/delta adapter | Clippy owns local lint semantics; Slop Gate could later gate only newly introduced configured lint violations. | Define Clippy JSON input, toolchain-version compatibility, and rule identity stability. |
| Public API contract gaps | Docs, examples, error contracts, and semver are not uniformly inferable from syntax. | Specify supported public API surfaces and documentation completeness metrics. |
| CI exit-contract gaps | Command integration tests require a supported test runner and CI-system model. | Define supported CI providers and a minimum executable contract-test format. |
