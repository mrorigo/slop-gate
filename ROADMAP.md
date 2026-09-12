# Slop Gate roadmap

## Lexical audit (glossary)

| Term | Definition |
| --- | --- |
| **in-scope Rust file** | A Git-tracked `*.rs` path selected by `[scope].include` and `[scope].exclude`. Defaults are `include = ["**/*.rs"]` and `exclude = []`; no source-layout convention is implied. |
| **added line** | A one-based head-revision line in a `+` hunk line from `git diff --unified=0 BASE HEAD -- <path>`. |
| **head fact** | A deterministic observation from the head revision with rule ID, path, line span, normalized identity, and properties. |
| **introduced fact** | A head fact whose primary syntax token overlaps an added line, or a manifest dependency edge absent from the base revision. |
| **lint suppression** | A Rust inner or outer attribute containing `allow`, `expect`, or `cfg_attr(..., allow(...))`. |
| **unsafe surface** | An unsafe block, unsafe function, unsafe trait, unsafe implementation, or unsafe extern block. |
| **direct dependency edge** | One dependency declaration in a package `Cargo.toml` under `dependencies`, `build-dependencies`, or a target-specific equivalent. Dev dependencies and workspace-only declarations are excluded in version 1. |

The rule names are immutable: `lint-suppression-growth`,
`unsafe-surface-growth`, and `dependency-surface-growth`.

## Logic and state translation

### Product boundary

| Rule | Slop Gate responsibility | Existing tool boundary |
| --- | --- | --- |
| `lint-suppression-growth` | Report a newly added or broadened compiler/Clippy lint exemption. | Rustc and Clippy honor an exemption but do not identify its introduction in a pull request. |
| `unsafe-surface-growth` | Report a newly introduced unsafe construct. | The compiler enforces unsafe syntax, but does not make new unsafe surface a repository policy decision. |
| `dependency-surface-growth` | Report a newly introduced direct production/build dependency or enabled dependency feature. | Cargo Audit reports known advisories; it does not report the introduction of maintenance or supply-chain surface. |

All rules use Git commit revisions and never a dirty working tree. They apply
to every in-scope Rust file or tracked package manifest, independent of whether
the repository uses `src/`, `crates/`, a custom Cargo target path, or another
layout. Each defaults to `warn`; calibration is required before `error`.

### Shared prerequisite: added-line facts

Status: **complete** (P5.0, 2026-09-12).

P5.0 adds `GitRepository::added_lines(base, head, path)`. It executes
`git diff --unified=0 --no-ext-diff BASE HEAD -- <path>` and accepts only hunk
headers of the form `@@ -old_start,old_count +new_start,new_count @@`.
It also returns changed `Cargo.toml` head paths and rename mappings. A manifest
added in head has an empty base edge set; a renamed manifest compares its head
edges with its old base path.

| Diff condition | Added-line result |
| --- | --- |
| New file | Every head line. |
| Modified file | Union of non-empty head hunk ranges. |
| Deleted file | Empty set. |
| Rename without content change | Empty set. |
| Rename with content change | Added ranges under the head path. |
| Malformed hunk | Operational error, exit 2, no partial report. |

Rust rules parse each changed in-scope head file and retain a fact only when
its primary syntax token intersects the file's added-line set. Manifest rules
compare normalized base/head dependency edges, so an added edge is introduced
even when its TOML declaration shares a line with unrelated changes.

### `lint-suppression-growth`

Status: **complete** (P5.1, 2026-09-12).

P5.1 extracts every Rust `attribute_item` or inner attribute whose normalized
form is one of:

| Pattern ID | Exact match |
| --- | --- |
| `allow` | `#[allow(lint_path, ...)]` or `#![allow(lint_path, ...)]` |
| `expect` | `#[expect(lint_path, ...)]` or `#![expect(lint_path, ...)]` |
| `cfg-allow` | `#[cfg_attr(condition, allow(lint_path, ...))]` or inner equivalent |

The fact identity is `(attribute_kind, sorted lint paths, normalized target
fingerprint)`, where the target fingerprint excludes attributes, comments, and
whitespace. A head fact is a finding when its identity is absent from the base
file, or when the lint-path set on an equal target fingerprint is a strict
superset of the base set. Reformatting and relocating an unchanged attribute do
not produce a finding. `deny`, `warn`, `forbid`, and non-lint attributes do not
match.

```gherkin
Given a changed Rust item gains #[allow(clippy::unwrap_used)]
When check evaluates lint-suppression-growth
Then it emits one finding on the allow attribute

Given #[allow(dead_code)] changes only formatting
When check evaluates lint-suppression-growth
Then it emits no finding

Given #[allow(clippy::panic, clippy::unwrap_used)] replaces #[allow(clippy::panic)]
When check evaluates lint-suppression-growth
Then it emits one broadened-suppression finding
```

### `unsafe-surface-growth`

Status: **complete** (P5.2, 2026-09-12).

P5.2 extracts these Rust syntax forms from every changed in-scope file:

| Pattern ID | Primary token | Match |
| --- | --- | --- |
| `unsafe-block` | `unsafe` | `unsafe { ... }` |
| `unsafe-function` | `unsafe` | `unsafe fn` declaration |
| `unsafe-trait` | `unsafe` | `unsafe trait` declaration |
| `unsafe-impl` | `unsafe` | `unsafe impl` declaration |
| `unsafe-extern-block` | `unsafe` | `unsafe extern` block |

A finding is emitted only when the primary `unsafe` token overlaps an added
line. Editing code inside a pre-existing unsafe block produces no finding.
Version 1 does not infer whether an unsafe operation is necessary and does not
require a comment format; both are review-policy decisions.

```gherkin
Given a changed Rust file adds unsafe { pointer.read() }
When check evaluates unsafe-surface-growth
Then it emits one unsafe-block finding on the unsafe token

Given a changed line is inside an existing unsafe block
And no unsafe token is added
When check evaluates unsafe-surface-growth
Then it emits no finding
```

### `dependency-surface-growth`

Status: **complete** (P5.3, 2026-09-12).

P5.3 processes every changed Git-tracked file named `Cargo.toml`. It parses
base and head TOML and extracts direct dependency edges from these table paths:

```text
dependencies.<key>
build-dependencies.<key>
target.<selector>.dependencies.<key>
target.<selector>.build-dependencies.<key>
```

Each edge identity is `(manifest_path, table_path, dependency_key)`. Its
properties are `package` when renamed, source kind (`registry`, `git`, `path`,
or unspecified), normalized source locator when present, `default-features`,
sorted `features`, and normalized version requirement when present.

| Base edge | Head edge | Result |
| --- | --- | --- |
| Absent | Present | `new-dependency` finding. |
| Present | Present; only version requirement changes | No finding. |
| Present | Present; one or more features added, `default-features` becomes true, or package/source declaration changes | `expanded-dependency-surface` finding. |
| Present | Absent | No finding. |
| Any | Invalid TOML | Operational error, exit 2. |

`dev-dependencies`, `workspace.dependencies`, lockfiles, and transitive
dependency changes are intentionally excluded in version 1. A package using
`foo.workspace = true` is a direct edge and is reported when newly introduced;
the workspace declaration alone is not.

```gherkin
Given crates/api/Cargo.toml adds [dependencies] serde_json = "1"
When check evaluates dependency-surface-growth
Then it emits one new-dependency finding

Given an existing dependency gains features = ["derive"]
When check evaluates dependency-surface-growth
Then it emits one expanded-dependency-surface finding

Given crates/api/Cargo.toml is renamed without changing its dependency edges
When check evaluates dependency-surface-growth
Then it emits no finding

Given only a dependency version requirement changes
When check evaluates dependency-surface-growth
Then it emits no finding
```

### Rule policy and reporting

P5.4 extends the existing TOML schema and rule dispatch:

| Rule | Default | Suppression identity | Finding properties |
| --- | --- | --- | --- |
| `lint-suppression-growth` | `warn` | Exact path and line | `pattern_id`, `lint_paths`, `target_kind` |
| `unsafe-surface-growth` | `warn` | Exact path and line | `pattern_id` |
| `dependency-surface-growth` | `warn` | Exact manifest path and declaration line | `pattern_id`, `dependency_key`, `table_path`, edge properties |

All findings use the existing human, JSON, and SARIF renderers. Suppression
occurs before severity evaluation. Findings are sorted by path, line, rule ID,
and normalized identity.

### Delivery plan

| Phase | Deliverable | Completion evidence |
| --- | --- | --- |
| P5.0 | Added-line parser and `ChangedLineSet`; no user-visible rule. **Complete 2026-09-12.** | Unit coverage for new, modified, deleted, renamed, malformed, and zero-length hunks. |
| P5.1 | `lint-suppression-growth` extractor and policy. **Complete 2026-09-12.** | New, broadened, unchanged, inner, outer, conditional, suppression, warn, error, JSON, and SARIF fixtures. |
| P5.2 | `unsafe-surface-growth` extractor and policy. **Complete 2026-09-12.** | All five syntax forms, edits inside existing unsafe blocks, suppression, warn, error, JSON, and SARIF fixtures. |
| P5.3 | `dependency-surface-growth` manifest parser and policy. **Complete 2026-09-12.** | Registry, Git, path, renamed, target-specific, workspace-inherited, feature-expansion, version-only, malformed-TOML, suppression, warn, error, JSON, and SARIF fixtures. |
| P5.4 | Calibration report for each enabled rule. **Complete 2026-09-12.** | At least 30 sampled warning findings per rule before promotion to `error`. |

## 0.2 enhancement plan: structural clone matching

Status: **planned**. This phase improves same-language near-clone matching
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

Retain the existing sorted token-shingle index and candidate limit of `64`.
Compute normalized AST five-node shingle sets only for retrieved candidates.
For each candidate, record both token-shingle Jaccard and AST-shingle Jaccard.
An exact normalized AST hash is a structural match with similarity `1.0`.

Do not change the default gate decision until calibration demonstrates that the
AST signal improves recall without exceeding the project false-positive limit.
The selected policy must define whether a finding requires the token score, the
AST score, or both. That decision must be represented in the policy fingerprint.

### P6.2: fixtures and rollout

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

## Quantification of constraints

| Qualitative term | Measurable acceptance criterion |
| --- | --- |
| **No legacy-debt noise** | Every Rust finding has an added primary token. Every dependency finding has an edge absent from base or an explicitly defined surface expansion. |
| **Layout-neutral** | Fixtures cover a root package, `crates/core`, an explicit custom Cargo target path, and an excluded path. Results follow scope configuration without `src/` special-casing. |
| **Deterministic** | Two runs with identical base, head, configuration, and artifact produce byte-identical JSON and SARIF output. |
| **Bounded** | Each changed in-scope Rust file is parsed once per Rust rule. Each changed `Cargo.toml` is parsed once per revision; no dependency resolution or network access occurs. |
| **Fast** | On a fixed 20-file diff with 2,000 in-scope Rust files and 50 changed manifests, P5 adds at most 250 ms p95 to `check` on the benchmark runner in `docs/SLOP-GATE-PLAN.md`. |
| **Low false-positive rollout** | Each warning-enabled rule has at least 30 sampled findings; promotion to `error` requires a documented false-positive rate at or below 5%. |

## Inquisition (decisions required before implementation)

1. Should `#[expect(...)]` be treated identically to `#[allow(...)]`? Proposed answer: **yes**; both locally weaken the normal diagnostic contract.
2. Should generated Rust files be included by default when they are Git-tracked? Proposed answer: **yes**; repositories exclude them explicitly through `[scope].exclude`.
3. Should unsafe functions, traits, implementations, and extern blocks have the same severity as unsafe blocks? Proposed answer: **yes** in version 1.
4. Should a new direct `path` dependency be a dependency-surface finding? Proposed answer: **yes**; it adds package coupling even without third-party supply-chain risk.
5. Should enabling a dependency's default features count as expansion when feature names are not explicit? Proposed answer: **yes**; `default-features = false` to true expands the resolved surface.

## Deferred candidates

| Candidate | Why it is deferred | Admission condition |
| --- | --- | --- |
| `policy-drift` | It requires a repository-specific configuration/data-flow contract and is not broadly useful as a default rule. | Define an explicit repository-contract feature separate from default code gates. |
| `error-context-loss` | Error transport conventions vary by architecture; a generic matcher would be noisy. | Define a reusable boundary-annotation contract and collect a 30-finding corpus across unrelated projects. |
| Clippy baseline/delta adapter | Clippy owns local lint semantics; Slop Gate could later gate only newly introduced configured lint violations. | Define Clippy JSON input, toolchain-version compatibility, and rule identity stability. |
| Public API contract gaps | Docs, examples, error contracts, and semver are not uniformly inferable from syntax. | Specify supported public API surfaces and documentation completeness metrics. |
| CI exit-contract gaps | Command integration tests require a supported test runner and CI-system model. | Define supported CI providers and a minimum executable contract-test format. |
