# Slop Gate calibration report

Date: 2026-09-12  
Repository commit: `71d4e44`  
Tool configuration: missing `.slop-gate.toml` (validated warning-only defaults)  
Toolchain: Rust 2024, Tree-sitter Rust 0.24  

## Scope

This report calibrates the three introduced-debt rules shipped in P5:

| Rule | Samples | Actionable | Acceptable deliberate code | Analyzer defects | False-positive rate |
| --- | ---: | ---: | ---: | ---: | ---: |
| `lint-suppression-growth` | 30 | 30 | 0 | 0 | 0.0% |
| `unsafe-surface-growth` | 30 | 30 | 0 | 0 | 0.0% |
| `dependency-surface-growth` | 30 | 30 | 0 | 0 | 0.0% |

The sample IDs are deterministic fixture observations, not claims about the
current Slop Gate source tree. Each was constructed to satisfy the rule's
specified introduction condition and has an expected finding. They establish
rule mechanics and renderer stability; they do not establish that a real
repository should promote a rule to `error`.

## Reproduction

The implementation and fixture tests are reproduced with:

```sh
cargo fmt --check
cargo check
cargo clippy --all-targets -- -D warnings
cargo test
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps
```

The three 30-observation sample sets are identified as follows:

| Rule | Sample IDs | Fixture dimensions |
| --- | --- | --- |
| `lint-suppression-growth` | `LS-001`–`LS-030` | new, broadened, outer, inner, conditional, formatting, relocation, path/line suppression, warning, error, JSON, SARIF |
| `unsafe-surface-growth` | `US-001`–`US-030` | block, function, trait, implementation, extern block, existing-surface edits, rename, suppression, warning, error, JSON, SARIF |
| `dependency-surface-growth` | `DS-001`–`DS-030` | registry, Git, path, target-specific, workspace-inherited, new, feature expansion, default-feature expansion, version-only, rename, malformed TOML, suppression, warning, error, JSON, SARIF |

Each dimension has at least one fixture assertion. Repeated IDs cover the
cross-product of introduction form and output/policy projection; fixture order
is stable by path, line, rule ID, and normalized identity.

## Decision

No rule is promoted to `error`. The 0.0% rate above is a synthetic-fixture
rate and is insufficient evidence for repository-wide policy. Before an error
promotion, run the protocol on at least 30 naturally occurring warning
findings per rule across the protected branch history, classify each finding,
and attach the resulting corpus and false-positive calculation to the policy
change. The existing defaults remain warning-only.
