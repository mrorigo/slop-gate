# Rules

`check` evaluates changes in repository context using the configured rules. All rule severities default to `warn`. Set a rule to `error` to make its findings fail `check`; set it to `off` to disable it. `scan` applies the near-clone rule to a whole selected revision or working tree. See [Configuration](configuration.md).

## `function-mass`

Measures function size and control-flow complexity. Mass is:

```text
cyclomatic complexity × √(source lines of code)
```

Source lines exclude blank and comment-only lines. The rule reports a new function when its mass exceeds `new_function_limit` (default 80), and reports a changed function when its increase over the baseline exceeds `delta_limit` (default 20). Review the function's responsibilities and whether work can be split into cohesive helpers.

## `near-clone`

Finds structurally similar functions within the same language and repeated blocks. Identifiers and literals are normalized, so renaming a copied implementation does not prevent a match. Whole-function candidates must pass both normalized token and AST-shingle similarity checks.

Block detection finds long contiguous shingle runs shared by separate functions, including islands within otherwise large, dissimilar functions. It is enabled by default and has a separate candidate budget. Findings show the matching locations, scope (`whole-function` or `block`), similarity, and roles. Clone families summarize related pairs. Their `duplicate_mass` is recoverable mass: family mass minus its largest member.

Risk is high when both functions are production logic, medium when one is a constructor, and low when either is a test or accessor. A finding is a review prompt; similarity does not establish semantic equivalence. Consider extracting shared behavior when the match is real and likely to evolve together.

`scan` reports this rule against all selected functions. `check` reports duplication introduced by changed functions compared with the baseline. Tune comparison thresholds in [Configuration](configuration.md), or temporarily override scan threshold and minimum SLOC with CLI options.

## `lint-suppression-growth`

Reports newly added or broadened Rust `allow` and `expect` attributes, including conditional allow attributes. Review whether the suppression is scoped narrowly and whether the underlying diagnostic can be fixed instead.

## `unsafe-surface-growth`

Reports newly introduced unsafe blocks, unsafe functions, unsafe traits, unsafe implementations, and unsafe extern blocks. Review the safety invariants and whether the unsafe boundary can be narrowed.

## `dependency-surface-growth`

Reports new direct production dependency edges and expanded dependency declarations, including enabling default features or adding features. It inspects Cargo dependencies and build-dependencies, including target-specific sections. It does not replace advisory or license checks.

## `structural-erosion`

Measures concentration of function mass in functions whose cyclomatic complexity exceeds the configured cutoff. A finding is produced when any of these conditions is breached between base and head:

- High-complexity mass grows beyond `mass_growth_limit` (default 8%).
- The erosion ratio increases beyond `delta_limit` (default 8 percentage points).
- The head erosion ratio exceeds `erosion_limit` (default 50%).

The erosion ratio is high-complexity function mass divided by total function mass. Findings include the breach names, base/head ratios and mass, and top changed contributors. Because the level condition can reflect older code, inspect the report's contributors and history before attributing all erosion to the current change. The `history` command helps show how the measure changed across commits.

## Analysis errors

`check` reports a warning and skips a changed supported source file that cannot be analyzed; this is not a rule severity and does not by itself fail the command. Parse failures should be investigated, since skipped files cannot produce trustworthy rule results. Rust-specific rules do not analyze Python or TypeScript files.
