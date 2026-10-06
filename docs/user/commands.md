# Commands

Run `slop-gate --help` or `slop-gate <command> --help` for options supported by the installed version.

## `init`

Create `.slop-gate.toml` in the Git repository root:

```sh
slop-gate init
```

The generated policy uses warning-only defaults. Existing files are preserved unless `--force` is supplied:

```sh
slop-gate init --force
```

See [Configuration](configuration.md).

## `index`

Analyze supported source files at a Git revision and write a versioned JSON baseline artifact:

```sh
slop-gate index --ref origin/main --output .slop-gate/main.json
```

`--ref` is required. The configured policy is loaded from the repository root and bound into the artifact's compatibility fingerprint. See [Results and CI](results-and-ci.md).

## `check`

Evaluate changes between a base and head revision using an artifact built for the base:

```sh
slop-gate check --base "$BASE_SHA" --head HEAD \
  --index .slop-gate/main.json --format human
```

Required options:

- `--base`: Git revision represented by the artifact.
- `--head`: Git revision to inspect.
- `--index`: baseline artifact path.
- `--format`: `human` (default), `json`, or `sarif`.

The artifact must match the resolved base commit, analyzer version, and active policy. `check` evaluates changed Rust, Python, and TypeScript files, plus relevant Cargo manifest changes. See [Rules](rules.md) and [Results and CI](results-and-ci.md).

## `scan`

Find structural near-clones in a committed revision or working tree:

```sh
slop-gate scan
slop-gate scan --ref v1.2.0 --format json
slop-gate scan --working-tree --path src --top 20
```

Options:

- `--ref REV`: scan a Git revision; defaults to `HEAD` and conflicts with `--working-tree`.
- `--working-tree`: include local edits and untracked, non-ignored supported source files.
- `--path PATH`: restrict by file or directory; repeat to select multiple paths.
- `--no-ignore`: include Git-ignored files.
- `--threshold 0.0..=1.0`: override the near-clone similarity threshold for this scan.
- `--min-sloc N`: override the minimum function size for this scan.
- `--top N`: limit output to the top N clone pairs.
- `--format`: `human` (default), `json`, or `sarif`.

The scan uses the near-clone rule's other settings from `.slop-gate.toml`. Warning findings do not fail the command; findings configured as errors do. See [Rules](rules.md).

## `history`

Report complexity and erosion metrics over first-parent Git history:

```sh
slop-gate history --ref HEAD --count 20
slop-gate history --ref HEAD --count 12 \
  --complexity-cutoffs 5,10,15 --format json
```

Options:

- `--ref REV`: end of the history window; defaults to `HEAD`.
- `--count N`: number of commits from 1 through 100; defaults to 10.
- `--complexity-cutoff CC`: override the policy cutoff for high-complexity functions.
- `--complexity-cutoffs CC,CC,...`: report several cutoffs from one source analysis; conflicts with `--complexity-cutoff`.
- `--format`: `human` (default), `json`, or `sarif`.

History is an observational report and exits successfully when analysis completes. The default cutoff comes from the structural-erosion policy. See [Rules](rules.md).
