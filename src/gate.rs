// Rust guideline compliant 2026-09-12
//! Baseline artifact construction and function-mass gate evaluation.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::Serialize;

use crate::analysis::{
    FunctionIdentity, FunctionRecord, FunctionRole, IndexArtifact, RepositorySummary, SHINGLE_SIZE,
    analyze_source_file, analyzer_fingerprint, analyzer_fingerprint_with_policy,
};
use crate::analysis::{dependency_edges, lint_suppressions, unsafe_surface};
use crate::config::{GateConfig, NearCloneRule, RuleSeverity};
use crate::error::{Error, Result};
use crate::git::{ChangedLineSet, GitRepository};

/// A location in repository-relative source coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Location {
    pub(crate) path: String,
    pub(crate) line: usize,
}

/// One error or warning produced by a gate evaluation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Finding {
    pub(crate) rule_id: String,
    pub(crate) severity: Severity,
    pub(crate) message: String,
    pub(crate) location: Location,
    pub(crate) base_location: Option<Location>,
    pub(crate) base_mass: Option<f64>,
    pub(crate) head_mass: Option<f64>,
    pub(crate) delta: Option<f64>,
    pub(crate) threshold: Option<f64>,
    pub(crate) similarity: Option<f64>,
    #[serde(default)]
    pub(crate) properties: BTreeMap<String, String>,
}

/// Finding severity used for exit status and renderers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Severity {
    Warning,
    Error,
}

/// Deterministically ordered result of a `check` invocation.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct CheckReport {
    pub(crate) findings: Vec<Finding>,
}

impl CheckReport {
    /// Returns whether any finding must fail a CI job.
    pub(crate) fn has_errors(&self) -> bool {
        self.findings
            .iter()
            .any(|finding| finding.severity == Severity::Error)
    }
}

/// Builds a complete Rust baseline artifact from one resolved revision.
pub(crate) fn build_artifact(repository: &GitRepository, revision: &str) -> Result<IndexArtifact> {
    let commit = repository.resolve_revision(revision)?;
    let files = repository
        .source_files(&commit)?
        .into_iter()
        .map(|path| {
            repository.read_blob(&commit, &path).and_then(|source| {
                analyze_source_file(&path, &source)
                    .map_err(|error| Error::invalid("source file", format!("{path}: {error}")))
            })
        })
        .collect::<Result<Vec<_>>>()?;
    IndexArtifact::new(commit, files)
}

/// Builds an in-memory artifact from the current working tree.
pub(crate) fn build_working_tree_artifact(
    repository: &GitRepository,
    include_ignored: bool,
) -> Result<IndexArtifact> {
    let commit = repository.resolve_revision("HEAD")?;
    let files = repository
        .working_tree_source_files(include_ignored)?
        .into_iter()
        .map(|path| {
            repository
                .read_working_tree(&path)
                .and_then(|source| analyze_source_file(&path, &source))
        })
        .collect::<Result<Vec<_>>>()?;
    IndexArtifact::new(commit, files)
}

/// Computes repository summaries for several cutoffs with one source analysis.
pub(crate) fn summarize_revision_with_cutoffs(
    repository: &GitRepository,
    revision: &str,
    complexity_cutoffs: &[u32],
) -> Result<(String, Vec<RepositorySummary>)> {
    if complexity_cutoffs.is_empty() {
        return Err(Error::invalid("complexity cutoffs", "must not be empty"));
    }
    let commit = repository.resolve_revision(revision)?;
    let files = repository
        .source_files(&commit)?
        .into_iter()
        .map(|path| {
            repository.read_blob(&commit, &path).and_then(|source| {
                analyze_source_file(&path, &source)
                    .map_err(|error| Error::invalid("source file", format!("{path}: {error}")))
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let files = files
        .into_iter()
        .filter(|file| file.language == "rust")
        .collect::<Vec<_>>();
    let summaries = complexity_cutoffs
        .iter()
        .map(|cutoff| RepositorySummary::from_files_with_cutoff(&files, *cutoff))
        .collect();
    Ok((commit, summaries))
}

/// Binds a successfully built artifact to the active repository policy.
pub(crate) fn bind_policy(artifact: &mut IndexArtifact, config: &GateConfig) -> Result<()> {
    artifact.analyzer_fingerprint = analyzer_fingerprint_with_policy(&config.fingerprint()?);
    artifact.validate_with_fingerprint(&artifact.analyzer_fingerprint)
}

/// Evaluates changed Rust functions in `head` against a validated base artifact.
pub(crate) fn check_mass(
    repository: &GitRepository,
    base: &str,
    head: &str,
    artifact: &IndexArtifact,
    config: &GateConfig,
) -> Result<CheckReport> {
    let base_commit = repository.resolve_revision(base)?;
    if artifact.repository_commit != base_commit {
        return Err(Error::invalid(
            "baseline artifact",
            format!(
                "was built for {}, but --base resolves to {}",
                artifact.repository_commit, base_commit
            ),
        ));
    }
    let expected_fingerprint = if artifact.analyzer_fingerprint == analyzer_fingerprint()
        && *config == GateConfig::default()
    {
        analyzer_fingerprint()
    } else {
        analyzer_fingerprint_with_policy(&config.fingerprint()?)
    };
    artifact.validate_with_fingerprint(&expected_fingerprint)?;
    let head_commit = repository.resolve_revision(head)?;
    let changed = repository.changed_source_files(&base_commit, &head_commit)?;
    let base_functions = artifact
        .files
        .iter()
        .flat_map(|file| file.functions.iter())
        .map(|function| (function.identity.clone(), function))
        .collect::<HashMap<_, _>>();
    let mut report = CheckReport::default();

    // Analyze changed files before building clone candidates so candidates
    // represent declarations that still exist in HEAD. Baseline records are
    // retained separately for mass-delta comparisons below.
    let mut head_files = HashMap::new();
    for path in &changed.paths {
        let source = repository.read_blob(&head_commit, path)?;
        match analyze_source_file(path, &source) {
            Ok(file) => {
                head_files.insert(path.clone(), file);
            }
            Err(error) => {
                report.findings.push(Finding {
                    rule_id: "analysis-error".to_string(),
                    severity: Severity::Warning,
                    message: format!("skipped {path}: {error}"),
                    location: Location {
                        path: path.clone(),
                        line: 1,
                    },
                    base_location: None,
                    base_mass: None,
                    head_mass: None,
                    delta: None,
                    threshold: None,
                    similarity: None,
                    properties: BTreeMap::new(),
                });
            }
        }
    }

    let head_paths = repository
        .source_files(&head_commit)?
        .into_iter()
        .collect::<HashSet<_>>();
    let changed_base_paths = changed
        .renamed_from
        .values()
        .chain(changed.paths.iter())
        .collect::<HashSet<_>>();
    let head_functions_by_base_identity = head_files
        .values()
        .flat_map(|file| {
            let previous_path = changed.renamed_from.get(&file.path);
            file.functions.iter().map(move |function| {
                (
                    renamed_identity(&function.identity, previous_path),
                    function,
                )
            })
        })
        .collect::<HashMap<_, _>>();
    let clone_candidates = artifact
        .files
        .iter()
        .flat_map(|file| file.functions.iter())
        .filter_map(|base_function| {
            let head_path = changed
                .renamed_from
                .iter()
                .find_map(|(new_path, old_path)| {
                    (old_path == &base_function.identity.path).then_some(new_path)
                })
                .unwrap_or(&base_function.identity.path);
            if !head_paths.contains(head_path) {
                return None;
            }
            if changed_base_paths.contains(&base_function.identity.path) {
                head_functions_by_base_identity
                    .get(&base_function.identity)
                    .map(|function| (*function).clone())
            } else {
                Some(base_function.clone())
            }
        })
        .collect::<Vec<_>>();
    let mut clone_index = CloneIndex::from_functions(clone_candidates);

    for path in &changed.paths {
        let Some(file) = head_files.get(path) else {
            continue;
        };
        let source = repository.read_blob(&head_commit, path)?;
        if file.language == "rust" {
            evaluate_lint_suppressions(
                repository,
                &base_commit,
                path,
                changed.renamed_from.get(path.as_str()),
                changed.added_lines.get(path.as_str()),
                &source,
                config,
                &mut report,
            )?;
            evaluate_unsafe_surface(
                repository,
                &base_commit,
                path,
                changed.renamed_from.get(path.as_str()),
                changed.added_lines.get(path.as_str()),
                &source,
                config,
                &mut report,
            )?;
        }
        let _manifest_path_count = changed.cargo_paths.len();
        let previous_path = changed.renamed_from.get(&file.path);
        for function in &file.functions {
            let base_identity = renamed_identity(&function.identity, previous_path);
            let base_function = base_functions.get(&base_identity).copied();
            let materially_changed =
                base_function.is_none_or(|base| function.normalized_hash != base.normalized_hash);
            if let Some(base_function) = base_function {
                if !materially_changed {
                    clone_index.insert(function.clone());
                    continue;
                }
                let delta = function.mass - base_function.mass;
                if delta > config.rules.function_mass.delta_limit {
                    push_if_enabled(
                        &mut report,
                        delta_finding(
                            function,
                            base_function,
                            delta,
                            config.rules.function_mass.delta_limit,
                        ),
                        config,
                    );
                }
            } else if function.mass > config.rules.function_mass.new_function_limit {
                push_if_enabled(
                    &mut report,
                    new_function_finding(function, config.rules.function_mass.new_function_limit),
                    config,
                );
            }
            if materially_changed {
                for (finding, _) in clone_matches(function, &clone_index, &config.rules.near_clone)
                {
                    push_if_enabled(&mut report, finding, config);
                }
            }
            clone_index.insert(function.clone());
        }
    }
    for path in &changed.cargo_paths {
        evaluate_dependency_surface(
            repository,
            &base_commit,
            path,
            changed.renamed_from.get(path.as_str()),
            &head_commit,
            config,
            &mut report,
        )?;
    }
    if !report
        .findings
        .iter()
        .any(|finding| finding.rule_id == "analysis-error")
    {
        let head_files = repository
            .source_files(&head_commit)?
            .into_iter()
            .map(|path| {
                repository
                    .read_blob(&head_commit, &path)
                    .and_then(|source| analyze_source_file(&path, &source))
            })
            .collect::<Result<Vec<_>>>()?;
        let policy = &config.rules.structural_erosion;
        let rust_head_files = head_files
            .iter()
            .filter(|file| file.language == "rust")
            .cloned()
            .collect::<Vec<_>>();
        let rust_base_files = artifact
            .files
            .iter()
            .filter(|file| file.language == "rust")
            .cloned()
            .collect::<Vec<_>>();
        let head_summary =
            RepositorySummary::from_files_with_cutoff(&rust_head_files, policy.complexity_cutoff);
        let base_summary =
            RepositorySummary::from_files_with_cutoff(&rust_base_files, policy.complexity_cutoff);
        let delta = head_summary.erosion_ratio - base_summary.erosion_ratio;
        // The ratio alone cannot separate "added complexity" from "deleted
        // easy code": removing low-complexity code shrinks the denominator and
        // raises the ratio. Growth of the absolute high-complexity mass is
        // immune to that and is compared alongside the ratio.
        let mass_growth = mass_growth(
            base_summary.high_complexity_mass,
            head_summary.high_complexity_mass,
        );
        let breaches = erosion_breaches(
            &head_summary,
            delta,
            mass_growth,
            policy.erosion_limit,
            policy.delta_limit,
            policy.mass_growth_limit,
        );
        if !breaches.is_empty() {
            let by_cutoff = CUTOFF_DISTRIBUTION
                .iter()
                .map(|cutoff| {
                    (
                        cutoff.to_string(),
                        format!(
                            "{:.6}",
                            RepositorySummary::from_files_with_cutoff(&head_files, *cutoff)
                                .erosion_ratio
                        ),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            push_if_enabled(
                &mut report,
                erosion_finding(ErosionFindingInput {
                    head_files: &head_files,
                    base_functions: &base_functions,
                    renamed_from: &changed.renamed_from,
                    added_lines: &changed.added_lines,
                    head: &head_summary,
                    base: &base_summary,
                    delta,
                    mass_growth,
                    breaches,
                    erosion_by_cutoff: by_cutoff,
                    erosion_limit: policy.erosion_limit,
                    delta_limit: policy.delta_limit,
                    mass_growth_limit: policy.mass_growth_limit,
                    complexity_cutoff: policy.complexity_cutoff,
                    top_contributors: policy.top_contributors,
                }),
                config,
            );
        }
    }
    report.findings.sort_by(finding_order);
    Ok(report)
}

/// Renders a report as stable human-readable text.
pub(crate) fn render_human(report: &CheckReport) -> String {
    if report.findings.is_empty() {
        return "slop-gate: no findings\n".to_string();
    }
    report
        .findings
        .iter()
        .map(|finding| {
            format!(
                "{}:{}: {}[{}]: {}\n",
                finding.location.path,
                finding.location.line,
                severity_name(finding.severity),
                finding.rule_id,
                finding.message
            )
        })
        .collect()
}

/// Returns the duplication findings for one function against the candidate pool.
///
/// Both whole-function and block matches are returned, with the matched
/// identity, so a caller can build clone families from the same data it
/// reports. A pair already reported as a whole-function clone is not reported
/// again for the block that makes up the same code.
///
/// # Arguments
///
/// * `function` - Function to compare against the index.
/// * `clone_index` - Candidate pool of functions still present in head.
/// * `policy` - Near-clone policy.
///
/// # Returns
///
/// Returns each finding with the identity of its match.
fn clone_matches(
    function: &FunctionRecord,
    clone_index: &CloneIndex,
    policy: &NearCloneRule,
) -> Vec<(Finding, FunctionIdentity)> {
    let whole = clone_index.best_match(function, policy).map(
        |(candidate, token_similarity, ast_similarity)| {
            (
                clone_finding(
                    function,
                    &candidate,
                    token_similarity,
                    ast_similarity,
                    policy.similarity_threshold,
                ),
                candidate.identity,
            )
        },
    );
    let mut findings = Vec::new();
    if let Some((finding, identity)) = whole {
        findings.push((finding, identity));
    }
    if !policy.detect_blocks {
        return findings;
    }
    for block in clone_index.best_block_matches(function, policy) {
        if findings
            .iter()
            .any(|(_, identity)| *identity == block.candidate.identity)
        {
            continue;
        }
        findings.push((
            block_clone_finding(function, &block, policy),
            block.candidate.identity,
        ));
    }
    findings
}

fn renamed_identity(
    identity: &FunctionIdentity,
    previous_path: Option<&String>,
) -> FunctionIdentity {
    let mut identity = identity.clone();
    if let Some(path) = previous_path {
        identity.path.clone_from(path);
    }
    identity
}

fn new_function_finding(function: &FunctionRecord, threshold: f64) -> Finding {
    Finding {
        rule_id: "function-mass".to_string(),
        severity: Severity::Error,
        message: format!(
            "new function mass {:.2} exceeds limit {:.2}",
            function.mass, threshold
        ),
        location: Location {
            path: function.identity.path.clone(),
            line: function.start_line,
        },
        base_location: None,
        base_mass: None,
        head_mass: Some(function.mass),
        delta: None,
        threshold: Some(threshold),
        similarity: None,
        properties: BTreeMap::new(),
    }
}

fn delta_finding(
    head: &FunctionRecord,
    base: &FunctionRecord,
    delta: f64,
    threshold: f64,
) -> Finding {
    Finding {
        rule_id: "function-mass".to_string(),
        severity: Severity::Error,
        message: format!(
            "function mass increased by {:.2}; limit is {:.2}",
            delta, threshold
        ),
        location: Location {
            path: head.identity.path.clone(),
            line: head.start_line,
        },
        base_location: Some(Location {
            path: base.identity.path.clone(),
            line: base.start_line,
        }),
        base_mass: Some(base.mass),
        head_mass: Some(head.mass),
        delta: Some(delta),
        threshold: Some(threshold),
        similarity: None,
        properties: BTreeMap::new(),
    }
}

/// Returns the duplication risk of a pair of functions.
///
/// Risk is steepest for production code that is structurally unrelated to
/// boilerplate, and lowest for test or accessor duplication.
///
/// # Arguments
///
/// * `left` - Function at the reported location.
/// * `right` - Matched candidate.
///
/// # Returns
///
/// Returns `high`, `medium`, or `low`.
fn clone_risk(left: &FunctionRecord, right: &FunctionRecord) -> &'static str {
    if left.role.is_low_risk() || right.role.is_low_risk() {
        return "low";
    }
    if left.role == FunctionRole::General && right.role == FunctionRole::General {
        return "high";
    }
    "medium"
}

fn clone_finding(
    head: &FunctionRecord,
    candidate: &FunctionRecord,
    token_similarity: f64,
    ast_similarity: f64,
    threshold: f64,
) -> Finding {
    let similarity = token_similarity.min(ast_similarity);
    let mut properties = BTreeMap::new();
    properties.insert(
        "token_similarity".to_string(),
        format!("{token_similarity:.6}"),
    );
    properties.insert("ast_similarity".to_string(), format!("{ast_similarity:.6}"));
    let (risk, same_file) = duplication_context(head, candidate);
    properties.insert("clone_scope".to_string(), "whole-function".to_string());
    properties.insert("clone_risk".to_string(), risk.to_string());
    properties.insert("left_role".to_string(), head.role.name().to_string());
    properties.insert("right_role".to_string(), candidate.role.name().to_string());
    properties.insert("same_file".to_string(), same_file.to_string());
    Finding {
        rule_id: "near-clone".to_string(),
        severity: Severity::Error,
        message: format!(
            "whole-function structural similarity {:.2}% to {} exceeds threshold {:.2}% [{}]",
            similarity * 100.0,
            candidate.identity.qualified_name,
            threshold * 100.0,
            risk
        ),
        location: Location {
            path: head.identity.path.clone(),
            line: head.start_line,
        },
        base_location: Some(Location {
            path: candidate.identity.path.clone(),
            line: candidate.start_line,
        }),
        base_mass: None,
        head_mass: None,
        delta: None,
        threshold: Some(threshold),
        similarity: Some(similarity),
        properties,
    }
}

/// Builds the finding for a duplicated block inside two large functions.
///
/// # Arguments
///
/// * `head` - Function containing the first island.
/// * `match_facts` - Located block and its candidate.
/// * `policy` - Near-clone thresholds.
///
/// # Returns
///
/// Returns a `near-clone` finding that points at both islands.
fn block_clone_finding(
    head: &FunctionRecord,
    match_facts: &BlockMatch,
    policy: &NearCloneRule,
) -> Finding {
    let candidate = &match_facts.candidate;
    let similarity = match_facts.similarity();
    let (risk, same_file) = duplication_context(head, candidate);
    let mut properties = BTreeMap::new();
    properties.insert("clone_scope".to_string(), "block".to_string());
    properties.insert("clone_risk".to_string(), risk.to_string());
    properties.insert(
        "token_containment".to_string(),
        format!("{:.6}", match_facts.token_containment),
    );
    properties.insert(
        "ast_containment".to_string(),
        format!("{:.6}", match_facts.ast_containment),
    );
    properties.insert(
        "island_shingles".to_string(),
        match_facts.island_shingles.to_string(),
    );
    properties.insert(
        "island_lines".to_string(),
        (match_facts.left_end_line - match_facts.left_start_line + 1).to_string(),
    );
    properties.insert("left_role".to_string(), head.role.name().to_string());
    properties.insert("right_role".to_string(), candidate.role.name().to_string());
    properties.insert("same_file".to_string(), same_file.to_string());
    properties.insert(
        "left_start_line".to_string(),
        match_facts.left_start_line.to_string(),
    );
    properties.insert(
        "right_start_line".to_string(),
        match_facts.right_start_line.to_string(),
    );
    Finding {
        rule_id: "near-clone".to_string(),
        severity: Severity::Error,
        message: format!(
            "block of ~{} statements duplicated at {}:{} [{}]",
            match_facts.left_end_line - match_facts.left_start_line + 1,
            candidate.identity.path,
            match_facts.right_start_line,
            risk
        ),
        location: Location {
            path: head.identity.path.clone(),
            line: match_facts.left_start_line,
        },
        base_location: Some(Location {
            path: candidate.identity.path.clone(),
            line: match_facts.right_start_line,
        }),
        base_mass: None,
        head_mass: None,
        delta: None,
        threshold: Some(policy.similarity_threshold),
        similarity: Some(similarity),
        properties,
    }
}

/// Returns the risk label and file relationship for a clone pair.
///
/// # Arguments
///
/// * `left` - Function at the reported location.
/// * `right` - Matched candidate.
///
/// # Returns
///
/// Returns the risk label and whether both functions share one file.
fn duplication_context(left: &FunctionRecord, right: &FunctionRecord) -> (&'static str, bool) {
    (
        clone_risk(left, right),
        left.identity.path == right.identity.path,
    )
}

/// Complexity cutoffs reported with every structural-erosion finding.
///
/// A single ratio is not interpretable across repositories: one panel median
/// reads 0.41 and another p95 reads 0.81. The distribution shows whether a
/// repository sits high at every cutoff or only at the configured one.
const CUTOFF_DISTRIBUTION: [u32; 5] = [5, 10, 15, 20, 30];

/// Returns growth of absolute high-complexity mass between two revisions.
///
/// # Arguments
///
/// * `base` - High-complexity mass at the base revision.
/// * `head` - High-complexity mass at the head revision.
///
/// # Returns
///
/// Returns relative growth, or zero when both revisions hold no
/// high-complexity mass.
fn mass_growth(base: f64, head: f64) -> f64 {
    if base == 0.0 {
        return f64::from(head > 0.0);
    }
    (head - base) / base
}

/// Returns the erosion conditions a revision breaches.
///
/// # Arguments
///
/// * `head` - Head summary.
/// * `delta` - Change in erosion ratio.
/// * `mass_growth` - Relative growth of high-complexity mass.
/// * `erosion_limit` - Maximum tolerated erosion ratio.
/// * `delta_limit` - Maximum tolerated erosion ratio change.
/// * `mass_growth_limit` - Maximum tolerated high-complexity mass growth.
///
/// # Returns
///
/// Returns the breached condition identifiers in a stable order.
fn erosion_breaches(
    head: &RepositorySummary,
    delta: f64,
    mass_growth: f64,
    erosion_limit: f64,
    delta_limit: f64,
    mass_growth_limit: f64,
) -> Vec<&'static str> {
    let mut breaches = Vec::new();
    if mass_growth > mass_growth_limit {
        breaches.push("mass-growth");
    }
    if delta > delta_limit {
        breaches.push("ratio-delta");
    }
    if head.erosion_ratio > erosion_limit {
        breaches.push("ratio-level");
    }
    breaches
}

struct ErosionFindingInput<'a> {
    head_files: &'a [crate::analysis::AnalyzedFile],
    base_functions: &'a HashMap<FunctionIdentity, &'a FunctionRecord>,
    renamed_from: &'a HashMap<String, String>,
    added_lines: &'a HashMap<String, ChangedLineSet>,
    head: &'a RepositorySummary,
    base: &'a RepositorySummary,
    delta: f64,
    mass_growth: f64,
    breaches: Vec<&'static str>,
    erosion_by_cutoff: BTreeMap<String, String>,
    erosion_limit: f64,
    delta_limit: f64,
    mass_growth_limit: f64,
    complexity_cutoff: u32,
    top_contributors: usize,
}

fn erosion_finding(input: ErosionFindingInput<'_>) -> Finding {
    let ErosionFindingInput {
        head_files,
        base_functions,
        renamed_from,
        added_lines,
        head,
        base,
        delta,
        mass_growth,
        breaches,
        erosion_by_cutoff,
        erosion_limit,
        delta_limit,
        mass_growth_limit,
        complexity_cutoff,
        top_contributors,
    } = input;
    let mut contributors = head_files
        .iter()
        .flat_map(|file| {
            let previous_path = renamed_from.get(&file.path);
            file.functions.iter().filter_map(move |function| {
                let changed_in_function = added_lines.get(&file.path).is_some_and(|lines| {
                    (function.start_line..=function.end_line).any(|line| lines.contains(line))
                });
                if !changed_in_function {
                    return None;
                }
                let identity = renamed_identity(&function.identity, previous_path);
                let base_mass = base_functions.get(&identity).map(|base| base.mass);
                (function.cc > complexity_cutoff
                    && base_mass.is_none_or(|mass| function.mass > mass))
                .then_some((function, base_mass))
            })
        })
        .collect::<Vec<_>>();
    contributors.sort_by(|(left, left_base), (right, right_base)| {
        let left_increase = left.mass - left_base.unwrap_or(0.0);
        let right_increase = right.mass - right_base.unwrap_or(0.0);
        right_increase
            .total_cmp(&left_increase)
            .then_with(|| right.mass.total_cmp(&left.mass))
            .then_with(|| left.identity.path.cmp(&right.identity.path))
            .then_with(|| left.start_line.cmp(&right.start_line))
            .then_with(|| {
                left.identity
                    .qualified_name
                    .cmp(&right.identity.qualified_name)
            })
    });
    let location = contributors
        .first()
        .map(|(function, _)| Location {
            path: function.identity.path.clone(),
            line: function.start_line,
        })
        // A ratio or mass breach can be caused entirely by unchanged code, in
        // which case there is no changed function to attribute. Fall back to the
        // largest high-complexity function in the head revision so the finding
        // still points at real source.
        .or_else(|| {
            head.top_contributors.first().map(|contributor| Location {
                path: contributor.path.clone(),
                line: contributor.line,
            })
        })
        .unwrap_or_else(|| Location {
            path: "<repository>".to_string(),
            line: 1,
        });
    let mut properties = BTreeMap::new();
    properties.insert(
        "base_erosion_ratio".to_string(),
        format!("{:.6}", base.erosion_ratio),
    );
    properties.insert(
        "head_erosion_ratio".to_string(),
        format!("{:.6}", head.erosion_ratio),
    );
    properties.insert("erosion_delta".to_string(), format!("{delta:.6}"));
    properties.insert(
        "base_high_complexity_mass".to_string(),
        format!("{:.6}", base.high_complexity_mass),
    );
    properties.insert(
        "head_high_complexity_mass".to_string(),
        format!("{:.6}", head.high_complexity_mass),
    );
    properties.insert(
        "high_complexity_mass_growth".to_string(),
        format!("{mass_growth:.6}"),
    );
    properties.insert(
        "mass_growth_limit".to_string(),
        format!("{mass_growth_limit:.6}"),
    );
    properties.insert("breaches".to_string(), breaches.join(","));
    for (cutoff, ratio) in &erosion_by_cutoff {
        properties.insert(format!("erosion_at_cc_{cutoff}"), ratio.clone());
    }
    properties.insert(
        "complexity_cutoff".to_string(),
        complexity_cutoff.to_string(),
    );
    properties.insert("erosion_limit".to_string(), format!("{erosion_limit:.6}"));
    properties.insert("delta_limit".to_string(), format!("{delta_limit:.6}"));
    for (index, (function, base_mass)) in contributors.iter().take(top_contributors).enumerate() {
        let prefix = format!("contributor_{}", index + 1);
        properties.insert(format!("{prefix}_path"), function.identity.path.clone());
        properties.insert(
            format!("{prefix}_qualified_name"),
            function.identity.qualified_name.clone(),
        );
        properties.insert(format!("{prefix}_line"), function.start_line.to_string());
        properties.insert(format!("{prefix}_cc"), function.cc.to_string());
        properties.insert(
            format!("{prefix}_head_mass"),
            format!("{:.6}", function.mass),
        );
        properties.insert(
            format!("{prefix}_base_mass"),
            base_mass.map_or_else(|| "new".to_string(), |mass| format!("{mass:.6}")),
        );
    }
    Finding {
        rule_id: "structural-erosion".to_string(),
        severity: Severity::Error,
        message: format!(
            "structural erosion at {:.2}% (base {:.2}%, ratio delta {:+.2}%, high-complexity mass {:+.2}% at CC>{}) [{}]",
            head.erosion_ratio * 100.0,
            base.erosion_ratio * 100.0,
            delta * 100.0,
            mass_growth * 100.0,
            complexity_cutoff,
            breaches.join(",")
        ),
        location,
        base_location: None,
        base_mass: Some(base.high_complexity_mass),
        head_mass: Some(head.high_complexity_mass),
        delta: Some(delta),
        threshold: Some(delta_limit),
        similarity: None,
        properties,
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate_lint_suppressions(
    repository: &GitRepository,
    base: &str,
    path: &str,
    renamed_from: Option<&String>,
    added_lines: Option<&ChangedLineSet>,
    head_source: &str,
    config: &GateConfig,
    report: &mut CheckReport,
) -> Result<()> {
    let Some(added_lines) = added_lines else {
        return Ok(());
    };
    let head_facts = lint_suppressions(path, head_source)?;
    let base_path = renamed_from.map_or(path, String::as_str);
    let base_facts = repository
        .read_blob_if_exists(base, base_path)?
        .map(|source| lint_suppressions(base_path, &source))
        .transpose()?
        .unwrap_or_default();
    for fact in head_facts {
        if !added_lines.contains(fact.line) {
            continue;
        }
        let exact = base_facts.iter().any(|base| {
            base.kind == fact.kind
                && base.lint_paths == fact.lint_paths
                && base.target_fingerprint == fact.target_fingerprint
        });
        let broadened = base_facts.iter().any(|base| {
            base.kind == fact.kind
                && base.target_fingerprint == fact.target_fingerprint
                && base
                    .lint_paths
                    .iter()
                    .all(|lint| fact.lint_paths.contains(lint))
                && base.lint_paths.len() < fact.lint_paths.len()
        });
        if exact {
            continue;
        }
        let mut properties = BTreeMap::new();
        properties.insert("pattern_id".to_string(), fact.kind.clone());
        properties.insert("lint_paths".to_string(), fact.lint_paths.join(","));
        properties.insert("target_kind".to_string(), fact.target_kind.clone());
        push_if_enabled(
            report,
            Finding {
                rule_id: "lint-suppression-growth".to_string(),
                severity: Severity::Warning,
                message: if broadened {
                    "lint suppression broadened".to_string()
                } else {
                    "new lint suppression introduced".to_string()
                },
                location: Location {
                    path: path.to_string(),
                    line: fact.line,
                },
                base_location: None,
                base_mass: None,
                head_mass: None,
                delta: None,
                threshold: None,
                similarity: None,
                properties,
            },
            config,
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn evaluate_unsafe_surface(
    repository: &GitRepository,
    base: &str,
    path: &str,
    renamed_from: Option<&String>,
    added_lines: Option<&ChangedLineSet>,
    source: &str,
    config: &GateConfig,
    report: &mut CheckReport,
) -> Result<()> {
    let Some(added_lines) = added_lines else {
        return Ok(());
    };
    let base_path = renamed_from.map_or(path, String::as_str);
    let mut base_facts = repository
        .read_blob_if_exists(base, base_path)?
        .map(|source| unsafe_surface(base_path, &source))
        .transpose()?
        .unwrap_or_default();
    let head_facts = unsafe_surface(path, source)?;
    let pending = head_facts
        .into_iter()
        .filter(|fact| added_lines.contains(fact.line))
        .collect::<Vec<_>>();
    let mut unmatched = Vec::new();
    for fact in &pending {
        if let Some(position) = base_facts.iter().position(|base| {
            base.pattern_id == fact.pattern_id && base.fingerprint == fact.fingerprint
        }) {
            base_facts.remove(position);
        } else {
            unmatched.push(fact.clone());
        }
    }
    let mut introduced = Vec::new();
    for fact in unmatched {
        if let Some(position) = base_facts
            .iter()
            .position(|base| base.pattern_id == fact.pattern_id && base.line == fact.line)
        {
            base_facts.remove(position);
        } else {
            introduced.push(fact);
        }
    }
    for fact in introduced {
        let mut properties = BTreeMap::new();
        properties.insert("pattern_id".to_string(), fact.pattern_id.clone());
        push_if_enabled(
            report,
            Finding {
                rule_id: "unsafe-surface-growth".to_string(),
                severity: Severity::Warning,
                message: format!("new {} introduced", fact.pattern_id),
                location: Location {
                    path: path.to_string(),
                    line: fact.line,
                },
                base_location: None,
                base_mass: None,
                head_mass: None,
                delta: None,
                threshold: None,
                similarity: None,
                properties,
            },
            config,
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn evaluate_dependency_surface(
    repository: &GitRepository,
    base: &str,
    path: &str,
    renamed_from: Option<&String>,
    head: &str,
    config: &GateConfig,
    report: &mut CheckReport,
) -> Result<()> {
    let head_source = repository.read_blob(head, path)?;
    let head_edges = dependency_edges(&head_source)?;
    let base_path = renamed_from.map_or(path, String::as_str);
    let base_edges = repository
        .read_blob_if_exists(base, base_path)?
        .map(|source| dependency_edges(&source))
        .transpose()?
        .unwrap_or_default();
    for edge in head_edges {
        let base_edge = base_edges.iter().find(|candidate| {
            candidate.table_path == edge.table_path
                && candidate.dependency_key == edge.dependency_key
        });
        let Some(base_edge) = base_edge else {
            dependency_finding(report, path, &edge, "new-dependency", config);
            continue;
        };
        let features_expanded = edge
            .features
            .iter()
            .any(|feature| !base_edge.features.contains(feature));
        let defaults_expanded = !base_edge.default_features && edge.default_features;
        let declaration_expanded = base_edge.package != edge.package
            || base_edge.source_kind != edge.source_kind
            || base_edge.source != edge.source;
        if features_expanded || defaults_expanded || declaration_expanded {
            dependency_finding(report, path, &edge, "expanded-dependency-surface", config);
        }
    }
    Ok(())
}

fn dependency_finding(
    report: &mut CheckReport,
    path: &str,
    edge: &crate::analysis::DependencyEdge,
    pattern_id: &str,
    config: &GateConfig,
) {
    let mut properties = BTreeMap::new();
    properties.insert("pattern_id".to_string(), pattern_id.to_string());
    properties.insert("dependency_key".to_string(), edge.dependency_key.clone());
    properties.insert("table_path".to_string(), edge.table_path.clone());
    properties.insert("source_kind".to_string(), edge.source_kind.clone());
    properties.insert(
        "default_features".to_string(),
        edge.default_features.to_string(),
    );
    properties.insert("features".to_string(), edge.features.join(","));
    if let Some(package) = &edge.package {
        properties.insert("package".to_string(), package.clone());
    }
    if let Some(source) = &edge.source {
        properties.insert("source".to_string(), source.clone());
    }
    if let Some(version) = &edge.version {
        properties.insert("version".to_string(), version.clone());
    }
    push_if_enabled(
        report,
        Finding {
            rule_id: "dependency-surface-growth".to_string(),
            severity: Severity::Warning,
            message: format!("{pattern_id}: {}", edge.dependency_key),
            location: Location {
                path: path.to_string(),
                line: edge.line,
            },
            base_location: None,
            base_mass: None,
            head_mass: None,
            delta: None,
            threshold: None,
            similarity: None,
            properties,
        },
        config,
    );
}

/// Scans all functions in an artifact for structural duplication.
pub(crate) fn scan_clones(artifact: &IndexArtifact, config: &GateConfig) -> CheckReport {
    let matches = collect_scan_matches(artifact, config);
    let records = function_records(artifact);
    let families = clone_families(&matches, &records);
    let block_members = block_members(&matches);
    let mut report = CheckReport::default();
    add_pair_findings(&mut report, matches, &families, config);
    add_family_summaries(&mut report, &families, &block_members, config);
    report.findings.sort_by(finding_order);
    report
}

fn collect_scan_matches(
    artifact: &IndexArtifact,
    config: &GateConfig,
) -> Vec<(Finding, FunctionIdentity, FunctionIdentity)> {
    let mut index = CloneIndex::default();
    let mut matches = Vec::new();
    for function in artifact.files.iter().flat_map(|file| file.functions.iter()) {
        for (finding, partner) in clone_matches(function, &index, &config.rules.near_clone) {
            matches.push((finding, function.identity.clone(), partner));
        }
        index.insert(function.clone());
    }
    matches
}

fn function_records(artifact: &IndexArtifact) -> HashMap<FunctionIdentity, FunctionRecord> {
    artifact
        .files
        .iter()
        .flat_map(|file| file.functions.iter())
        .map(|function| (function.identity.clone(), function.clone()))
        .collect()
}

fn add_pair_findings(
    report: &mut CheckReport,
    matches: Vec<(Finding, FunctionIdentity, FunctionIdentity)>,
    families: &HashMap<FunctionIdentity, CloneFamily>,
    config: &GateConfig,
) {
    for (mut finding, left, _right) in matches {
        if let Some(family) = families.get(&left) {
            finding
                .properties
                .insert("clone_family_id".to_string(), family.id.clone());
        }
        push_if_enabled(report, finding, config);
    }
}

/// Returns the duplication risk of a clone family.
///
/// # Arguments
///
/// * `members` - Functions in the family.
///
/// # Returns
///
/// Returns `high` when no member is boilerplate, `low` when every member is,
/// and `medium` otherwise.
fn family_risk_label(members: &[FunctionRecord]) -> &'static str {
    let production = members
        .iter()
        .filter(|member| !member.role.is_low_risk())
        .count();
    match production {
        0 => "low",
        count if count == members.len() => "high",
        _ => "medium",
    }
}

/// Returns the distinct roles present in a clone family.
///
/// # Arguments
///
/// * `members` - Functions in the family.
///
/// # Returns
///
/// Returns the role names in a stable, comma-separated order.
fn family_roles(members: &[FunctionRecord]) -> String {
    members
        .iter()
        .map(|member| member.role.name())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(",")
}

/// Returns the functions that took part in at least one block match.
fn block_members(
    matches: &[(Finding, FunctionIdentity, FunctionIdentity)],
) -> HashSet<FunctionIdentity> {
    matches
        .iter()
        .filter(|(finding, _, _)| {
            finding
                .properties
                .get("clone_scope")
                .is_some_and(|scope| scope == "block")
        })
        .flat_map(|(_, left, right)| [left.clone(), right.clone()])
        .collect()
}

fn add_family_summaries(
    report: &mut CheckReport,
    families: &HashMap<FunctionIdentity, CloneFamily>,
    block_members: &HashSet<FunctionIdentity>,
    config: &GateConfig,
) {
    let mut emitted_families = HashSet::new();
    for family in families.values() {
        if family.members.len() < 2 {
            continue;
        }
        if !emitted_families.insert(family.id.clone()) {
            continue;
        }
        let first = &family.members[0];
        let second = &family.members[1];
        let risk_label = family_risk_label(&family.members);
        let roles = family_roles(&family.members);
        let mut properties = BTreeMap::new();
        properties.insert("finding_kind".to_string(), "family-summary".to_string());
        properties.insert("clone_family_id".to_string(), family.id.clone());
        properties.insert("member_count".to_string(), family.members.len().to_string());
        properties.insert("family_mass".to_string(), format!("{:.6}", family.mass));
        properties.insert(
            "duplicate_mass".to_string(),
            format!("{:.6}", family.recoverable()),
        );
        properties.insert("clone_risk".to_string(), risk_label.to_string());
        properties.insert("member_roles".to_string(), roles);
        properties.insert(
            "has_block_match".to_string(),
            family
                .members
                .iter()
                .any(|member| block_members.contains(&member.identity))
                .to_string(),
        );
        push_if_enabled(
            report,
            Finding {
                rule_id: "near-clone".to_string(),
                severity: Severity::Error,
                message: format!(
                    "clone family of {} functions, {:.2} recoverable mass of {:.2} family mass [{}]",
                    family.members.len(),
                    family.recoverable(),
                    family.mass,
                    risk_label
                ),
                location: Location {
                    path: first.identity.path.clone(),
                    line: first.start_line,
                },
                base_location: Some(Location {
                    path: second.identity.path.clone(),
                    line: second.start_line,
                }),
                base_mass: None,
                head_mass: None,
                delta: None,
                threshold: Some(config.rules.near_clone.similarity_threshold),
                similarity: None,
                properties,
            },
            config,
        );
    }
}

/// Limits exploratory scan output to clone pairs while retaining their summaries.
pub(crate) fn limit_scan_report(report: &mut CheckReport, top: usize) -> Result<()> {
    if top == 0 {
        return Err(Error::invalid("scan top", "must be positive"));
    }
    let (mut pairs, summaries): (Vec<_>, Vec<_>) = report
        .findings
        .drain(..)
        .partition(|finding| !finding.properties.contains_key("finding_kind"));
    let selected_families = pairs
        .iter()
        .take(top)
        .filter_map(|finding| finding.properties.get("clone_family_id").cloned())
        .collect::<HashSet<_>>();
    pairs.truncate(top);
    report.findings = pairs;
    report
        .findings
        .extend(summaries.into_iter().filter(|finding| {
            finding
                .properties
                .get("clone_family_id")
                .is_some_and(|family_id| selected_families.contains(family_id))
        }));
    Ok(())
}

struct CloneFamily {
    id: String,
    members: Vec<FunctionRecord>,
    mass: f64,
    recoverable_mass: f64,
}

impl CloneFamily {
    /// Returns the mass a refactor could plausibly remove from the family.
    ///
    /// The family total counts every member, including the copy that would be
    /// kept. Only the total minus the largest member is recoverable, so
    /// reporting the raw total overstates the available win by roughly the size
    /// of the surviving function.
    ///
    /// # Returns
    ///
    /// Returns the recoverable mass, never below zero.
    fn recoverable(&self) -> f64 {
        self.recoverable_mass
    }
}

fn clone_families(
    matches: &[(Finding, FunctionIdentity, FunctionIdentity)],
    records: &HashMap<FunctionIdentity, FunctionRecord>,
) -> HashMap<FunctionIdentity, CloneFamily> {
    let mut edges = Vec::new();
    for (_finding, left, right) in matches {
        edges.push((left.clone(), right.clone()));
    }
    let mut groups = HashMap::<FunctionIdentity, Vec<FunctionIdentity>>::new();
    for identity in records.keys() {
        let mut group = vec![identity.clone()];
        let mut changed = true;
        while changed {
            changed = false;
            for (left, right) in &edges {
                if group.contains(left) || group.contains(right) {
                    if !group.contains(left) {
                        group.push(left.clone());
                        changed = true;
                    }
                    if !group.contains(right) {
                        group.push(right.clone());
                        changed = true;
                    }
                }
            }
        }
        group.sort_by(identity_order);
        if let Some(first) = group.first() {
            groups.insert(first.clone(), group);
        }
    }
    groups
        .into_values()
        .map(|group| {
            let id = format!("CF-{:016x}", stable_family_hash(&group));
            let records = group
                .iter()
                .filter_map(|identity| records.get(identity).cloned())
                .collect::<Vec<_>>();
            let mass: f64 = records.iter().map(|record| record.mass).sum();
            let largest = records
                .iter()
                .map(|record| record.mass)
                .fold(0.0_f64, f64::max);
            let recoverable_mass = (mass - largest).max(0.0);
            (
                group,
                CloneFamily {
                    id,
                    members: records,
                    mass,
                    recoverable_mass,
                },
            )
        })
        .flat_map(|(group, family)| {
            group.into_iter().map(move |identity| {
                (
                    identity,
                    CloneFamily {
                        id: family.id.clone(),
                        members: family.members.clone(),
                        mass: family.mass,
                        recoverable_mass: family.recoverable_mass,
                    },
                )
            })
        })
        .collect()
}

fn identity_order(left: &FunctionIdentity, right: &FunctionIdentity) -> std::cmp::Ordering {
    (&left.path, &left.qualified_name).cmp(&(&right.path, &right.qualified_name))
}

fn stable_family_hash(members: &[FunctionIdentity]) -> u64 {
    let mut input = String::new();
    for member in members {
        input.push_str(&member.path);
        input.push('\0');
        input.push_str(&member.qualified_name);
        input.push('\0');
    }
    let hash = blake3::hash(input.as_bytes());
    let mut bytes = [0; 8];
    bytes.copy_from_slice(&hash.as_bytes()[..8]);
    u64::from_le_bytes(bytes)
}

/// Maximum candidate positions inspected when locating one island.
///
/// The anchor shingle is common in idiomatic code, so the search is bounded and
/// takes the best of the first few occurrences.
const ISLAND_SEARCH_LIMIT: usize = 8;

/// One duplicated block of statements found inside a function.
///
/// Block detection is function-independent: the same contiguous run of
/// normalized shingles is located in two different functions even when the
/// surrounding functions are large and dissimilar.
#[derive(Debug, Clone, PartialEq)]
struct BlockMatch {
    candidate: FunctionRecord,
    token_containment: f64,
    ast_containment: f64,
    island_shingles: usize,
    left_start_line: usize,
    left_end_line: usize,
    right_start_line: usize,
    right_end_line: usize,
}

impl BlockMatch {
    /// Returns the minimum of the token and AST containment scores.
    ///
    /// # Returns
    ///
    /// Returns the block similarity used for threshold comparison.
    fn similarity(&self) -> f64 {
        self.token_containment.min(self.ast_containment)
    }
}

#[derive(Debug, Default)]
struct CloneIndex {
    candidates: Vec<FunctionRecord>,
    by_shingle: HashMap<u32, Vec<usize>>,
    identities: HashSet<FunctionIdentity>,
}

impl CloneIndex {
    fn from_functions(functions: impl IntoIterator<Item = FunctionRecord>) -> Self {
        let mut index = Self::default();
        for function in functions {
            index.insert(function);
        }
        index
    }

    /// Adds a function to the index, ignoring an identity already present.
    ///
    /// A changed file contributes its head version twice: once when the
    /// candidate pool is built and once when the head function is evaluated.
    /// Indexing it twice would report the same duplication against itself.
    fn insert(&mut self, function: FunctionRecord) {
        if !self.identities.insert(function.identity.clone()) {
            return;
        }
        let id = self.candidates.len();
        for hash in &function.shingle_hashes {
            self.by_shingle.entry(*hash).or_default().push(id);
        }
        self.candidates.push(function);
    }

    fn best_match(
        &self,
        function: &FunctionRecord,
        policy: &NearCloneRule,
    ) -> Option<(FunctionRecord, f64, f64)> {
        self.matches(function, policy).into_iter().next()
    }

    /// Returns the duplicated blocks this function shares with other functions.
    ///
    /// Whole-function similarity dilutes as functions grow, so a byte-identical
    /// block inside two large functions scores near zero. This comparison is
    /// function-independent: it looks for the longest contiguous run of
    /// shingles that also occurs in the candidate, and scores that run against
    /// the configured minimum run length.
    ///
    /// # Arguments
    ///
    /// * `function` - Function whose blocks are compared against the index.
    /// * `policy` - Near-clone thresholds, including the block candidate budget.
    ///
    /// # Returns
    ///
    /// Returns at most `block_max_families` matches above the similarity
    /// threshold, ordered by descending similarity.
    fn best_block_matches(
        &self,
        function: &FunctionRecord,
        policy: &NearCloneRule,
    ) -> Vec<BlockMatch> {
        let (Some(min_run), Some(island_floor)) =
            (block_minimum_run(policy), policy.block_minimum_tokens())
        else {
            return Vec::new();
        };
        let floor = island_floor.saturating_mul(2);
        if function.token_count < floor {
            return Vec::new();
        }
        let query_tokens = sorted_unique(&function.token_shingle_sequence);
        let mut matches = self
            .candidate_ids(function)
            .into_iter()
            .take(policy.block_max_candidates)
            .filter_map(|id| {
                let candidate = &self.candidates[id];
                (candidate.identity != function.identity && candidate.token_count >= floor)
                    .then(|| score_block_match(function, candidate, &query_tokens, min_run, policy))
                    .flatten()
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| {
            right
                .similarity()
                .total_cmp(&left.similarity())
                .then_with(|| right.island_shingles.cmp(&left.island_shingles))
                .then_with(|| candidate_key(&left.candidate).cmp(&candidate_key(&right.candidate)))
        });
        matches.truncate(policy.block_max_families);
        matches
    }

    fn matches(
        &self,
        function: &FunctionRecord,
        policy: &NearCloneRule,
    ) -> Vec<(FunctionRecord, f64, f64)> {
        if function.sloc < policy.minimum_sloc || function.token_count < policy.minimum_tokens {
            return Vec::new();
        }
        let mut matches = self
            .candidate_ids(function)
            .into_iter()
            .take(policy.max_candidates)
            .filter_map(|id| self.score_candidate(id, function, policy))
            .collect::<Vec<_>>();
        matches.sort_by(
            |(left, left_token, left_ast), (right, right_token, right_ast)| {
                right_token
                    .min(*right_ast)
                    .total_cmp(&left_token.min(*left_ast))
                    .then_with(|| candidate_key(left).cmp(&candidate_key(right)))
            },
        );
        matches
    }

    fn candidate_ids(&self, function: &FunctionRecord) -> Vec<usize> {
        let mut overlap_counts = HashMap::<usize, usize>::new();
        for hash in &function.shingle_hashes {
            if let Some(ids) = self.by_shingle.get(hash) {
                for id in ids {
                    if self.candidates[*id].identity.language == function.identity.language {
                        *overlap_counts.entry(*id).or_default() += 1;
                    }
                }
            }
        }
        let mut ids = overlap_counts.into_iter().collect::<Vec<_>>();
        ids.sort_by(|(left_id, left_count), (right_id, right_count)| {
            right_count.cmp(left_count).then_with(|| {
                candidate_key(&self.candidates[*left_id])
                    .cmp(&candidate_key(&self.candidates[*right_id]))
            })
        });
        ids.into_iter().map(|(id, _)| id).collect()
    }

    fn score_candidate(
        &self,
        id: usize,
        function: &FunctionRecord,
        policy: &NearCloneRule,
    ) -> Option<(FunctionRecord, f64, f64)> {
        let candidate = &self.candidates[id];
        if candidate.identity == function.identity
            || candidate.identity.language != function.identity.language
            || candidate.sloc < policy.minimum_sloc
            || candidate.token_count < policy.minimum_tokens
        {
            return None;
        }
        let token_similarity = if candidate.normalized_hash == function.normalized_hash {
            1.0
        } else {
            jaccard_similarity(&function.shingle_hashes, &candidate.shingle_hashes)
        };
        let ast_similarity = if candidate.ast_hash == function.ast_hash {
            1.0
        } else {
            jaccard_similarity(&function.ast_shingle_hashes, &candidate.ast_shingle_hashes)
        };
        (token_similarity >= policy.similarity_threshold
            && ast_similarity >= policy.similarity_threshold)
            .then(|| (candidate.clone(), token_similarity, ast_similarity))
    }
}

/// Scores the longest block two functions share.
///
/// # Arguments
///
/// * `function` - Query function holding the island.
/// * `candidate` - Candidate function to compare against.
/// * `query_tokens` - Sorted unique token shingles of the query.
/// * `min_run` - Shortest run that qualifies as a duplicated block.
/// * `policy` - Near-clone thresholds.
///
/// # Returns
///
/// Returns the located island, or `None` when the pair shares no block that
/// clears the similarity threshold in both streams.
fn score_block_match(
    function: &FunctionRecord,
    candidate: &FunctionRecord,
    query_tokens: &[u32],
    min_run: usize,
    policy: &NearCloneRule,
) -> Option<BlockMatch> {
    let token_run = longest_shared_run(
        &function.token_shingle_sequence,
        &candidate.shingle_hashes,
        min_run,
    )?;
    let ast_run = longest_shared_run(
        &function.ast_shingle_sequence,
        &candidate.ast_shingle_hashes,
        min_run,
    )?;
    let right = locate_shared_island(
        &candidate.token_shingle_sequence,
        query_tokens,
        function.token_shingle_sequence[token_run.0],
        min_run,
    )?;
    // The island is only duplicated as far as both sides agree, so the shorter
    // of the two runs sets the score.
    let island_shingles = token_run.1.min(right.1);
    let token_containment = containment(island_shingles, min_run);
    let ast_containment = containment(ast_run.1.min(island_shingles), min_run);
    if token_containment < policy.similarity_threshold
        || ast_containment < policy.similarity_threshold
    {
        return None;
    }
    let right_start = right.0;
    Some(BlockMatch {
        candidate: candidate.clone(),
        token_containment,
        ast_containment,
        island_shingles,
        left_start_line: function.token_line_at(token_run.0),
        left_end_line: function.token_line_at(token_run.0 + token_run.1 - 1),
        right_start_line: candidate.token_line_at(right_start),
        right_end_line: candidate.token_line_at(right_start + token_run.1 - 1),
    })
}

/// Returns the shortest shingle run eligible for block comparison.
///
/// A run of `n` shingles spans `n + SHINGLE_SIZE - 1` normalized tokens, so the
/// block token floor converts directly to a run length.
///
/// # Arguments
///
/// * `policy` - Near-clone thresholds.
///
/// # Returns
///
/// Returns the minimum run length, or `None` when the policy cannot express one.
fn block_minimum_run(policy: &NearCloneRule) -> Option<usize> {
    policy
        .block_minimum_tokens()?
        .checked_sub(SHINGLE_SIZE - 1)
        .filter(|run| *run > 0)
}

/// Scores a shared run against the minimum run length.
///
/// # Arguments
///
/// * `run_length` - Length of the contiguous shared shingle run.
/// * `min_run` - Minimum eligible run length.
///
/// # Returns
///
/// Returns the run length as a fraction of the minimum, capped at one.
fn containment(run_length: usize, min_run: usize) -> f64 {
    (run_length as f64 / min_run as f64).min(1.0)
}

/// Returns a sorted, deduplicated copy of a shingle sequence.
///
/// # Arguments
///
/// * `sequence` - Ordered shingle hashes.
///
/// # Returns
///
/// Returns the unique values in ascending order.
fn sorted_unique(sequence: &[u32]) -> Vec<u32> {
    let mut sorted = sequence.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    sorted
}

/// Returns the longest contiguous run of shingles also present in `other`.
///
/// # Arguments
///
/// * `sequence` - Ordered shingle hashes of the query function.
/// * `other` - Sorted unique shingle hashes of the candidate.
/// * `min_run` - Shortest run that qualifies as a duplicated block.
///
/// # Returns
///
/// Returns the run start position and run length, or `None` when no run is long
/// enough.
fn longest_shared_run(sequence: &[u32], other: &[u32], min_run: usize) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    let mut start = 0;
    let mut length = 0;
    for (index, hash) in sequence.iter().enumerate() {
        if other.binary_search(hash).is_ok() {
            if length == 0 {
                start = index;
            }
            length += 1;
            if length >= min_run && best.is_none_or(|(_, best_length)| length > best_length) {
                best = Some((start, length));
            }
        } else {
            length = 0;
        }
    }
    best
}

/// Returns where the candidate holds the same island as the query.
///
/// The candidate's copy of the island need not be the same length as the
/// query's: shingle windows straddle statement boundaries, so the two runs
/// start at different offsets and can end at different statements. The longest
/// candidate run anchored at the same shingle is therefore located, and the
/// caller scores the overlap both sides agree on.
///
/// # Arguments
///
/// * `candidate_sequence` - Ordered shingle hashes of the candidate.
/// * `query_set` - Sorted unique shingle hashes of the query function.
/// * `anchor` - Shingle hash that opens the island in the query.
/// * `min_run` - Shortest run that qualifies as a duplicated block.
///
/// # Returns
///
/// Returns the candidate's island start position and run length, or `None` when
/// no candidate run reaches the minimum.
fn locate_shared_island(
    candidate_sequence: &[u32],
    query_set: &[u32],
    anchor: u32,
    min_run: usize,
) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    let mut inspected = 0;
    for (index, hash) in candidate_sequence.iter().enumerate() {
        if *hash != anchor {
            continue;
        }
        inspected += 1;
        if inspected > ISLAND_SEARCH_LIMIT {
            break;
        }
        let mut length = 0;
        while let Some(value) = candidate_sequence.get(index + length) {
            if query_set.binary_search(value).is_err() {
                break;
            }
            length += 1;
        }
        if length >= min_run && best.is_none_or(|(_, best_length)| length > best_length) {
            best = Some((index, length));
        }
    }
    best
}

fn jaccard_similarity(left: &[u32], right: &[u32]) -> f64 {
    let mut left_index = 0;
    let mut right_index = 0;
    let mut shared = 0;
    while left_index < left.len() && right_index < right.len() {
        match left[left_index].cmp(&right[right_index]) {
            std::cmp::Ordering::Less => left_index += 1,
            std::cmp::Ordering::Greater => right_index += 1,
            std::cmp::Ordering::Equal => {
                shared += 1;
                left_index += 1;
                right_index += 1;
            }
        }
    }
    let union = left.len() + right.len() - shared;
    if union == 0 {
        0.0
    } else {
        shared as f64 / union as f64
    }
}

fn candidate_key(function: &FunctionRecord) -> (&str, usize, &str) {
    (
        &function.identity.path,
        function.start_line,
        &function.identity.qualified_name,
    )
}

fn finding_order(left: &Finding, right: &Finding) -> std::cmp::Ordering {
    (
        &left.location.path,
        left.location.line,
        &left.rule_id,
        left.base_location.as_ref().map(|location| &location.path),
    )
        .cmp(&(
            &right.location.path,
            right.location.line,
            &right.rule_id,
            right.base_location.as_ref().map(|location| &location.path),
        ))
}

fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Warning => "warning",
        Severity::Error => "error",
    }
}

fn push_if_enabled(report: &mut CheckReport, mut finding: Finding, config: &GateConfig) {
    let policy = match finding.rule_id.as_str() {
        "function-mass" => config.rules.function_mass.severity,
        "near-clone" => config.rules.near_clone.severity,
        "lint-suppression-growth" => config.rules.lint_suppression.severity,
        "unsafe-surface-growth" => config.rules.unsafe_surface.severity,
        "dependency-surface-growth" => config.rules.dependency_surface.severity,
        "structural-erosion" => config.rules.structural_erosion.severity,
        _ => return report.findings.push(finding),
    };
    finding.severity = match policy {
        RuleSeverity::Off => return,
        RuleSeverity::Warn => Severity::Warning,
        RuleSeverity::Error => Severity::Error,
    };
    if !config.is_suppressed(
        &finding.rule_id,
        &finding.location.path,
        finding.location.line,
    ) {
        report.findings.push(finding);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        CheckReport, CloneIndex, Severity, bind_policy, build_artifact, check_mass,
        limit_scan_report, mass_growth, scan_clones,
    };
    use crate::analysis::{IndexArtifact, analyze_rust_file};
    use crate::config::GateConfig;
    use crate::git::GitRepository;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestRepository {
        path: PathBuf,
    }

    impl TestRepository {
        fn new(source: &str) -> Self {
            let nonce = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "slop-gate-gate-test-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(path.join("src")).unwrap();
            git(&path, ["init", "--quiet"]);
            git(&path, ["config", "user.email", "test@example.invalid"]);
            git(&path, ["config", "user.name", "Slop Gate Test"]);
            fs::write(path.join("src/lib.rs"), source).unwrap();
            git(&path, ["add", "src/lib.rs"]);
            git(&path, ["commit", "--quiet", "-m", "base"]);
            Self { path }
        }

        fn commit_source(&self, source: &str) {
            fs::write(self.path.join("src/lib.rs"), source).unwrap();
            git(&self.path, ["add", "src/lib.rs"]);
            git(&self.path, ["commit", "--quiet", "-m", "head"]);
        }

        fn commit_new_file(&self, relative_path: &str, source: &str) {
            let path = self.path.join(relative_path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, source).unwrap();
            git(&self.path, ["add", relative_path]);
            git(&self.path, ["commit", "--quiet", "-m", "head"]);
        }

        fn delete_source(&self) {
            git(&self.path, ["rm", "--quiet", "src/lib.rs"]);
            git(&self.path, ["commit", "--quiet", "-m", "delete"]);
        }

        fn repository(&self) -> GitRepository {
            GitRepository::open(&self.path).unwrap()
        }
    }

    impl Drop for TestRepository {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn detects_mass_growth_in_a_changed_function() {
        let repository = TestRepository::new("fn parse(flag: bool) { if flag {} }\n");
        let git_repo = repository.repository();
        let artifact = build_artifact(&git_repo, "HEAD").unwrap();
        let body = std::iter::repeat_n("    if flag {}\n", 12).collect::<String>();
        repository.commit_source(&format!("fn parse(flag: bool) {{\n{body}}}\n"));

        let report = check_mass(
            &git_repo,
            "HEAD~1",
            "HEAD",
            &artifact,
            &GateConfig::default(),
        )
        .unwrap();
        assert_eq!(
            report
                .findings
                .iter()
                .filter(|finding| finding.rule_id == "function-mass")
                .count(),
            1
        );
        assert!(!report.has_errors());
    }

    /// Returns a duplicated statement block long enough to clear the block floor.
    fn shared_island() -> String {
        let mut island = String::from(
            "    let count_embed = model.count_embed(&batch);\n    let logits = count_embed.forward(&engine)?;\n    let flat = flatten(&logits, &span_rep, &span_mask)?;\n    let masks = flatten_mask(&span_mask, &spans_idx)?;\n    if flat.is_empty() { return Ok(Vec::new()); }\n",
        );
        for index in 0..20 {
            island.push_str(&format!(
                "    let projected_{index} = project(&struct_proj, &flat, &masks, offset_{index})?;\n"
            ));
        }
        island
    }

    /// Returns structurally distinct filler so the enclosing functions stay
    /// dissimilar as whole functions.
    fn arithmetic_filler() -> String {
        (0..40)
            .map(|index| {
                format!("    let a{index} = base_{index} * scale_{index} + offset_{index};\n")
            })
            .collect()
    }

    fn dispatch_filler() -> String {
        (0..40)
            .map(|index| {
                format!(
                    "    match state_{index} {{\n        State::Ready => emit_{index}(sink)?,\n        State::Busy => {{}}\n        _ => {{}}\n    }}\n"
                )
            })
            .collect()
    }

    fn island_source() -> String {
        let island = shared_island();
        format!(
            "fn extract_relations_from_output() -> Result<Vec<u8>, ()> {{\n{}{island}    let _done = 1;\n    Ok(vec![0])\n}}\n\nfn extract_structures_from_output() -> Result<Vec<u8>, ()> {{\n{}{island}    let _done = 2;\n    Ok(vec![1])\n}}\n",
            arithmetic_filler(),
            dispatch_filler()
        )
    }

    fn scan_source(source: &str) -> CheckReport {
        let file = analyze_rust_file("src/inference/engine.rs", source).unwrap();
        let artifact = IndexArtifact::new("a".repeat(40), vec![file]).unwrap();
        scan_clones(&artifact, &GateConfig::default())
    }

    #[test]
    fn triaged_noise_families_produce_no_block_findings() {
        // Shapes an adopter triaged by hand: mirrored gather kernels, Kleene-3
        // conjunction and disjunction, struct-variant constructors, and
        // parallel table tests. Whole-function reporting of these is expected;
        // block reporting is not.
        let fixture = include_str!("../tests/fixtures/noise_families.rs");
        let report = scan_source(fixture);
        let blocks = report
            .findings
            .iter()
            .filter(|finding| finding.properties.get("clone_scope") == Some(&"block".to_string()))
            .collect::<Vec<_>>();
        assert!(
            blocks.is_empty(),
            "triaged noise must not produce block findings: {blocks:?}"
        );
    }

    #[test]
    fn a_changed_file_does_not_report_its_own_head_copy() {
        // A changed file contributes its head version to the candidate pool and
        // is evaluated again as a query, so an unguarded index holds two copies
        // and reports the same island twice.
        let source = island_source();
        let (base, _) = source
            .split_once("fn extract_structures_from_output")
            .unwrap();
        let repository = TestRepository::new(base);
        let git_repo = repository.repository();
        let mut artifact = build_artifact(&git_repo, "HEAD").unwrap();
        repository.commit_source(&source);
        let config = GateConfig::default();
        bind_policy(&mut artifact, &config).unwrap();

        let report = check_mass(&git_repo, "HEAD~1", "HEAD", &artifact, &config).unwrap();
        let islands = report
            .findings
            .iter()
            .filter(|finding| finding.properties.get("clone_scope") == Some(&"block".to_string()))
            .collect::<Vec<_>>();
        assert_eq!(
            islands.len(),
            1,
            "expected one island finding, got {:?}",
            report
                .findings
                .iter()
                .map(|finding| (finding.rule_id.clone(), finding.message.clone()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn block_duplicate_inside_large_functions_is_reported() {
        let report = scan_source(&island_source());
        let block = report
            .findings
            .iter()
            .find(|finding| {
                finding
                    .properties
                    .get("clone_scope")
                    .is_some_and(|scope| scope == "block")
            })
            .expect("block duplicate finding");

        // The whole-function rule cannot see a block inside a large function,
        // which is the recall gap block detection exists to close.
        assert!(
            !report
                .findings
                .iter()
                .any(|finding| finding.properties.get("clone_scope")
                    == Some(&"whole-function".to_string()))
        );
        assert_eq!(block.properties["clone_risk"], "high");
        assert_eq!(block.properties["same_file"], "true");
        assert!(block.similarity.is_some_and(|value| value >= 0.85));
        assert_eq!(block.rule_id, "near-clone");

        let left = block.properties["left_start_line"]
            .parse::<usize>()
            .unwrap();
        let right = block.properties["right_start_line"]
            .parse::<usize>()
            .unwrap();
        assert!(
            left > 40 && right > 40,
            "island must be inside both functions"
        );
        assert_eq!(block.location.line, left);
        assert_eq!(block.base_location.as_ref().unwrap().line, right);
        // Both islands are inside their functions rather than at a declaration.
        assert!(
            right + 20
                <= scan_source(&island_source())
                    .findings
                    .iter()
                    .filter_map(|finding| finding.base_location.as_ref())
                    .map(|location| location.line)
                    .max()
                    .unwrap_or(0)
        );
    }

    #[test]
    fn repeated_idioms_below_the_block_floor_are_not_reported() {
        // Constructor boilerplate and one-line accessors are the shape of the
        // near-clone noise reported by adopters. None of it is an island.
        let mut source = String::new();
        for index in 0..8 {
            source.push_str(&format!(
                "struct Config{index} {{ alpha: usize, beta: usize, gamma: usize }}\n\nimpl Config{index} {{\n    fn new(alpha: usize, beta: usize, gamma: usize) -> Self {{\n        Self {{ alpha, beta, gamma }}\n    }}\n\n    fn alpha(&self) -> usize {{\n        self.alpha\n    }}\n\n    fn beta(&self) -> usize {{\n        self.beta\n    }}\n\n    fn gamma(&self) -> usize {{\n        self.gamma\n    }}\n}}\n\n"
            ));
        }
        let report = scan_source(&source);
        assert!(
            !report
                .findings
                .iter()
                .any(|finding| finding.properties.get("clone_scope") == Some(&"block".to_string())),
            "idiomatic repetition must not produce block findings"
        );
    }

    /// Returns a field accessor formatted across enough lines to clear the
    /// whole-function floors.
    fn accessor(name: &str) -> String {
        format!(
            "    fn {name}(&self) -> usize {{\n        self.config\n            .{name}\n            .value\n            .saturating_add(\n                self\n                    .config\n                    .limit,\n            )\n            .saturating_mul(\n                self\n                    .config\n                    .weight,\n            )\n            .saturating_sub(\n                self\n                    .config\n                    .reserved,\n            )\n    }}\n"
        )
    }

    fn accessor_source() -> String {
        let names = ["alpha", "beta", "gamma", "delta"];
        format!(
            "struct Holder {{ config: Config }}\n\nstruct Config {{ alpha: usize, beta: usize, gamma: usize, delta: usize, limit: usize, weight: usize, reserved: usize }}\n\nimpl Holder {{\n{}}}\n",
            names
                .iter()
                .map(|name| accessor(name))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }

    /// Returns a test body large enough to clear the whole-function floors.
    fn test_case(index: usize) -> String {
        format!(
            "fn case_{index}(flag: bool, values: &[usize]) -> usize {{\n    let mut total = 0;\n    for (position, value) in values.iter().enumerate() {{\n        if flag && position % 2 == 0 {{\n            total += value + position;\n        }} else if flag {{\n            total -= value;\n        }} else {{\n            total += position;\n        }}\n    }}\n    if total > 100 {{\n        total /= 2;\n    }} else {{\n        total += 1;\n    }}\n    total\n}}\n"
        )
    }

    #[test]
    fn test_duplication_is_ranked_low_risk() {
        let source = (0..3).map(test_case).collect::<Vec<_>>().join("\n");
        let report = scan_source(&format!("mod tests {{\n{source}}}\n"));
        let pair = report
            .findings
            .iter()
            .find(|finding| finding.properties.contains_key("left_role"))
            .expect("near-clone pair finding");
        assert_eq!(
            pair.properties.get("clone_risk").map(String::as_str),
            Some("low"),
            "properties: {:?}",
            pair.properties
        );
        assert_eq!(pair.properties["left_role"], "test");
    }

    #[test]
    fn accessor_duplication_is_ranked_low_risk() {
        let report = scan_source(&accessor_source());
        let pair = report
            .findings
            .iter()
            .find(|finding| finding.properties.contains_key("left_role"))
            .expect("near-clone pair finding");
        assert_eq!(pair.properties["clone_risk"], "low");
        assert_eq!(pair.properties["left_role"], "accessor");
    }

    #[test]
    fn family_summary_reports_recoverable_mass_not_family_mass() {
        let builder = |name: &str| {
            format!(
                "    fn {name}(value: usize) -> Self {{\n        let scaled = value.saturating_mul(2);\n        let shifted = scaled >> 1;\n        Self {{\n            value,\n            scaled,\n            shifted,\n            label: \"alpha\",\n        }}\n    }}\n"
            )
        };
        let source = format!(
            "struct Alpha {{ value: usize, scaled: usize, shifted: usize, label: &'static str }}\n\nimpl Alpha {{\n{}{}\n}}\n",
            builder("new"),
            builder("widened")
        );
        let report = scan_source(&source);
        let summary = report
            .findings
            .iter()
            .find(|finding| finding.properties.contains_key("finding_kind"))
            .expect("family summary");
        let family_mass: f64 = summary.properties["family_mass"].parse().unwrap();
        let duplicate_mass: f64 = summary.properties["duplicate_mass"].parse().unwrap();
        // Only the smaller copy is removable, so the recoverable mass is half
        // the family total for a two-member family.
        assert!(duplicate_mass < family_mass);
        assert!((duplicate_mass - (family_mass / 2.0)).abs() < 0.01);
    }

    #[test]
    fn deleting_easy_code_does_not_trip_high_complexity_mass_growth() {
        // Removing 437 lines of low-complexity code raises the erosion ratio
        // without adding complexity. The ratio must not be the only signal.
        let dominant = std::iter::repeat_n("    if flag {}\n", 30).collect::<String>();
        let easy = (0..40)
            .map(|index| format!("fn easy_{index}() -> usize {{ {index} }}\n"))
            .collect::<String>();
        let repository =
            TestRepository::new(&format!("fn dominant(flag: bool) {{\n{dominant}}}\n{easy}"));
        let git_repo = repository.repository();
        let mut artifact = build_artifact(&git_repo, "HEAD").unwrap();
        repository.commit_source(&format!("fn dominant(flag: bool) {{\n{dominant}}}\n"));
        let mut config = GateConfig::default();
        config.rules.structural_erosion.erosion_limit = 1.0;
        config.rules.structural_erosion.delta_limit = 1.0;
        config.rules.structural_erosion.mass_growth_limit = 0.0;
        bind_policy(&mut artifact, &config).unwrap();

        let report = check_mass(&git_repo, "HEAD~1", "HEAD", &artifact, &config).unwrap();
        assert!(
            !report
                .findings
                .iter()
                .any(|finding| finding.rule_id == "structural-erosion"),
            "deleting low-complexity code must not read as added complexity"
        );
    }

    #[test]
    fn erosion_finding_reports_mass_growth_and_cutoff_distribution() {
        let body = std::iter::repeat_n("    if flag {}\n", 30).collect::<String>();
        let repository = TestRepository::new("fn small() {}\n");
        let git_repo = repository.repository();
        let mut artifact = build_artifact(&git_repo, "HEAD").unwrap();
        repository.commit_source(&format!(
            "fn dominant(flag: bool) {{\n{body}}}\nfn small() {{}}\n"
        ));
        let mut config = GateConfig::default();
        config.rules.structural_erosion.erosion_limit = 1.0;
        config.rules.structural_erosion.delta_limit = 1.0;
        config.rules.structural_erosion.mass_growth_limit = 0.0;
        bind_policy(&mut artifact, &config).unwrap();

        let report = check_mass(&git_repo, "HEAD~1", "HEAD", &artifact, &config).unwrap();
        let finding = report
            .findings
            .iter()
            .find(|finding| finding.rule_id == "structural-erosion")
            .expect("structural-erosion finding");
        assert_eq!(
            finding.properties.get("breaches").map(String::as_str),
            Some("mass-growth")
        );
        assert!(
            finding.properties["head_high_complexity_mass"]
                .parse::<f64>()
                .unwrap()
                > finding.properties["base_high_complexity_mass"]
                    .parse::<f64>()
                    .unwrap()
        );
        for cutoff in ["5", "10", "15", "20", "30"] {
            assert!(
                finding
                    .properties
                    .contains_key(&format!("erosion_at_cc_{cutoff}")),
                "cutoff distribution must include CC>{cutoff}"
            );
        }
    }

    #[test]
    fn high_complexity_mass_growth_is_scale_free() {
        assert!((mass_growth(100.0, 150.0) - 0.5).abs() < 1e-9);
        assert_eq!(mass_growth(0.0, 0.0), 0.0);
        assert_eq!(mass_growth(0.0, 10.0), 1.0);
        assert!((mass_growth(100.0, 40.0) + 0.6).abs() < 1e-9);
    }

    #[test]
    fn structural_erosion_attribution_ignores_unchanged_dominant_functions() {
        let body = std::iter::repeat_n("    if flag {}\n", 30).collect::<String>();
        let repository = TestRepository::new(&format!("fn dominant(flag: bool) {{\n{body}}}\n"));
        let git_repo = repository.repository();
        let mut artifact = build_artifact(&git_repo, "HEAD").unwrap();
        repository.commit_source(&format!(
            "fn dominant(flag: bool) {{\n{body}}}\nfn helper() {{}}\n"
        ));
        let mut config = GateConfig::default();
        config.rules.structural_erosion.erosion_limit = 0.0;
        config.rules.structural_erosion.delta_limit = 1.0;
        bind_policy(&mut artifact, &config).unwrap();

        let report = check_mass(&git_repo, "HEAD~1", "HEAD", &artifact, &config).unwrap();
        let finding = report
            .findings
            .iter()
            .find(|finding| finding.rule_id == "structural-erosion")
            .expect("structural-erosion finding");
        // The breach is caused by unchanged code, so no changed function is
        // attributed as a contributor, but the finding still points at the
        // dominant function in the head revision instead of a placeholder.
        assert_eq!(finding.location.path, "src/lib.rs");
        assert!(!finding.properties.contains_key("contributor_1_path"));
        assert_eq!(
            finding.properties.get("breaches").map(String::as_str),
            Some("ratio-level")
        );
    }

    #[test]
    fn does_not_flag_an_unchanged_renamed_function() {
        let repository = TestRepository::new("fn parse(flag: bool) { if flag {} }\n");
        let git_repo = repository.repository();
        let artifact = build_artifact(&git_repo, "HEAD").unwrap();
        git(&repository.path, ["mv", "src/lib.rs", "src/renamed.rs"]);
        git(&repository.path, ["commit", "--quiet", "-m", "rename"]);

        let report = check_mass(
            &git_repo,
            "HEAD~1",
            "HEAD",
            &artifact,
            &GateConfig::default(),
        )
        .unwrap();
        assert!(report.findings.is_empty());
    }

    #[test]
    fn detects_an_oversized_new_function() {
        let repository = TestRepository::new("fn existing() {}\n");
        let git_repo = repository.repository();
        let artifact = build_artifact(&git_repo, "HEAD").unwrap();
        let body = std::iter::repeat_n("    if flag {}\n", 30).collect::<String>();
        repository.commit_source(&format!("fn generated(flag: bool) {{\n{body}}}\n"));

        let report = check_mass(
            &git_repo,
            "HEAD~1",
            "HEAD",
            &artifact,
            &GateConfig::default(),
        )
        .unwrap();
        let finding = report
            .findings
            .iter()
            .find(|finding| finding.rule_id == "function-mass")
            .expect("function-mass finding");
        assert!(finding.base_mass.is_none());
    }

    #[test]
    fn checks_a_new_rust_file_without_a_base_blob() {
        let repository = TestRepository::new("fn existing() {}\n");
        let git_repo = repository.repository();
        let artifact = build_artifact(&git_repo, "HEAD").unwrap();
        repository.commit_new_file("tests/common/mod.rs", "fn helper() {}\n");

        let report = check_mass(
            &git_repo,
            "HEAD~1",
            "HEAD",
            &artifact,
            &GateConfig::default(),
        )
        .unwrap();
        assert!(
            !report
                .findings
                .iter()
                .any(|finding| finding.rule_id == "analysis-error")
        );
    }

    #[test]
    fn ignores_a_clone_moved_from_a_deleted_file() {
        let helper = clone_function("helper", "input");
        let repository = TestRepository::new(&helper);
        let git_repo = repository.repository();
        let artifact = build_artifact(&git_repo, "HEAD").unwrap();
        repository.commit_source("fn existing() {}\n");
        repository.commit_new_file("tests/common/mod.rs", &helper);

        let report = check_mass(
            &git_repo,
            "HEAD~2",
            "HEAD",
            &artifact,
            &GateConfig::default(),
        )
        .unwrap();
        assert!(
            !report
                .findings
                .iter()
                .any(|finding| finding.rule_id == "near-clone")
        );
    }

    #[test]
    fn ignores_a_clone_moved_from_a_deleted_function() {
        let helper = clone_function("helper", "input");
        let repository = TestRepository::new(&format!("{helper}\nfn existing() {{}}\n"));
        let git_repo = repository.repository();
        let artifact = build_artifact(&git_repo, "HEAD").unwrap();
        repository.commit_source(&format!("fn existing() {{}}\n{helper}"));

        let report = check_mass(
            &git_repo,
            "HEAD~1",
            "HEAD",
            &artifact,
            &GateConfig::default(),
        )
        .unwrap();
        assert!(
            !report
                .findings
                .iter()
                .any(|finding| finding.rule_id == "near-clone")
        );
    }

    #[test]
    fn checks_a_new_manifest_without_a_base_blob() {
        let repository = TestRepository::new("fn existing() {}\n");
        let git_repo = repository.repository();
        let artifact = build_artifact(&git_repo, "HEAD").unwrap();
        repository.commit_new_file("Cargo.toml", "[dependencies]\nserde = \"1\"\n");

        let report = check_mass(
            &git_repo,
            "HEAD~1",
            "HEAD",
            &artifact,
            &GateConfig::default(),
        )
        .unwrap();
        assert!(report.findings.iter().any(|finding| {
            finding.rule_id == "dependency-surface-growth"
                && finding.properties["pattern_id"] == "new-dependency"
        }));
    }

    #[test]
    fn detects_an_added_lint_suppression() {
        let repository = TestRepository::new("fn existing() {}\n");
        let git_repo = repository.repository();
        let artifact = build_artifact(&git_repo, "HEAD").unwrap();
        repository.commit_source("#[allow(clippy::unwrap_used)]\nfn existing() {}\n");

        let report = check_mass(
            &git_repo,
            "HEAD~1",
            "HEAD",
            &artifact,
            &GateConfig::default(),
        )
        .unwrap();
        let finding = report
            .findings
            .iter()
            .find(|finding| finding.rule_id == "lint-suppression-growth")
            .unwrap();
        assert_eq!(finding.location.line, 1);
        assert_eq!(finding.properties["pattern_id"], "allow");
        assert!(!report.has_errors());
    }

    #[test]
    fn detects_added_unsafe_surface_but_ignores_edits_inside_existing_block() {
        let repository = TestRepository::new(
            "fn existing() {\n    unsafe { let value = 1; let _ = value; }\n}\n",
        );
        let git_repo = repository.repository();
        let artifact = build_artifact(&git_repo, "HEAD").unwrap();
        repository.commit_source(
            "fn existing() {\n    unsafe { let _value = 3; }\n    unsafe { let value = 2; let _ = value; }\n}\n",
        );

        let report = check_mass(
            &git_repo,
            "HEAD~1",
            "HEAD",
            &artifact,
            &GateConfig::default(),
        )
        .unwrap();
        let unsafe_findings = report
            .findings
            .iter()
            .filter(|finding| finding.rule_id == "unsafe-surface-growth")
            .collect::<Vec<_>>();
        assert_eq!(unsafe_findings.len(), 1);
        assert_eq!(unsafe_findings[0].properties["pattern_id"], "unsafe-block");
        assert_eq!(unsafe_findings[0].location.line, 2);

        let error_config =
            GateConfig::from_toml("[rules.unsafe_surface]\nseverity = \"error\"\n").unwrap();
        let mut error_artifact = artifact.clone();
        bind_policy(&mut error_artifact, &error_config).unwrap();
        let error_report =
            check_mass(&git_repo, "HEAD~1", "HEAD", &error_artifact, &error_config).unwrap();
        assert!(error_report.has_errors());

        let suppressed_config = GateConfig::from_toml(
            "[[suppressions]]\nrule = \"unsafe-surface-growth\"\npath = \"src/lib.rs\"\nline = 2\nreason = \"reviewed\"\n",
        )
        .unwrap();
        let mut suppressed_artifact = artifact;
        bind_policy(&mut suppressed_artifact, &suppressed_config).unwrap();
        let suppressed_report = check_mass(
            &git_repo,
            "HEAD~1",
            "HEAD",
            &suppressed_artifact,
            &suppressed_config,
        )
        .unwrap();
        assert!(
            !suppressed_report
                .findings
                .iter()
                .any(|finding| finding.rule_id == "unsafe-surface-growth")
        );
    }

    #[test]
    fn detects_new_and_expanded_dependencies_but_ignores_version_only_changes() {
        let repository = TestRepository::new("fn existing() {}\n");
        fs::write(
            repository.path.join("Cargo.toml"),
            "[dependencies]\nserde = \"1\"\nfoo = { version = \"1\", default-features = false }\n",
        )
        .unwrap();
        git(&repository.path, ["add", "Cargo.toml"]);
        git(
            &repository.path,
            ["commit", "--quiet", "-m", "manifest-base"],
        );
        let git_repo = repository.repository();
        let artifact = build_artifact(&git_repo, "HEAD").unwrap();
        fs::write(
            repository.path.join("Cargo.toml"),
            "[dependencies]\nserde = \"2\"\nfoo = { version = \"1\", default-features = true, features = [\"derive\"] }\nbar = \"1\"\n",
        )
        .unwrap();
        git(&repository.path, ["add", "Cargo.toml"]);
        git(
            &repository.path,
            ["commit", "--quiet", "-m", "manifest-head"],
        );

        let report = check_mass(
            &git_repo,
            "HEAD~1",
            "HEAD",
            &artifact,
            &GateConfig::default(),
        )
        .unwrap();
        let findings = report
            .findings
            .iter()
            .filter(|finding| finding.rule_id == "dependency-surface-growth")
            .collect::<Vec<_>>();
        assert_eq!(findings.len(), 2);
        assert!(findings.iter().any(|finding| {
            finding.properties["dependency_key"] == "bar"
                && finding.properties["pattern_id"] == "new-dependency"
        }));
        assert!(findings.iter().any(|finding| {
            finding.properties["dependency_key"] == "foo"
                && finding.properties["pattern_id"] == "expanded-dependency-surface"
        }));
        assert!(
            !findings
                .iter()
                .any(|finding| finding.properties["dependency_key"] == "serde")
        );

        let error_config =
            GateConfig::from_toml("[rules.dependency_surface]\nseverity = \"error\"\n").unwrap();
        let mut error_artifact = artifact.clone();
        bind_policy(&mut error_artifact, &error_config).unwrap();
        let error_report =
            check_mass(&git_repo, "HEAD~1", "HEAD", &error_artifact, &error_config).unwrap();
        assert!(error_report.has_errors());

        let suppressed_config = GateConfig::from_toml(
            "[[suppressions]]\nrule = \"dependency-surface-growth\"\npath = \"Cargo.toml\"\nline = 4\nreason = \"reviewed\"\n",
        )
        .unwrap();
        let mut suppressed_artifact = artifact;
        bind_policy(&mut suppressed_artifact, &suppressed_config).unwrap();
        let suppressed_report = check_mass(
            &git_repo,
            "HEAD~1",
            "HEAD",
            &suppressed_artifact,
            &suppressed_config,
        )
        .unwrap();
        assert_eq!(
            suppressed_report
                .findings
                .iter()
                .filter(|finding| finding.rule_id == "dependency-surface-growth")
                .count(),
            1
        );
    }

    #[test]
    fn ignores_a_renamed_manifest_with_unchanged_dependency_edges() {
        let repository = TestRepository::new("fn existing() {}\n");
        fs::write(
            repository.path.join("Cargo.toml"),
            "[dependencies]\nserde = \"1\"\n",
        )
        .unwrap();
        git(&repository.path, ["add", "Cargo.toml"]);
        git(
            &repository.path,
            ["commit", "--quiet", "-m", "manifest-base"],
        );
        let git_repo = repository.repository();
        let artifact = build_artifact(&git_repo, "HEAD").unwrap();
        fs::create_dir_all(repository.path.join("nested")).unwrap();
        git(&repository.path, ["mv", "Cargo.toml", "nested/Cargo.toml"]);
        git(
            &repository.path,
            ["commit", "--quiet", "-m", "manifest-rename"],
        );

        let report = check_mass(
            &git_repo,
            "HEAD~1",
            "HEAD",
            &artifact,
            &GateConfig::default(),
        )
        .unwrap();
        assert!(
            !report
                .findings
                .iter()
                .any(|finding| finding.rule_id == "dependency-surface-growth")
        );
    }

    #[test]
    fn detects_a_broadened_but_not_reformatted_suppression() {
        let repository = TestRepository::new("#[allow(clippy::panic)]\nfn existing() {}\n");
        let git_repo = repository.repository();
        let artifact = build_artifact(&git_repo, "HEAD").unwrap();
        repository
            .commit_source("#[allow( clippy::panic, clippy::unwrap_used )]\nfn existing() {}\n");

        let report = check_mass(
            &git_repo,
            "HEAD~1",
            "HEAD",
            &artifact,
            &GateConfig::default(),
        )
        .unwrap();
        assert_eq!(
            report
                .findings
                .iter()
                .filter(|finding| finding.rule_id == "lint-suppression-growth")
                .count(),
            1
        );
    }

    #[test]
    fn ignores_reformatted_unchanged_suppression() {
        let repository = TestRepository::new("#[allow(dead_code)]\nfn existing() {}\n");
        let git_repo = repository.repository();
        let artifact = build_artifact(&git_repo, "HEAD").unwrap();
        repository.commit_source("#[allow( dead_code )]\nfn existing() {}\n");

        let report = check_mass(
            &git_repo,
            "HEAD~1",
            "HEAD",
            &artifact,
            &GateConfig::default(),
        )
        .unwrap();
        assert!(
            !report
                .findings
                .iter()
                .any(|finding| finding.rule_id == "lint-suppression-growth")
        );
    }

    #[test]
    fn lint_suppression_honors_error_severity_and_exact_suppression() {
        let repository = TestRepository::new("fn existing() {}\n");
        let git_repo = repository.repository();
        let mut artifact = build_artifact(&git_repo, "HEAD").unwrap();
        repository.commit_source("#[allow(clippy::unwrap_used)]\nfn existing() {}\n");
        let error_config =
            GateConfig::from_toml("[rules.lint_suppression]\nseverity = \"error\"\n").unwrap();
        bind_policy(&mut artifact, &error_config).unwrap();
        let report = check_mass(&git_repo, "HEAD~1", "HEAD", &artifact, &error_config).unwrap();
        assert!(report.has_errors());

        let suppressed_config = GateConfig::from_toml(
            "[[suppressions]]\nrule = \"lint-suppression-growth\"\npath = \"src/lib.rs\"\nline = 1\nreason = \"intentional\"\n",
        )
        .unwrap();
        bind_policy(&mut artifact, &suppressed_config).unwrap();
        let report =
            check_mass(&git_repo, "HEAD~1", "HEAD", &artifact, &suppressed_config).unwrap();
        assert!(
            !report
                .findings
                .iter()
                .any(|finding| finding.rule_id == "lint-suppression-growth")
        );
    }

    #[test]
    fn ignores_deleted_functions() {
        let repository = TestRepository::new("fn existing() {}\n");
        let git_repo = repository.repository();
        let artifact = build_artifact(&git_repo, "HEAD").unwrap();
        repository.delete_source();

        let report = check_mass(
            &git_repo,
            "HEAD~1",
            "HEAD",
            &artifact,
            &GateConfig::default(),
        )
        .unwrap();
        assert!(report.findings.is_empty());
    }

    #[test]
    fn rejects_an_artifact_for_a_different_base() {
        let repository = TestRepository::new("fn existing() {}\n");
        let git_repo = repository.repository();
        let artifact = build_artifact(&git_repo, "HEAD").unwrap();
        repository.commit_source("fn changed() {}\n");

        assert!(check_mass(&git_repo, "HEAD", "HEAD", &artifact, &GateConfig::default()).is_err());
    }

    #[test]
    fn warns_and_skips_a_changed_file_with_syntax_errors() {
        let repository = TestRepository::new("fn existing() {}\n");
        let git_repo = repository.repository();
        let artifact = build_artifact(&git_repo, "HEAD").unwrap();
        repository.commit_source("fn broken( {");

        let report = check_mass(
            &git_repo,
            "HEAD~1",
            "HEAD",
            &artifact,
            &GateConfig::default(),
        )
        .unwrap();
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].rule_id, "analysis-error");
    }

    #[test]
    fn ignores_dirty_worktree_edits_when_head_is_a_commit() {
        let repository = TestRepository::new("fn existing() {}\n");
        let git_repo = repository.repository();
        let artifact = build_artifact(&git_repo, "HEAD").unwrap();
        fs::write(
            repository.path.join("src/lib.rs"),
            "fn changed() { if true {} }\n",
        )
        .unwrap();

        let report =
            check_mass(&git_repo, "HEAD", "HEAD", &artifact, &GateConfig::default()).unwrap();
        assert!(report.findings.is_empty());
    }

    #[test]
    fn detects_an_identifier_renamed_clone_from_the_base() {
        let original = clone_function("original", "input");
        let repository = TestRepository::new(&original);
        let git_repo = repository.repository();
        let artifact = build_artifact(&git_repo, "HEAD").unwrap();
        repository.commit_source(&format!(
            "{original}\n{}",
            clone_function("duplicate", "value")
        ));

        let report = check_mass(
            &git_repo,
            "HEAD~1",
            "HEAD",
            &artifact,
            &GateConfig::default(),
        )
        .unwrap();
        let clone = report
            .findings
            .iter()
            .find(|finding| finding.rule_id == "near-clone")
            .unwrap();
        assert_eq!(clone.similarity, Some(1.0));
        assert_eq!(clone.properties["token_similarity"], "1.000000");
        assert_eq!(clone.properties["ast_similarity"], "1.000000");
    }

    #[test]
    fn scan_reports_one_of_two_structural_clones() {
        let source = format!(
            "{}\n{}",
            clone_function("first", "input"),
            clone_function("second", "value")
        );
        let repository = TestRepository::new(&source);
        let artifact = build_artifact(&repository.repository(), "HEAD").unwrap();

        let report = scan_clones(&artifact, &GateConfig::default());
        let repeated_report = scan_clones(&artifact, &GateConfig::default());
        assert_eq!(report.findings.len(), 2);
        assert_eq!(report.findings[0].rule_id, "near-clone");
        assert!(report.findings.iter().any(|finding| {
            finding
                .properties
                .get("finding_kind")
                .is_some_and(|kind| kind == "family-summary")
        }));
        let mut limited = report.clone();
        limit_scan_report(&mut limited, 1).unwrap();
        assert_eq!(
            limited
                .findings
                .iter()
                .filter(|finding| !finding.properties.contains_key("finding_kind"))
                .count(),
            1
        );
        assert_eq!(
            limited
                .findings
                .iter()
                .filter(|finding| finding.properties.contains_key("finding_kind"))
                .count(),
            1
        );
        assert_eq!(
            serde_json::to_vec(&report).unwrap(),
            serde_json::to_vec(&repeated_report).unwrap()
        );
        assert_eq!(
            serde_json::to_vec(&crate::sarif::render(&report)).unwrap(),
            serde_json::to_vec(&crate::sarif::render(&repeated_report)).unwrap()
        );
    }

    #[test]
    fn scan_applies_the_configured_clone_thresholds() {
        let source = format!(
            "{}\n{}",
            clone_function("first", "input"),
            clone_function("second", "value")
        );
        let repository = TestRepository::new(&source);
        let artifact = build_artifact(&repository.repository(), "HEAD").unwrap();
        let config = GateConfig::from_toml("[rules.near_clone]\nminimum_tokens = 10000\n").unwrap();

        assert!(scan_clones(&artifact, &config).findings.is_empty());
    }

    #[test]
    fn clone_matching_requires_both_token_and_ast_signals() {
        let source = clone_function("first", "input");
        let candidate = analyze_rust_file("src/first.rs", &source)
            .unwrap()
            .functions
            .into_iter()
            .next()
            .unwrap();
        let mut subject = analyze_rust_file("src/second.rs", &clone_function("second", "value"))
            .unwrap()
            .functions
            .into_iter()
            .next()
            .unwrap();
        subject.ast_shingle_hashes.clear();
        subject.ast_hash = "different".to_string();

        let index = CloneIndex::from_functions([candidate]);
        let config = GateConfig::default();
        assert!(
            index
                .best_match(&subject, &config.rules.near_clone)
                .is_none()
        );
    }

    #[test]
    fn calibrates_thirty_structural_clone_pairs_and_thirty_negatives() {
        let config = GateConfig::default();
        let calibration_source = (0..31)
            .map(|index| clone_function(&format!("clone_{index}"), &format!("value_{index}")))
            .collect::<Vec<_>>()
            .join("\n");
        let calibration_repository = TestRepository::new(&calibration_source);
        let calibration_artifact =
            build_artifact(&calibration_repository.repository(), "HEAD").unwrap();
        let calibration_report = scan_clones(&calibration_artifact, &config);
        assert_eq!(calibration_report.findings.len(), 31);
        assert!(calibration_report.findings.iter().all(
            |finding| finding.rule_id == "near-clone" && finding.severity == Severity::Warning
        ));

        let mut positive_matches = 0;
        let mut negative_matches = 0;
        for index in 0..30 {
            let candidate = analyze_rust_file(
                &format!("src/candidate_{index}.rs"),
                &clone_function("candidate", "input"),
            )
            .unwrap()
            .functions
            .into_iter()
            .next()
            .unwrap();
            let subject = analyze_rust_file(
                &format!("src/subject_{index}.rs"),
                &clone_function("subject", "value"),
            )
            .unwrap()
            .functions
            .into_iter()
            .next()
            .unwrap();
            let index_for_pair = CloneIndex::from_functions([candidate]);
            if index_for_pair
                .best_match(&subject, &config.rules.near_clone)
                .is_some()
            {
                positive_matches += 1;
            }

            let negative = analyze_rust_file(
                &format!("src/negative_{index}.rs"),
                &different_function("negative", "value"),
            )
            .unwrap()
            .functions
            .into_iter()
            .next()
            .unwrap();
            if index_for_pair
                .best_match(&negative, &config.rules.near_clone)
                .is_some()
            {
                negative_matches += 1;
            }
        }
        assert_eq!(positive_matches, 30);
        assert_eq!(negative_matches, 0);
    }

    fn clone_function(name: &str, parameter: &str) -> String {
        format!(
            "fn {name}({parameter}: usize) -> usize {{\n    let mut total = 0;\n    if {parameter} > 0 {{ total += {parameter}; }}\n    if {parameter} > 1 {{ total += 1; }}\n    if {parameter} > 2 {{ total += 2; }}\n    if {parameter} > 3 {{ total += 3; }}\n    if {parameter} > 4 {{ total += 4; }}\n    total\n}}\n"
        )
    }

    fn different_function(name: &str, parameter: &str) -> String {
        format!(
            "fn {name}({parameter}: usize) -> usize {{\n    let mut total = 0;\n    let mut cursor = {parameter};\n    while cursor > 0 {{\n        total += cursor;\n        cursor -= 1;\n    }}\n    match total {{\n        0 => 7,\n        _ => total / 2,\n    }}\n}}\n"
        )
    }

    fn git<'a>(path: &Path, arguments: impl IntoIterator<Item = &'a str>) {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
