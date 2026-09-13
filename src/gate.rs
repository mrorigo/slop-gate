// Rust guideline compliant 2026-09-12
//! Baseline artifact construction and function-mass gate evaluation.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use crate::analysis::{
    FunctionIdentity, FunctionRecord, IndexArtifact, analyze_rust_file, analyzer_fingerprint,
    analyzer_fingerprint_with_policy,
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
        .rust_files(&commit)?
        .into_iter()
        .map(|path| {
            repository
                .read_blob(&commit, &path)
                .and_then(|source| analyze_rust_file(&path, &source))
        })
        .collect::<Result<Vec<_>>>()?;
    IndexArtifact::new(commit, files)
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
    let changed = repository.changed_rust_files(&base_commit, &head_commit)?;
    let base_functions = artifact
        .files
        .iter()
        .flat_map(|file| file.functions.iter())
        .map(|function| (function.identity.clone(), function))
        .collect::<HashMap<_, _>>();
    let mut clone_index = CloneIndex::from_functions(
        artifact
            .files
            .iter()
            .flat_map(|file| file.functions.iter().cloned()),
    );
    let mut report = CheckReport::default();

    for path in &changed.paths {
        let source = repository.read_blob(&head_commit, path)?;
        let file = match analyze_rust_file(path, &source) {
            Ok(file) => file,
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
                continue;
            }
        };
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
            if materially_changed
                && let Some((candidate, token_similarity, ast_similarity)) =
                    clone_index.best_match(function, &config.rules.near_clone)
            {
                push_if_enabled(
                    &mut report,
                    clone_finding(
                        function,
                        &candidate,
                        token_similarity,
                        ast_similarity,
                        config.rules.near_clone.similarity_threshold,
                    ),
                    config,
                );
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
    Finding {
        rule_id: "near-clone".to_string(),
        severity: Severity::Error,
        message: format!(
            "structural similarity {:.2}% to {} exceeds threshold {:.2}%",
            similarity * 100.0,
            candidate.identity.qualified_name,
            threshold * 100.0
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
    let mut index = CloneIndex::default();
    let mut report = CheckReport::default();
    for function in artifact.files.iter().flat_map(|file| file.functions.iter()) {
        if let Some((candidate, token_similarity, ast_similarity)) =
            index.best_match(function, &config.rules.near_clone)
        {
            push_if_enabled(
                &mut report,
                clone_finding(
                    function,
                    &candidate,
                    token_similarity,
                    ast_similarity,
                    config.rules.near_clone.similarity_threshold,
                ),
                config,
            );
        }
        index.insert(function.clone());
    }
    report.findings.sort_by(finding_order);
    report
}

#[derive(Debug, Default)]
struct CloneIndex {
    candidates: Vec<FunctionRecord>,
    by_shingle: HashMap<u64, Vec<usize>>,
}

impl CloneIndex {
    fn from_functions(functions: impl IntoIterator<Item = FunctionRecord>) -> Self {
        let mut index = Self::default();
        for function in functions {
            index.insert(function);
        }
        index
    }

    fn insert(&mut self, function: FunctionRecord) {
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
        if function.sloc < policy.minimum_sloc || function.token_count < policy.minimum_tokens {
            return None;
        }
        let mut overlap_counts = HashMap::<usize, usize>::new();
        for hash in &function.shingle_hashes {
            if let Some(ids) = self.by_shingle.get(hash) {
                for id in ids {
                    *overlap_counts.entry(*id).or_default() += 1;
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
        let mut best: Option<(FunctionRecord, f64, f64)> = None;
        for (id, _) in ids.into_iter().take(policy.max_candidates) {
            let candidate = &self.candidates[id];
            if candidate.identity == function.identity
                || candidate.sloc < policy.minimum_sloc
                || candidate.token_count < policy.minimum_tokens
            {
                continue;
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
            if token_similarity < policy.similarity_threshold
                || ast_similarity < policy.similarity_threshold
            {
                continue;
            }
            let combined_similarity = token_similarity.min(ast_similarity);
            let should_replace = best.as_ref().is_none_or(
                |(current, current_token_similarity, current_ast_similarity)| {
                    let current_combined = (*current_token_similarity).min(*current_ast_similarity);
                    combined_similarity > current_combined
                        || (combined_similarity == current_combined
                            && candidate_key(candidate) < candidate_key(current))
                },
            );
            if should_replace {
                best = Some((candidate.clone(), token_similarity, ast_similarity));
            }
        }
        best
    }
}

fn jaccard_similarity(left: &[u64], right: &[u64]) -> f64 {
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

    use super::{CloneIndex, Severity, bind_policy, build_artifact, check_mass, scan_clones};
    use crate::analysis::analyze_rust_file;
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
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].rule_id, "function-mass");
        assert!(!report.has_errors());
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
        assert_eq!(report.findings.len(), 1);
        assert!(report.findings[0].base_mass.is_none());
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
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].rule_id, "near-clone");
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
        assert_eq!(calibration_report.findings.len(), 30);
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
