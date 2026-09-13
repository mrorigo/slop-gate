# Slop Gate

> A deterministic CI gate for code that is getting larger, more complex, or
> suspiciously similar to code you already own.

`slop-gate` reviews a pull request in repository context. It builds a compact
baseline artifact from a trusted Git commit, then evaluates only changed Rust
functions in the candidate commit. It runs locally, uses no model, and makes no
network request during analysis.

```text
main commit ── slop-gate index ──► versioned baseline artifact
                                        │
pull request ── slop-gate check ────────┴──► human, JSON, or SARIF findings
```

## What it catches

| Rule | Question it answers | Measurement |
| --- | --- | --- |
| `function-mass` | Did this change add too much function-level complexity? | `CC × √SLOC` and the change from the base function. |
| `near-clone` | Did this pull request add an implementation that already exists? | Bounded token-shingle candidates requiring both normalized token and AST-shingle similarity. |
| `lint-suppression-growth` | Did this change weaken a compiler or Clippy diagnostic? | Added or broadened `allow`/`expect` attributes. |
| `unsafe-surface-growth` | Did this change add unsafe surface? | Added unsafe blocks, declarations, implementations, or extern blocks. |
| `dependency-surface-growth` | Did this change expand production dependency surface? | New direct edges or added features/default features. |

Cyclomatic complexity (CC) counts independent control-flow paths. Source lines
of code (SLOC) exclude blank and comment-only lines. Identifiers and literals
are normalized before clone comparison, so a renamed copy still matches.

## Install

Build from this checkout:

```sh
cargo install --path .
```

Or run without installing:

```sh
cargo run --release -- --help
```

The current supported gate language is Rust.

## Use it in CI

1. Build an artifact from the protected branch after merge.

   ```sh
   slop-gate index --ref origin/main --output .slop-gate/main.json
   ```

2. Store `.slop-gate/main.json` in CI storage keyed by the exact main commit.

3. Fetch the pull request base commit and restore its matching artifact.

4. Run the gate against the pull request head.

   ```sh
   slop-gate check \
     --base "$BASE_SHA" \
     --head HEAD \
     --index .slop-gate/main.json \
     --format sarif > slop-gate.sarif
   ```

The artifact is tied to both the exact base commit and the active policy. A
changed policy requires a rebuilt artifact. The reference GitHub Actions
workflow is [`.github/workflows/slop-gate.yml`](.github/workflows/slop-gate.yml).

## Start with an audit

Use `scan` before blocking a merge. It reports structural near-clones in one
revision and helps calibrate thresholds.

```sh
slop-gate scan --ref HEAD --format human
slop-gate scan --ref HEAD --format sarif > slop-gate.sarif
```

Follow the [calibration protocol](docs/CALIBRATION.md) before changing a rule
from `warn` to `error`.

## Configure policy

Create `.slop-gate.toml` in the repository root. Missing configuration uses
warning-only defaults. Unknown keys are errors, so a misspelled rule cannot
silently weaken a gate.

```toml
version = 1

[rules.function_mass]
severity = "warn"          # off | warn | error
new_function_limit = 80.0
delta_limit = 20.0

[rules.near_clone]
severity = "warn"          # off | warn | error

[rules.lint_suppression]
severity = "warn"

[rules.unsafe_surface]
severity = "warn"

[rules.dependency_surface]
severity = "warn"

[[suppressions]]
rule = "near-clone"
path = "src/compat.rs"
line = 42                   # optional; omit to suppress this rule for the path
reason = "Protocol compatibility requires this implementation."
```

Use suppressions sparingly. A suppression must name a supported rule, use a
repository-relative path, and include a reason. Prefer a shared utility or a
refactor when possible.

## Understand results and exits

`check` and `scan` render the same findings in three formats:

| Format | Use |
| --- | --- |
| `human` | Local review and CI logs. |
| `json` | Custom CI processing. |
| `sarif` | Code-scanning integrations. |

| Exit code | Meaning |
| --- | --- |
| `0` | Analysis completed without error-severity findings. |
| `1` | Analysis completed with at least one error-severity finding. |
| `2` | Configuration, Git, artifact, or analyzer failure prevented a trustworthy result. |

Warnings are always reported. They do not fail the command.

## Design boundaries

Slop Gate complements, rather than replaces, the standard Rust checks.

| Tool | Primary job |
| --- | --- |
| `cargo fmt --check` | Formatting consistency. |
| `cargo clippy -- -D warnings` | Local correctness and idiomatic Rust. |
| `cargo audit` | Dependency advisory checks. |
| `slop-gate check` | PR-introduced function growth and repository-scale duplication. |

It does not prove semantic equivalence, assess security, replace a linter, or
analyze uncommitted working-tree changes. A malformed changed Rust file produces
an analyzer warning and is skipped. A malformed baseline prevents artifact
creation.

## Develop

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The full design and phase history live in
[docs/SLOP-GATE-PLAN.md](docs/SLOP-GATE-PLAN.md) and [HANDOFF.md](HANDOFF.md).
