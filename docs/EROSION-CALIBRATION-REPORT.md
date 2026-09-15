# Structural erosion calibration

Status: preliminary panel evidence; not a threshold decision.

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

## P9.4 panel run

On 2026-09-15, the calibration runner attempted the 20 repositories listed in
`calibration/manifest.toml`. After bounded window retries and parser fixes, 14
repositories produced 20 valid commit records each, for 280 valid samples.
Five repositories still produced incomplete records because their tracked test
fixtures or generated/stress sources use syntax outside the current parser
coverage. Those records remain in
`calibration/results/` and are not treated as zero measurements.

For the 280 valid samples, erosion delta ranged from `-0.0157402` to
`0.0086378`; the median was `0`, p90 was `0.0001266`, p95 was `0.0005497`,
and p99 was `0.0061209`. No sample exceeded the provisional `0.08` delta
limit. The erosion-ratio median was `0.4111`, with p95 and maximum both about
`0.7237`. This suggests that background erosion and erosion regression are
different signals: high erosion can be common while adjacent-commit deltas
remain small. This is a runner smoke test, not threshold calibration: it is a
bounded first-parent window rather than the required monthly, release, and
high-churn sample, and it contains no reviewer classifications or contributor
stability judgments.

Cutoff sensitivity is available for the four recovered focused repositories.
Their mean erosion ratios were `0.6300` at cutoff 5, `0.4777` at cutoff 10,
and `0.3911` at cutoff 15. These figures are directional only and are not
sufficient to choose a cutoff.

The run therefore does not satisfy the plan's threshold-decision criteria.
The rule remains warning-only.

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
