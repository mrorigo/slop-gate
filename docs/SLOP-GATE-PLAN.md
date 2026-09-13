# Slop Gate: Specification and Transformation Plan

## Decision record

This document defines the target for the `semble-rs` fork. It is deliberately
more constrained than the initial idea: version 1 is a deterministic,
repository-aware CI gate for structural duplication and function growth. It is
not an AI judge, formatter, general linter, or a claim that semantic similarity
proves duplicated behaviour.

The product is named `slop-gate`; the package, library, binary, cache
namespace, documentation, and public diagnostics will be renamed from
`semble-rs`/`semble` as part of the first implementation milestone. Compatibility
aliases are out of scope unless an adoption constraint requires them.

### Product outcome

Given a Git base revision and a head revision, `slop-gate check` must inspect
only changed, supported source files, compare new or changed functions with a
baseline repository artifact, and return a process status plus machine-readable
diagnostics. A diagnostic must identify the rule, the introduced code location,
the compared baseline location where applicable, the measured values, and the
configured threshold that caused it.

The initial gate rules are:

1. `function-mass`: blocks a newly introduced function whose mass exceeds a
   configured limit, or an existing function whose mass increase exceeds a
   configured delta limit.
2. `near-clone`: blocks a new or materially changed function structurally
   similar to a different function already present in the base artifact, or to
   an earlier changed function in the same head.

The default disposition is important: a rule is only enforced when configured
as an error. The shipped starter configuration must set both rules to `warn`.
This avoids declaring an uncalibrated heuristic an immediate release blocker.

### Explicit non-goals for version 1

1. Inferring intent from embeddings, docstrings, or LLM calls.
2. Proving semantic equivalence, correctness, or security.
3. Cross-language clone detection.
4. Whole-repository historical erosion scoring.
5. Replacing Clippy, ESLint, a formatter, or an SAST scanner.
6. Network access at check time.
7. MCP server support. The search MCP code is a legacy substrate, not a gate
   requirement.

## Verified substrate and consequences

| Verified repository fact | Source | Design consequence |
| --- | --- | --- |
| The crate is still `semble-rs` and its primary CLI is `semble`. | `Cargo.toml`, `src/cli.rs` | Rename is a real breaking transformation, not a documentation edit. |
| Tree-sitter 0.26 is present for Rust, Python, JS/TS, Java, Go, C/C++, and Ruby. | `Cargo.toml`, `src/chunking/tree_sitter.rs` | Reuse parsers and language dispatch; add function extraction separate from search chunking. |
| Current AST logic extracts selected declarations but chunks arbitrary structural regions. | `src/chunking/tree_sitter.rs`, `src/chunking/core.rs` | Search `Chunk` is not a function record and must not become the gate schema. |
| A persistent cache reuses chunks and embeddings per changed file. | `src/index/create.rs`, `src/index/persist.rs` | The gate can reuse walker/ignore knowledge, but needs its own portable baseline artifact. |
| Existing lexical/dense indexes use positional chunk IDs and Model2Vec may download/fall back. | `src/index/{sparse,dense,model}.rs` | Do not make existing BM25 or embeddings a correctness dependency of a CI gate. |
| Git interaction currently shells out for cloning only. | `src/index/engine.rs` | Use the installed `git` executable for revision resolution and blob reads; do not add `git2` for v1. |

The existing incremental search cache is local-machine state under
`~/.semble/index`; it is not an artifact a CI job can safely consume as a
baseline. It combines model-specific embeddings with mutable cache semantics.
`slop-gate` needs a versioned, self-describing artifact whose compatibility is
checked before evaluation.

## Lexical audit (glossary)

| Term | Immutable definition |
| --- | --- |
| **base** | Git commit used as the pre-change repository state. `BASE...HEAD` is the default comparison range. |
| **head** | Git commit or working-tree state under review. In `check`, the default is `HEAD`; a dirty working tree is included only with an explicit option. |
| **artifact** | Serialized `IndexArtifact` built from exactly one repository commit and one analyzer/configuration version. |
| **supported file** | A non-generated, UTF-8 source file accepted by the configured language and ignore policy. |
| **function record** | One named function, method, associated function, or configured anonymous closure, extracted from a concrete syntax tree. It is not a search chunk. |
| **function identity** | `(language, repo-relative path, qualified declaration path, declaration kind)`. It is a matching hint, not proof of rename continuity. |
| **new function** | A head function with no base function sharing its identity. |
| **changed function** | A head function sharing base identity whose normalized structural hash differs. |
| **mass** | `cyclomatic_complexity × √physical_sloc`; both inputs are non-negative integers and mass is serialized as a finite decimal-compatible `f64`. |
| **physical SLOC** | Count of non-blank lines within the function byte range after lines occupied solely by Tree-sitter comment nodes are excluded. A line containing code and a comment counts once. |
| **cyclomatic complexity (CC)** | `1 +` the count of configured decision AST nodes in the function body, excluding nested named functions and closures. |
| **normalized token stream** | Ordered syntax tokens with comments and whitespace removed; identifiers become `ID`; string, numeric, and character literals become `LIT`; punctuation and keywords are retained. |
| **near clone** | Same-language functions whose token and normalized-AST shingle Jaccard scores and length criteria meet the configured rule. It is evidence of structural duplication, not semantic equivalence. |
| **material change** | A function whose normalized structural hash changes. Formatting/comment-only changes are therefore not material. |
| **violation** | A finding whose configured severity is `error`; warnings are reported but do not make `check` fail. |

Terms intentionally not used as requirements: “semantic overlap,” “utility
namespace,” “slop,” “fast,” and “slightly different.” They are too ambiguous
to gate a build without a separately specified rule.

## Configuration and artifact contract

The repository config file is `.slop-gate.toml`. Missing configuration loads
the shipped warning-only defaults. Unknown keys and invalid values are hard
configuration errors (exit code 2), so typos never silently weaken a gate.

```toml
version = 1

[scope]
languages = ["rust", "python", "typescript", "javascript", "go"]
exclude = ["**/generated/**", "**/*.generated.*"]
include_closures = false

[rules.function-mass]
severity = "warn"                 # off | warn | error
new_function_limit = 80.0
delta_limit = 20.0

[rules.near-clone]
severity = "warn"
minimum_sloc = 8
minimum_tokens = 40
shingle_size = 5
similarity_threshold = 0.85        # 0.0 <= threshold <= 1.0
max_candidates = 64
```

`IndexArtifact` is JSON for v2, compressed only by the CI transport layer. It
contains no source content and has this logical schema:

```text
artifact_version: u32
tool_version: String
analyzer_fingerprint: BLAKE3(config canonical form + language-rule versions)
repository_commit: 40-or-64 hex object id
files: [
  { path, language, content_hash, functions: [FunctionRecord] }
]
FunctionRecord:
  { identity, name, declaration_kind, start_line, end_line,
    normalized_hash, ast_hash, ast_node_count, token_count, sloc, cc, mass,
    shingle_hashes: sorted unique u64[],
    ast_shingle_hashes: sorted unique u64[] }
```

The artifact must be rejected (exit code 2) if its version is unsupported, its
analyzer fingerprint differs, its repository commit does not equal `--base`,
or its content fails deserialization/validation. `check --build-base` is an
explicit local convenience that builds the base artifact from Git blobs; CI
must normally download the main-branch artifact produced by `index`.

## Logic and state translation

### Required commands

```text
slop-gate index --ref <commit> --output <path>
slop-gate check --base <commit> --head <commit> --index <path> [--format human|json|sarif]
slop-gate scan --ref <commit> [--format human|json|sarif]
```

`index` reads every supported file at `<commit>` through `git show` (or the
checked-out tree only when `--ref HEAD` resolves to it), writes one artifact,
and exits 0 on success. `scan` has no CI semantics: it evaluates every
function against every other eligible function in one revision and reports
findings without a base/head delta.

### BDD scenarios

**Base-artifact validation**

```gherkin
Given an artifact built for commit B and the active analyzer fingerprint F
When check is invoked with --base B and fingerprint F
Then evaluation may proceed

Given an artifact built for commit X where X != B
When check is invoked with --base B
Then it exits 2 and emits no rule findings
```

**Mass evaluation**

```gherkin
Given a head function H with no base identity match
When H.mass > function-mass.new_function_limit
Then emit one function-mass finding at H's declaration

Given a head function H matching base function B
And H.normalized_hash != B.normalized_hash
When H.mass - B.mass > function-mass.delta_limit
Then emit one function-mass finding with base_mass, head_mass, and delta

Given H has the same normalized_hash as B
When only whitespace or comments changed
Then emit no function-mass finding for H
```

**Clone evaluation**

```gherkin
Given an eligible new or materially changed head function H
And an eligible comparison function C in the base artifact or an earlier head function
When language(H) = language(C)
And min(token_count(H), token_count(C)) >= minimum_tokens
And min(sloc(H), sloc(C)) >= minimum_sloc
And Jaccard(shingles(H), shingles(C)) >= similarity_threshold
Then emit one near-clone finding locating H and C

Given H and C have the same function identity
When check compares the changed function to its predecessor
Then C is excluded from clone candidates
```

**Git state**

```gherkin
Given --base resolves to no commit in the local clone
When check starts
Then it exits 2 with instructions to fetch the base ref

Given a changed file is deleted in head
When check starts
Then it produces no head finding for that file

Given a changed supported file has a Tree-sitter syntax error
When check starts
Then it emits an analyzer warning, skips rules for that file, and exits 1 only if parse-error severity is error
```

### Rule decision table

| Subject | Baseline identity | Structural change | Rule condition | Result |
| --- | --- | --- | --- | --- |
| Head function | absent | any | mass > new limit | new-mass finding |
| Head function | present | yes | mass delta > delta limit | delta-mass finding |
| Head function | present | no | any | no mass finding |
| Head function | absent/present | material | clone thresholds satisfied | clone finding |
| Head function | absent/present | no | any | excluded from clone evaluation |
| Unsupported/deleted file | n/a | n/a | any | no rule finding; optional scope diagnostic |

## Analysis design

### Function extractor

Create a new `analysis` layer; it must own trees and function records until the
record is complete. Search chunking remains unchanged initially. Per supported
language, define a declarative extractor with:

1. A Tree-sitter query/node predicate for named functions and methods.
2. Parent kinds that establish a qualified scope (module, class, impl, trait,
   namespace where supported).
3. Decision-node kinds used by CC.
4. Comment-node kinds used to calculate SLOC.

The first shipped language is Rust. Python and TypeScript follow only after
their extractor fixtures pass. Treating the current eleven search languages as
gate-ready would create deceptive coverage: JSON, YAML, Markdown, and text
have no function semantics; C/C++ declarator forms require independent tests.

Nested named functions and closures are their own records when enabled. Their
nodes are excluded from their parent function's CC and token stream, preventing
double counting. v1 defaults `include_closures = false`; when disabled,
closures contribute to their enclosing function but are not independently
gated.

### Complexity and fingerprints

For Rust v1, CC increments for `if_expression`, `match_arm` except `_`,
`for_expression`, `while_expression`, `loop_expression`, `&&`, `||`, and
`?`/`try_expression` if the grammar exposes it. The exact per-language node
map is versioned in the analyzer fingerprint and unit-tested from fixtures.
This is a practical, explicit metric—not a language-theoretic universal CC
definition.

Generate the normalized token stream from named and anonymous leaf tokens
inside the selected function body. Do not use raw source tokenization: its
result must be invariant under identifier renaming, literal changes, formatting,
and comments. Hash the stream with BLAKE3. Generate k-token shingles, hash
each with BLAKE3 truncated to `u64`, sort, and deduplicate them. Exact normalized
hash equality may be reported as clone similarity `1.0` without a Jaccard pass.

Artifact schema v2 also stores the normalized AST-shape BLAKE3 hash, node count,
and five-node shingles. The representation preserves node and statement order,
normalizes identifiers and literals, and excludes comments, attributes, and
nested named functions. A clone finding requires both token-shingle and
AST-shingle similarity to meet the configured threshold; this is structural
evidence and not semantic equivalence.

Candidate lookup is an in-memory `HashMap<u64, Vec<FunctionId>>` from shingle
hash to artifact function IDs. For each evaluated function, tally shared
shingles, retain the top `max_candidates`, then calculate exact set Jaccard.
This replaces the idea of using BM25 as a structural proof: BM25 is useful for
search but cannot implement a defined clone threshold.

### Diagnostics and exits

Each finding has stable fields: `rule_id`, `severity`, `message`, `location`,
`properties` (all measured values), and optional `related_location`. Sort by
`path, start_line, rule_id, related_path` before rendering. The human renderer,
JSON renderer, and SARIF 2.1.0 renderer must be projections of this one model.

| Exit | Meaning |
| --- | --- |
| 0 | Analysis completed and no error-severity findings occurred. |
| 1 | Analysis completed and at least one error-severity finding occurred. |
| 2 | Invocation, Git, artifact, configuration, or analyzer failure prevented a trustworthy result. |

Warnings never affect the exit code. SARIF output is written to stdout only;
operational diagnostics go to stderr. This keeps redirection deterministic.

## Quantification of constraints

| Qualitative goal | Required measurable acceptance target |
| --- | --- |
| “Fast CI check” | On the fixed benchmark corpus and a 20-file diff, p95 wall time ≤ 2 s on a 2-core GitHub `ubuntu-latest` runner, excluding artifact download and Git fetch. Record corpus commit, runner, and command in the benchmark report. |
| “Incremental” | `check` reads/parses no unchanged head file, except candidates already represented in the base artifact. It may read base blobs only with explicit `--build-base`. |
| “Repository-scale” | Artifact creation and scan complete without O(N²) pairwise comparisons; candidate evaluation is bounded by `max_candidates` per evaluated function. |
| “Reliable gate” | Every enforced rule has fixture tests covering below/equal/above threshold, a stable diagnostic snapshot, and false-failure input (renamed identifiers, comments, formatting). |
| “Portable artifact” | An artifact generated on macOS is accepted on Linux when tool/analyzer/config versions match. JSON field ordering must not affect loading. |

No numerical default can be universal. The values in the starter config are
calibration placeholders. Before either rule becomes `error` in this repository,
run `scan` on the main branch, sample at least 30 findings per rule, and publish
the selected thresholds plus the observed false-positive rate.

## Transformation plan

### P0 — Rebrand and remove product ambiguity

1. Change package, library, binary, repository metadata, cache/environment
   names, help text, and README from `semble` to `slop-gate`.
2. Delete or move legacy product plans only after links are updated; preserve
   history rather than mixing old and new promises in the root README.
3. Decide whether search/MCP remains a separate crate. Recommended: retain it
   temporarily under `crates/legacy-search`, but do not expose it from the
   default `slop-gate` binary. If it has no committed user, remove it before
   release rather than carrying Model2Vec, `reqwest`, and MCP dependencies into
   a CI gate.
4. Add `--version`, `--help`, and package metadata tests that assert the new
   product name.

**Exit criterion:** `cargo run -- --help` presents only the new product
surface, and a clean build contains no required runtime dependency on a model
download.

### P1 — Deterministic core and Rust reference implementation

1. Add `src/analysis/{mod.rs,extract.rs,metrics.rs,normalize.rs,artifact.rs}`.
2. Implement Rust function extraction, scopes, parse-error reporting, comment
   exclusion, CC, SLOC, normalized hashes, and shingles.
3. Create small checked-in fixture repositories under `tests/fixtures/`;
   include methods, `impl`s, traits, nested closures, comments, malformed
   syntax, and Unicode identifiers.
4. Define the serializable artifact and strict validation. Store relative paths
   with `/` separators; reject absolute and parent-traversal paths.

**Exit criterion:** fixture artifact snapshots are byte-stable on repeated
runs, and metric/normalization tests cover all listed Rust node categories.

### P2 — Git delta evaluator and mass gate

1. Add a small Git boundary module around `git rev-parse`, `git diff
   --name-status`, and `git show <rev>:<path>`; every command failure returns a
   typed, rendered error—never a panic.
2. Implement `index` and artifact/base fingerprint validation.
3. Implement `check` changed-file loading, function identity matching, and
   `function-mass` findings.
4. Implement human and JSON output plus exit-code contract.

**Exit criterion:** an end-to-end temporary Git repository proves unchanged,
new, modified, renamed, deleted, missing-base, stale-artifact, and dirty-tree
behaviour.

### P3 — Near-clone gate and SARIF

1. Build artifact shingle lookup and bounded candidate scoring.
2. Compare base functions and earlier head functions; make ordering stable so
   two added clones generate one deterministic primary/related diagnostic.
3. Add SARIF 2.1.0 projection and validate it against a checked-in schema
   fixture or an independent parser in tests.
4. Add `scan` using the same evaluator with an all-functions subject set.

**Exit criterion:** exact copy, identifier-renamed copy, below-threshold
similar code, same-function predecessor, and same-PR duplicate fixtures all
produce their specified results.

### P4 — Calibrate, add languages, and harden CI

1. Run the tool on representative Rust repositories and tune default warning
   thresholds; record datasets and results.
2. Add Python and TypeScript one at a time, each with its own extraction,
   complexity, and normalization fixtures. Do not inherit Rust rules blindly.
3. Add GitHub Actions examples: main builds/uploads `slop-gate-index.json`;
   pull requests download it, fetch base, and run SARIF upload/check.
4. Add the benchmark corpus and enforce the P0 performance SLI as a non-flaky
   tracked report, not initially as a PR test.

**Exit criterion:** at least one CI workflow using a base artifact succeeds
from a shallow checkout after the documented fetch step.

### P5 — Deliberately deferred semantic advisor

Only after P4 data demonstrates unacceptable false negatives for structural
clones, evaluate an opt-in non-gating `semantic-overlap` advisory. It must have
its own model provenance, offline artifact strategy, privacy statement, and
calibration dataset. It must never silently fall back between models or turn a
network/model failure into a passing gate.

## File and module impact

| Area | Action |
| --- | --- |
| `Cargo.toml`, `src/main.rs`, `src/lib.rs`, `src/cli.rs` | Rebrand; replace search-first command surface with `index`, `check`, and `scan`. |
| `src/analysis/` | New deterministic function extraction, metrics, fingerprint, artifacts, evaluator, and reporters. |
| `src/git.rs` | New narrow, mockable Git command boundary. |
| `src/chunking/tree_sitter.rs` | Reuse parser/language selection, preferably by extracting a parser-provider API; do not couple metrics to chunk boundaries. |
| `src/index/file_walker.rs` | Reuse ignore/supported-file policy after its generated-file decision is documented. |
| `src/index/{dense,sparse,engine,persist}.rs`, `src/mcp.rs`, `src/search.rs` | Move to an explicitly legacy crate/module or remove in P0; they are not on the gate’s critical path. |
| `tests/fixtures/` and integration tests | New authoritative specification examples and Git command integration coverage. |
| `README.md`, `docs/` | Replace search positioning with CI gating use, artifact workflow, configuration reference, and rule limitations. |

## Inquisition (decisions required before implementation)

1. Must v1 preserve the existing Semble search/MCP product in this repository,
   or may we remove/move it and make a clean `slop-gate` binary? **Recommended:
   remove or isolate it before the first Slop Gate release.**
2. Is GitHub Actions the only initial CI target, and can the main-branch
   artifact be stored as an Actions artifact/cache, or must it be portable to
   arbitrary CI from day one? **Recommended: portable file artifact, with
   GitHub Actions as the first documented integration.**
3. Does a parse failure in a changed supported file block merges by default?
   **Recommended: yes (`error`) for Rust once Rust support is declared stable;
   warn during the calibration phase.**
4. Are generated/vendor directories a repository-owned explicit exclusion list,
   or should v1 attempt automatic generated-code detection? **Recommended:
   explicit configuration only; automatic detection is too error-prone for a
   gate.**
5. Are closures first-class gate subjects? **Recommended: no in v1; count them
   inside their parent and introduce independent closure diagnostics only with
   evidence of a need.**
6. Is the intended policy to fail a PR for any duplicate of legacy code, or
   only when it introduces duplication not already present? **Recommended:
   only introduced/changed head functions, with a base artifact as the
   comparator.**

The key decision is P0: whether this is a clean product transformation or a
search engine with a gate bolted alongside it. The latter keeps dependency,
surface-area, and reliability costs that do not serve CI gating.
