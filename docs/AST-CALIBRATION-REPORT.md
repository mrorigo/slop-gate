# AST clone calibration report

Date: 2026-09-13  
Rule: `near-clone`  
Policy: shipped warning-only defaults  
Artifact schema: v2

## Scope

This report covers the normalized AST signal added in 0.2. The calibration is a
deterministic synthetic fixture corpus. It validates matcher mechanics and
false-positive behavior for the selected syntax cases. It is not evidence for
promoting the rule to `error` on arbitrary repositories.

| Corpus | Samples | Expected matches | Observed matches | False-positive rate |
| --- | ---: | ---: | ---: | ---: |
| Identifier and literal-renamed clone pairs | 30 | 30 | 30 | 0.0% |
| Structurally different negative pairs | 30 | 0 | 0 | 0.0% |

The positive corpus emits exactly 30 `near-clone` findings at warning severity.
The negative corpus uses materially different control-flow and expression
structure. No negative pair passes both the token and AST similarity thresholds.

## Coverage

The fixture suite covers:

- identifier and literal normalization;
- formatting and comment invariance;
- statement-order sensitivity;
- changed operators and control-flow structure;
- closure, macro, attribute, and nested-function syntax;
- artifact rejection for schema version 1;
- deterministic component similarity properties in clone findings;
- the requirement that both token and AST signals pass.

## Reproduction

Run the calibration assertion with:

```sh
cargo test calibrates_thirty_structural_clone_pairs_and_thirty_negatives
```

Run the full validation suite with:

```sh
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps
cargo audit
```

The calibration test passed in debug mode in `0.28 s` and in optimized mode in
`0.17 s` on the development machine. Twenty optimized process runs ranged from
`0.14 s` to `0.53 s`; the sample p95 was `0.35 s`. This is a smoke measurement,
not the fixed 20-file repository benchmark.

## Decision

The AST signal remains warning-only. The synthetic corpus meets the mechanical
30-sample and 5% thresholds, but it does not justify error-level promotion.
Collect at least 30 naturally occurring findings from protected-branch history
before changing the default severity.
