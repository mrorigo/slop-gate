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

Cutoff sensitivity is available for all 14 accepted repositories and 280
revisions in `calibration/results-sensitivity/`. Mean erosion ratios were
`0.5038` at cutoff 5, `0.3507` at cutoff 10, and `0.2639` at cutoff 15. The
monotonic decrease is expected because higher cutoffs classify fewer functions
as high complexity. These means describe cutoff behavior, not a threshold
decision; the sample still lacks reviewer classifications and monthly,
release, and high-churn stratification.

The monthly run is preserved in `calibration/results-monthly/`. It contains
168 successful checkpoints, 12 for each accepted repository, with no missing
or failed month. Across these checkpoints, the erosion-ratio median was
`0.5418` and p95 was `0.8140`. Mean ratios by cutoff were `0.5033`, `0.3507`,
and `0.2638` for cutoffs 5, 10, and 15. The monthly data confirms that erosion
level varies materially across repositories and time; it does not yet provide
commit-level reviewer classifications.

The high-churn run is preserved in `calibration/results-high-churn/`. It
selected the five largest first-parent diffs from the most recent 200 commits
in each accepted repository: 70 commits total, with 68 valid analyses and two
analysis failures. The valid erosion deltas ranged from `-0.0227137` to
`0.0465756`; the median was `-0.0000055`, p95 was `0.0067620`, and no sample
exceeded `0.08`. Each record now retains the commit subject and changed paths,
which are the inputs required for reviewer classification.

This is measurement context, not reviewer classification. Contributor
stability and human actionability remain unrecorded; the high-churn sample must
not be used as evidence that the rule is ready for error severity.

A provisional subject/path audit classified the 68 valid high-churn commits as
52 expected-maintenance commits, 15 feature-growth commits, and one deliberate
refactoring commit. These labels are keyword-based triage only; they are not
reviewer judgments and are excluded from threshold fitting. No contributor
stability or reviewer-actionability result is claimed by this report.

The contributor-aware reruns are preserved in
`calibration/results-monthly-contributors/` and
`calibration/results-high-churn-contributors/`. All 168 monthly checkpoints
and 68 valid high-churn records include top-contributor locations. The leading
contributor remained the same path and qualified function across 152 of 154
adjacent monthly pairs (98.70%). This is strong persistence evidence for the
current panel, although it does not establish that the location is the one a
human reviewer would choose first.

Contributor selection now uses only high-complexity functions whose source
range intersects an added head line. Unchanged dominant functions remain part
of the repository erosion ratio but cannot be reported as change contributors.
This addresses the main actionability failure observed in the review sample.

The run therefore does not satisfy the plan's threshold-decision criteria.
The rule remains warning-only.

## v0.5 changes from adopter evidence

Feedback from wiring Slop Gate into a 7k-line pure-Rust repository produced
five changes. Two are measurement corrections to this report's own defaults.

The ratio limit is permanently true for a large share of repositories. The
monthly panel p95 is 0.8140 against an `erosion_limit` of 0.50, so for those
repositories the level disjunct fires on every commit and the rule reduces to
its delta disjunct. The adjacent-commit delta p99 is 0.0061209 and no sample in
any run reached the 0.08 `delta_limit`, so that disjunct does not discriminate
either. The rule was effectively inert.

The ratio also cannot separate added complexity from deleted easy code. Deleting
437 lines of low-complexity test code moved erosion from 75.89% to 76.06%
because the denominator shrank while the high-complexity mass stayed put. The
same report recorded a real refactor worth -0.58%, so a deletion and a
deduplication were being compared as if they were the same class of change.

v0.5 adds `mass_growth_limit`, the relative growth of absolute high-complexity
mass, which is immune to denominator effects because it only moves when
high-complexity mass itself moves. The finding now reports
`base_high_complexity_mass`, `head_high_complexity_mass`,
`high_complexity_mass_growth`, and the `breaches` that fired. Adjacent-commit
high-complexity mass deltas measured over the last 40 first-parent revisions of
this repository were 0 at the median, 114.1 at p90, and 230.6 at the maximum,
against a base high-complexity mass of 1178.7, so 8% growth is above ordinary
churn and below a real feature-sized addition.

Every erosion finding now carries `erosion_at_cc_5`, `erosion_at_cc_10`,
`erosion_at_cc_15`, `erosion_at_cc_20`, and `erosion_at_cc_30`. The panel means
of 0.5033, 0.3507, and 0.2638 at cutoffs 5, 10, and 15 show that a single ratio
is not comparable across repositories, and the distribution shows whether a
repository is high everywhere or only at the configured cutoff. The per-repo
cutoff that makes the ratio interpretable is still an open question; this makes
the raw material available rather than answering it.

Block detection calibration. The adopter's central finding was a recall gap: a
byte-identical 30-line block inside two ~187-line functions scored 0.16 as a
whole-function pair. Block detection is function-independent, and its
`block_minimum_tokens` floor trades recall against the noise that whole-function
comparison already produces. Measured on this repository at 9.3k lines, with
the adopter's 30-line island as the recall probe:

| block_minimum_tokens | findings on this repository | island found |
| --- | --- | --- |
| 40 (equal to minimum_tokens) | 82 | yes |
| 60 | 24 | yes |
| 80 | 10 | yes |
| 120 (default, 3x minimum_tokens) | 2 | yes |
| 160 | 1 | yes |
| 400 | 1 | yes |

The default sits at the knee. The two findings at the default are duplicated
blocks in the analyzer's own normalization functions, both rated high risk.
Recall for a genuinely large island is insensitive to the floor: the island
stays found at every value tested, including one above its own size, because a
byte-identical block scores full containment whenever the floor is cleared.

The rule remains warning-only. This is one repository for the noise side of the
trade and a synthetic probe for the recall side; it is not the 20-repository
sample the release gate requires.

## Release gate

Before promoting `structural-erosion` beyond warning severity, sample at least
20 maintained Rust repositories with varied sizes and history. Record feature
additions, refactors, erosion deltas, contributor stability, and reviewer
classification. Compare cutoff values below, at, and above `10`.

The default limits remain provisional:

```toml
erosion_limit = 0.50
delta_limit = 0.08
mass_growth_limit = 0.08
```

The paper that motivated this rule reports Python measurements. Those values
are evidence for the concept, not Rust threshold evidence.
