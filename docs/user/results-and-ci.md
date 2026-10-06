# Results and CI

## Report formats

`check` and `scan` support three formats:

- `human` (default): readable terminal and CI log output.
- `json`: structured report for custom tooling.
- `sarif`: SARIF 2.1.0 for code-scanning integrations.

Findings include a rule ID, severity, message, source location, and rule-specific properties. Clone findings can include a related location for the matching code. SARIF includes related locations when available.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Analysis completed without error-severity findings. |
| `1` | Analysis completed with one or more error-severity findings. |
| `2` | Configuration, Git, artifact, or analysis failure prevented a trustworthy result. |

Warnings are reported but do not fail the command. `history` is a report command and returns success when it completes, including when rule thresholds would otherwise be exceeded.

## Baseline artifacts

Build an artifact for the protected branch:

```sh
slop-gate index --ref origin/main --output .slop-gate/main.json
```

Run a pull request check against the artifact for its exact base commit:

```sh
slop-gate check \
  --base "$BASE_SHA" \
  --head HEAD \
  --index .slop-gate/main.json \
  --format sarif > slop-gate.sarif
```

The artifact is bound to its Git commit, tool/analyzer version, and active configuration policy. A different base commit, policy, or incompatible tool version requires rebuilding it. Cache or store artifacts keyed by the exact commit SHA; do not reuse the artifact for a nearby commit.

The artifact can be stored in CI cache or another artifact store. Fetch the base revision and restore its artifact before running `check`. If artifact validation fails, regenerate the artifact with the same tool version and policy used for the check.

## GitHub Actions

This repository includes a reference workflow at `.github/workflows/slop-gate.yml`. It downloads and verifies a Linux release binary and runs the gate. Set `SLOP_GATE_RELEASE` to a release tag to pin the tool version. Review the workflow's triggers and repository-specific setup when copying it into another project.

To display the workflow status in a README, use:

```markdown
[![Slop Gate passing](https://github.com/OWNER/REPO/actions/workflows/slop-gate.yml/badge.svg)](https://github.com/OWNER/REPO/actions/workflows/slop-gate.yml)
```

Replace `OWNER` and `REPO`. Enable the workflow on the default branch so GitHub can show its status.

## Review boundaries

Slop Gate complements `cargo fmt --check`, Clippy/compiler checks, and dependency auditing. It identifies structural patterns and change growth; it does not prove semantic equivalence, assess security, or replace a linter. Start with warning severity and calibrate thresholds against real repository changes before making a rule merge-blocking.
