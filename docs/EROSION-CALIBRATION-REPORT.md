# Structural erosion calibration

Status: preliminary local evidence only.

## Implemented measurement

Slop Gate computes:

```text
mass(function) = cyclomatic_complexity(function) × √SLOC(function)
erosion = mass(functions with CC > 10) / mass(all functions)
```

The measurement is deterministic for a fixed Git revision, analyzer, scope,
and cutoff. The default cutoff is `10`.

## Local evidence

The v0.4 implementation was exercised against this repository's history with:

```sh
slop-gate history --ref HEAD --count 3 --format json
```

The observed erosion ratios were approximately `0.3973`, `0.3925`, and
`0.4367`. This sample is not a calibration set. It contains one repository and
adjacent development commits. It cannot establish thresholds or a
false-positive rate.

## Release gate

Before promoting `structural-erosion` beyond warning severity, sample at least
20 maintained Rust repositories with varied sizes and history. Record feature
additions, refactors, erosion deltas, contributor stability, and reviewer
classification. Compare cutoff values below, at, and above `10`.

The default limits remain provisional:

```toml
erosion_limit = 0.50
delta_limit = 0.08
```

The paper that motivated this rule reports Python measurements. Those values
are evidence for the concept, not Rust threshold evidence.
