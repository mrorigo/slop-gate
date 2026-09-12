# Calibration protocol

Run the warning-default configuration against the protected branch before
promoting any rule to `error`.

1. Build an artifact and scan a pinned main-branch commit.
2. Sample at least 30 findings for each enabled rule (`function-mass`,
   `near-clone`, `lint-suppression-growth`, `unsafe-surface-growth`, and
   `dependency-surface-growth` when enabled).
3. Classify each as actionable duplication/growth, acceptable deliberate code,
   or analyzer defect.
4. Record the commit, configuration, rule thresholds, sample size, and
   false-positive rate in the pull request that changes severity.
5. Add a suppression only with a specific reason and exact path; prefer shared
   utilities or a refactor over recurring suppressions.

The initial policy should remain warning-only until the observed false-positive
rate is acceptable to the repository owners. Re-run the protocol after changing
normalization, parser versions, or a rule threshold.
