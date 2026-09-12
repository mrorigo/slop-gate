# Slop Gate roadmap

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

## Later candidates

These items remain outside the 0.2 scope.

| Candidate | Why it is deferred | Admission condition |
| --- | --- | --- |
| `policy-drift` | It requires a repository-specific configuration/data-flow contract and is not broadly useful as a default rule. | Define an explicit repository-contract feature separate from default code gates. |
| `error-context-loss` | Error transport conventions vary by architecture; a generic matcher would be noisy. | Define a reusable boundary-annotation contract and collect a 30-finding corpus across unrelated projects. |
| Clippy baseline/delta adapter | Clippy owns local lint semantics; Slop Gate could later gate only newly introduced configured lint violations. | Define Clippy JSON input, toolchain-version compatibility, and rule identity stability. |
| Public API contract gaps | Docs, examples, error contracts, and semver are not uniformly inferable from syntax. | Specify supported public API surfaces and documentation completeness metrics. |
| CI exit-contract gaps | Command integration tests require a supported test runner and CI-system model. | Define supported CI providers and a minimum executable contract-test format. |
