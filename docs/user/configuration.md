# Configuration

Slop Gate reads `.slop-gate.toml` from the Git repository root. If the file is absent, it uses the defaults shown below: all rules warn, and no findings block the command. `slop-gate init` writes a complete starter file.

Unknown keys are errors. The current configuration schema is version 1.

## Example

```toml
version = 1

[rules.function_mass]
severity = "warn"          # off | warn | error
new_function_limit = 80.0
delta_limit = 20.0

[rules.near_clone]
severity = "warn"
minimum_sloc = 8
minimum_tokens = 40
similarity_threshold = 0.85
max_candidates = 64
detect_blocks = true
block_max_candidates = 256
block_max_families = 3
# block_minimum_tokens defaults to 3 * minimum_tokens (120 here).

[rules.lint_suppression]
severity = "warn"

[rules.unsafe_surface]
severity = "warn"

[rules.dependency_surface]
severity = "warn"

[rules.structural_erosion]
severity = "warn"
erosion_limit = 0.50
delta_limit = 0.08
mass_growth_limit = 0.08
complexity_cutoff = 10
top_contributors = 3

[[suppressions]]
rule = "near-clone"
path = "src/compat.rs"
line = 42 # optional; omit to suppress this rule throughout the file
reason = "Protocol compatibility requires this implementation."
```

Every field has a default, so you can include only the settings you want to change. Values above are the defaults.

## Rule severity

Each rule accepts one of:

- `off`: do not report findings for this rule.
- `warn`: report findings without failing `check` or `scan`.
- `error`: report findings and return exit code 1.

Operational failures, including invalid configuration, return exit code 2 regardless of rule severity. See [Results and CI](results-and-ci.md).

## Settings

### `function_mass`

- `new_function_limit` (default `80.0`): mass above which a new function is reported.
- `delta_limit` (default `20.0`): increase in function mass above which a changed function is reported.

Function mass is cyclomatic complexity multiplied by the square root of source lines of code. See [Rules](rules.md).

### `near_clone`

- `minimum_sloc` (default `8`): minimum function source lines for whole-function clone comparison.
- `minimum_tokens` (default `40`): minimum token sequence length for whole-function comparison; must exceed one internal shingle window.
- `similarity_threshold` (default `0.85`): required similarity, from `0.0` through `1.0`.
- `max_candidates` (default `64`): candidate budget for whole-function comparison.
- `detect_blocks` (default `true`): detect duplicated contiguous blocks inside larger functions.
- `block_max_candidates` (default `256`): separate candidate budget for block comparison.
- `block_max_families` (default `3`): maximum block clone families reported per function.
- `block_minimum_tokens` (default three times `minimum_tokens`): minimum token span for a duplicated block.

Block detection is independent of whole-function comparison. Its defaults are intentionally sized to find substantial copied regions without reporting small repeated idioms. `scan --threshold` and `scan --min-sloc` override the corresponding values for a single scan.

### `lint_suppression`, `unsafe_surface`, and `dependency_surface`

Each section contains only `severity` (default `warn`). These rules report additions or expansions in changed content. The dependency rule inspects Cargo manifest dependency declarations.

### `structural_erosion`

- `erosion_limit` (default `0.50`): maximum allowed share of total function mass in functions above the complexity cutoff.
- `delta_limit` (default `0.08`): maximum allowed increase in that share from base to head.
- `mass_growth_limit` (default `0.08`): maximum relative growth of absolute high-complexity function mass.
- `complexity_cutoff` (default `10`): functions are considered high-complexity when cyclomatic complexity is greater than this value.
- `top_contributors` (default `3`): number of changed high-complexity contributors included in finding properties; must be from 1 through 10.

The rule reports a finding if any of the three limits is breached. See [Rules](rules.md).

## Suppressions

Suppressions are exact policy exceptions. Each entry requires:

- `rule`: one of `function-mass`, `near-clone`, `lint-suppression-growth`, `unsafe-surface-growth`, `dependency-surface-growth`, or `structural-erosion`.
- `path`: repository-relative path.
- `reason`: non-empty explanation.
- `line`: optional positive source line. If omitted, the rule is suppressed for that path.

Keep exceptions narrow and explain why the finding is acceptable. A suppression is part of the active policy fingerprint, so changing it requires rebuilding the baseline artifact used by `check`.
