# Structural-erosion calibration

This directory contains the pinned input and runner for the P9.4 calibration
study. It is separate from the Rust build and never runs Cargo in a cloned
repository.

Run from the repository root:

```sh
./calibration/run.sh --binary target/release/slop-gate --cutoffs 5,10,15
```

The runner clones each repository into a temporary directory, resolves its
default branch, samples first-parent history, and writes results under
`calibration/results` (or `--output`). It records resolved commits in
`manifest.lock` and removes checkouts when it exits. If the preferred window
contains invalid historical Rust, it tries up to five earlier windows. Use
`--retries N` to change that bounded limit.

The run requires `git`, `jq`, and a built Slop Gate binary. Network access is
needed only for cloning.

Each history document includes `erosion_by_cutoff` values. The first cutoff is
also used for the commit-level `erosion_delta` records.
