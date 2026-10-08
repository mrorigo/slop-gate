# Slop Gate user guide

Slop Gate is a deterministic, local analysis tool for Rust, Python, and
TypeScript repositories. It helps teams review large or complex functions and
structural duplication. Rust rules also track unsafe and lint-suppression
surface, dependency changes, and structural erosion.

Analysis runs without a model or network access. The `check` command compares a candidate Git revision with an indexed baseline; `scan` and `history` provide repository-level views.

## Contents

- [Getting started](getting-started.md): install Slop Gate and run an initial audit.
- [Commands](commands.md): command reference for `init`, `index`, `check`, `scan`, and `history`.
- [Configuration](configuration.md): policy file format, defaults, and suppressions.
- [Rules](rules.md): what each rule measures and how to interpret its thresholds.
- [Results and CI](results-and-ci.md): output formats, exit codes, baseline artifacts, and CI workflow.

## Typical workflow

1. Install Slop Gate and create a policy with `slop-gate init`.
2. Run `slop-gate scan` to review existing duplicate Rust code.
3. In CI, build a baseline artifact for the protected branch with `slop-gate index`.
4. Run `slop-gate check` for the pull request using the artifact for its exact base commit.
5. Review warnings, tune policy thresholds, and promote selected rules to errors when they fit the project.

See [Getting started](getting-started.md) for commands and [Results and CI](results-and-ci.md) for artifact handling and CI details.

## Scope

Rust has the full rule set. Python and TypeScript support named functions and
methods for `function-mass` and same-language `near-clone` findings. Calibrate
their metrics before relying on error severity. Slop Gate complements formatting,
compiler, lint, and dependency-audit checks. It does not prove semantic
equivalence or assess security. See [Rules](rules.md) and [Results and CI](results-and-ci.md)
for measurement details and limits.
