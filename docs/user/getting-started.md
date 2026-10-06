# Getting started

## Install

Install from the current checkout:

```sh
cargo install --path .
```

Install the published crate:

```sh
cargo install slop-gate --locked
```

Or run it from the checkout without installing:

```sh
cargo run --release -- --help
```

## Create a policy

From inside a Git repository, run:

```sh
slop-gate init
```

This creates `.slop-gate.toml` at the repository root with all rules set to warning severity. It will not replace an existing file. To intentionally regenerate it, use `slop-gate init --force`. See [Configuration](configuration.md) to adjust the policy.

## Run an initial scan

Scan the committed `HEAD` revision:

```sh
slop-gate scan
```

Include local edits and untracked, non-ignored Rust files:

```sh
slop-gate scan --working-tree
```

Limit the scan to selected paths and show at most 20 clone pairs:

```sh
slop-gate scan --working-tree --path crates/core --top 20
```

Git-ignored files are excluded by default, including in a working-tree scan. Add `--no-ignore` to include them. A scan finds structural near-clones; it does not report function-mass or change-growth findings. See [Commands](commands.md) and [Rules](rules.md).

## Add the gate to CI

The core workflow is to create an artifact from the protected branch and use it to evaluate the pull request against that exact base commit:

```sh
slop-gate index --ref origin/main --output .slop-gate/main.json

slop-gate check \
  --base "$BASE_SHA" \
  --head HEAD \
  --index .slop-gate/main.json \
  --format sarif > slop-gate.sarif
```

Store or cache the artifact by the base commit SHA, and restore that matching artifact for each check. See [Results and CI](results-and-ci.md) for artifact compatibility and the repository's reference GitHub Actions workflow.
