// Rust guideline compliant 2026-10-15
//! Snapshot-based application API that does not inspect a repository.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::analysis::{
    AnalyzedFile, FunctionRecord, analyze_source_file, analyzer_fingerprint, dependency_edges,
};
use crate::config::GateConfig;
use crate::diff::{added_line_numbers, line_hashes};
use crate::error::{Error, Result};
use crate::gate::{
    Severity, evaluate_function_changes, evaluate_lint_suppression_records,
    evaluate_unsafe_surface_records,
};
use crate::git::ChangedLineSet;
use crate::path::is_relative_path;

/// A caller-owned, deterministic source snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSnapshot {
    /// Caller-supplied identity; this is not interpreted as a Git revision.
    pub id: String,
    /// Repository-relative entries ordered by path when validated.
    pub entries: Vec<SnapshotEntry>,
}

/// One file-system entry captured by the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotEntry {
    /// Repository-relative path using `/` separators.
    pub path: String,
    /// Declared entry kind.
    pub kind: EntryKind,
    /// Git-compatible mode, such as `0o100644` for a regular file.
    pub mode: u32,
    /// Captured bytes. Symlinks contain their link target bytes and are never followed.
    pub bytes: Vec<u8>,
}

/// Supported source-snapshot entry kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// Regular file contents.
    File,
    /// A symbolic link represented by its target bytes.
    Symlink,
    /// A Git submodule, which cannot be analyzed as source.
    Submodule,
}

/// A validated policy parsed from TOML bytes.
#[derive(Debug, Clone)]
pub struct Policy {
    config: GateConfig,
    fingerprint: String,
}

/// A validated in-memory analysis index for an explicit snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotIndex {
    /// Snapshot index schema version.
    pub schema_version: u32,
    /// Snapshot identity including paths, entry kinds, modes, and bytes.
    pub snapshot_id: String,
    /// Caller-provided index identity.
    pub source_id: String,
    /// Compatibility identity for analyzer implementation.
    pub analyzer_fingerprint: String,
    /// Compatibility identity for the effective policy.
    pub policy_fingerprint: String,
    /// Parsed records, sorted by path.
    pub files: Vec<AnalyzedFile>,
    /// Coverage accumulated while building this index.
    pub coverage: Vec<FileCoverage>,
    /// Path, kind, mode, and content hashes used to validate change manifests.
    pub entries: Vec<SnapshotEntryMetadata>,
    /// Rust lint suppression facts retained for baseline comparison.
    pub lint_suppressions: Vec<SnapshotLintSuppression>,
    /// Rust unsafe-surface facts retained for baseline comparison.
    pub unsafe_constructs: Vec<SnapshotUnsafeConstruct>,
}

/// Content-free metadata for one indexed snapshot entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotEntryMetadata {
    /// Repository-relative path.
    pub path: String,
    /// Declared entry kind.
    pub kind: EntryKind,
    /// Git-compatible entry mode.
    pub mode: u32,
    /// BLAKE3 hash of captured bytes.
    pub content_hash: String,
    /// Stable hashes of newline-delimited source lines for change mapping.
    pub line_hashes: Vec<u64>,
}

/// A normalized Rust lint suppression retained in a baseline index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotLintSuppression {
    /// Repository-relative path.
    pub path: String,
    /// Suppression syntax category.
    pub kind: String,
    /// Sorted lint paths named by the attribute.
    pub lint_paths: Vec<String>,
    /// Stable fingerprint of the suppressed target.
    pub target_fingerprint: String,
    /// Target syntax kind used for reporting.
    pub target_kind: String,
    /// One-based attribute line.
    pub line: usize,
}

/// A normalized Rust unsafe construct retained in a baseline index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotUnsafeConstruct {
    /// Repository-relative path.
    pub path: String,
    /// Versioned syntax pattern identifier.
    pub pattern_id: String,
    /// Normalized construct fingerprint used for matching.
    pub fingerprint: String,
    /// One-based line containing the primary `unsafe` token.
    pub line: usize,
}

/// Analysis coverage for one path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileCoverage {
    /// Repository-relative path.
    pub path: String,
    /// Coverage outcome.
    pub status: CoverageStatus,
    /// Explanation for an exclusion or failure.
    pub detail: Option<String>,
}

/// Coverage outcome for one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    /// Successfully analyzed.
    Analyzed,
    /// Excluded because the entry or language is unsupported.
    Excluded,
    /// Input was malformed or could not be decoded.
    Failed,
}

/// Structured execution completion state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Completeness {
    /// True only when all required analysis finished.
    pub complete: bool,
    /// Reasons this evaluation is incomplete.
    pub reasons: Vec<IncompleteReason>,
}

/// A structured reason analysis did not complete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum IncompleteReason {
    /// Relevant input failed parsing or decoding.
    ParseFailure(String),
    /// Required input kind cannot be analyzed.
    UnsupportedInput(String),
    /// Caller requested cancellation.
    Cancelled,
    /// An explicit analysis budget was reached.
    BudgetExceeded(String),
    /// An enabled rule is not yet evaluated by the snapshot facade.
    RuleNotImplemented(String),
}

/// Coverage for one rule and one snapshot entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleCoverage {
    /// Repository-relative path.
    pub path: String,
    /// Stable rule identifier.
    pub rule_id: String,
    /// Whether this rule applies to this input.
    pub applicable: bool,
    /// Whether the facade completed this rule for this input.
    pub completed: bool,
    /// Source-analysis outcome for this path.
    pub status: CoverageStatus,
    /// Explanation when the rule is inapplicable or incomplete.
    pub detail: Option<String>,
}

/// Stable structured finding returned by snapshot evaluation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiFinding {
    /// Stable rule identifier.
    pub rule_id: String,
    /// Severity inherited from the active policy.
    pub severity: String,
    /// Finding location.
    pub path: String,
    /// One-based source line.
    pub line: usize,
    /// Related source path, when the finding compares two declarations.
    pub related_path: Option<String>,
    /// Related one-based source line.
    pub related_line: Option<usize>,
    /// Stable fingerprint for this finding.
    pub fingerprint: String,
    /// Human-readable explanation.
    pub message: String,
    /// Rule-specific properties.
    pub properties: BTreeMap<String, String>,
}

/// Typed result from evaluating one candidate snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Validated baseline identity.
    pub baseline_id: String,
    /// Validated candidate identity.
    pub candidate_id: String,
    /// Content identity of the supplied baseline source snapshot.
    pub baseline_snapshot_id: String,
    /// Content identity of the supplied candidate source snapshot.
    pub candidate_snapshot_id: String,
    /// Effective policy compatibility identity.
    pub policy_fingerprint: String,
    /// Analyzer compatibility identity.
    pub analyzer_fingerprint: String,
    /// All findings; no presentation limit is applied.
    pub findings: Vec<ApiFinding>,
    /// Candidate coverage by input path.
    pub coverage: Vec<FileCoverage>,
    /// Per-file and per-rule applicability and completion.
    pub rule_coverage: Vec<RuleCoverage>,
    /// Execution completeness and reasons.
    pub completeness: Completeness,
}

/// Caller-specified changed-path manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeManifest {
    /// Paths added, modified, renamed to, or mode-changed in the candidate.
    pub changed: BTreeSet<String>,
    /// Paths removed from the baseline.
    pub deleted: BTreeSet<String>,
    /// Optional destination-to-source rename mapping.
    pub renamed_from: BTreeMap<String, String>,
}

/// Cooperative resource limits for snapshot indexing and evaluation.
#[derive(Debug, Clone, Copy, Default)]
pub struct ResourceLimits {
    /// Maximum entries analyzed; `None` means unlimited.
    pub max_files: Option<usize>,
    /// Maximum total source bytes; `None` means unlimited.
    pub max_source_bytes: Option<usize>,
    /// Maximum baseline-candidate line comparison cells; `None` is unlimited.
    pub max_diff_cells: Option<usize>,
}

/// Parses and validates a policy supplied by the caller.
///
/// # Arguments
/// * `bytes` - UTF-8 TOML policy bytes; empty input selects current defaults.
///
/// # Errors
/// Returns an error when TOML or policy constraints are invalid.
pub fn parse_policy(bytes: &[u8]) -> Result<Policy> {
    let text =
        std::str::from_utf8(bytes).map_err(|error| Error::InvalidPolicy(error.to_string()))?;
    let config = if text.trim().is_empty() {
        GateConfig::default()
    } else {
        GateConfig::from_toml(text).map_err(|error| Error::InvalidPolicy(error.to_string()))?
    };
    let fingerprint = config.fingerprint()?;
    Ok(Policy {
        config,
        fingerprint,
    })
}

impl Policy {
    /// Returns the stable effective-policy fingerprint.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

impl SnapshotIndex {
    /// Serializes this reusable index as compact JSON.
    ///
    /// # Errors
    /// Returns an error if serialization fails.
    pub fn to_json(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(|error| Error::Persistence(error.to_string()))
    }

    /// Deserializes an index and rejects schema or compatibility mismatches.
    ///
    /// # Arguments
    /// * `bytes` - Serialized index bytes.
    /// * `policy` - Effective policy required for reuse.
    ///
    /// # Errors
    /// Returns an error for malformed data, unsupported schema, or analyzer or policy mismatch.
    pub fn from_json(bytes: &[u8], policy: &Policy) -> Result<Self> {
        let index = serde_json::from_slice::<Self>(bytes)
            .map_err(|error| Error::Persistence(error.to_string()))?;
        if index.schema_version != 1
            || index.analyzer_fingerprint != analyzer_fingerprint()
            || index.policy_fingerprint != policy.fingerprint
        {
            return Err(Error::IncompatibleBaseline(
                "index schema, analyzer, or effective policy fingerprint differs".to_string(),
            ));
        }
        validate_index(&index)?;
        Ok(index)
    }
}

/// Builds an index solely from caller-supplied snapshot entries.
///
/// Unsupported file kinds and languages are recorded as exclusions. Invalid
/// paths or duplicate paths reject the snapshot. Cancellation and budgets are
/// checked between entries; a partial index is returned with incomplete coverage.
pub fn build_index(
    snapshot: &SourceSnapshot,
    policy: &Policy,
    cancelled: &AtomicBool,
    limits: ResourceLimits,
) -> Result<SnapshotIndex> {
    validate_snapshot(snapshot)?;
    let mut entries = snapshot.entries.clone();
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let metadata = entries
        .iter()
        .map(|entry| SnapshotEntryMetadata {
            path: entry.path.clone(),
            kind: entry.kind,
            mode: entry.mode,
            content_hash: blake3::hash(&entry.bytes).to_hex().to_string(),
            line_hashes: line_hashes(&entry.bytes),
        })
        .collect::<Vec<_>>();
    let snapshot_id = snapshot_fingerprint(&metadata);
    let mut files = Vec::new();
    let mut coverage_records = Vec::new();
    let mut consumed = 0usize;
    let mut lint_suppressions = Vec::new();
    let mut unsafe_constructs = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if cancelled.load(Ordering::Relaxed) {
            coverage_records.extend(
                entries[index..]
                    .iter()
                    .map(|e| coverage(e, CoverageStatus::Failed, "cancelled")),
            );
            break;
        }
        if limits.max_files.is_some_and(|limit| index >= limit)
            || limits
                .max_source_bytes
                .is_some_and(|limit| consumed.saturating_add(entry.bytes.len()) > limit)
        {
            coverage_records.extend(
                entries[index..]
                    .iter()
                    .map(|e| coverage(e, CoverageStatus::Failed, "resource budget exceeded")),
            );
            break;
        }
        consumed = consumed.saturating_add(entry.bytes.len());
        if entry.kind != EntryKind::File {
            coverage_records.push(coverage(
                entry,
                CoverageStatus::Excluded,
                if entry.kind == EntryKind::Submodule {
                    "Git submodule is unsupported"
                } else {
                    "symbolic links are declared but not followed"
                },
            ));
            continue;
        }
        let Ok(source) = std::str::from_utf8(&entry.bytes) else {
            coverage_records.push(coverage(
                entry,
                CoverageStatus::Failed,
                "source is not UTF-8",
            ));
            continue;
        };
        if entry.path.ends_with(".rs") {
            let suppressions = match crate::analysis::lint_suppressions(&entry.path, source) {
                Ok(facts) => facts,
                Err(error) => {
                    coverage_records.push(coverage(
                        entry,
                        CoverageStatus::Failed,
                        &error.to_string(),
                    ));
                    continue;
                }
            };
            let unsafe_facts = match crate::analysis::unsafe_surface(&entry.path, source) {
                Ok(facts) => facts,
                Err(error) => {
                    coverage_records.push(coverage(
                        entry,
                        CoverageStatus::Failed,
                        &error.to_string(),
                    ));
                    continue;
                }
            };
            lint_suppressions.extend(suppressions.into_iter().map(|fact| {
                SnapshotLintSuppression {
                    path: entry.path.clone(),
                    kind: fact.kind,
                    lint_paths: fact.lint_paths,
                    target_fingerprint: fact.target_fingerprint,
                    target_kind: fact.target_kind,
                    line: fact.line,
                }
            }));
            unsafe_constructs.extend(unsafe_facts.into_iter().map(|fact| {
                SnapshotUnsafeConstruct {
                    path: entry.path.clone(),
                    pattern_id: fact.pattern_id,
                    fingerprint: fact.fingerprint,
                    line: fact.line,
                }
            }));
        }
        if entry.path.ends_with("Cargo.toml") {
            match dependency_edges(source) {
                Ok(_) => coverage_records.push(coverage(
                    entry,
                    CoverageStatus::Analyzed,
                    "Cargo manifest parsed",
                )),
                Err(error) => coverage_records.push(coverage(
                    entry,
                    CoverageStatus::Failed,
                    &error.to_string(),
                )),
            }
            continue;
        }
        match analyze_source_file(&entry.path, source) {
            Ok(file) => {
                files.push(file);
                coverage_records.push(coverage(
                    entry,
                    CoverageStatus::Analyzed,
                    "analysis completed",
                ));
            }
            Err(error) if is_supported(&entry.path) => {
                coverage_records.push(coverage(entry, CoverageStatus::Failed, &error.to_string()))
            }
            Err(_) => coverage_records.push(coverage(
                entry,
                CoverageStatus::Excluded,
                "unsupported language",
            )),
        }
    }
    Ok(SnapshotIndex {
        schema_version: 1,
        snapshot_id,
        source_id: snapshot.id.clone(),
        analyzer_fingerprint: analyzer_fingerprint(),
        policy_fingerprint: policy.fingerprint.clone(),
        files,
        coverage: coverage_records,
        entries: metadata,
        lint_suppressions,
        unsafe_constructs,
    })
}

/// Evaluates a candidate snapshot against an explicit baseline index.
///
/// Evaluates function-mass and near-clone rules using the same rule engine as
/// the CLI. Other enabled rules are identified as incomplete in rule coverage
/// until their snapshot-native evaluators are added. Baseline compatibility
/// and manifest consistency are checked before candidate bytes are analyzed.
pub fn evaluate(
    baseline: &SnapshotIndex,
    candidate: &SourceSnapshot,
    manifest: &ChangeManifest,
    policy: &Policy,
    cancelled: &AtomicBool,
    limits: ResourceLimits,
) -> Result<EvaluationReport> {
    if baseline.analyzer_fingerprint != analyzer_fingerprint()
        || baseline.policy_fingerprint != policy.fingerprint
    {
        return Err(Error::IncompatibleBaseline(
            "analyzer or effective policy fingerprint differs".to_string(),
        ));
    }
    validate_index(baseline)?;
    validate_snapshot(candidate)?;
    validate_manifest(baseline, candidate, manifest)?;
    validate_baseline_coverage(baseline, policy)?;
    let candidate_index = build_index(candidate, policy, cancelled, limits)?;
    let changed_files = candidate_index
        .files
        .iter()
        .filter(|file| manifest.changed.contains(&file.path))
        .map(|file| (file.path.clone(), file.clone()))
        .collect::<HashMap<_, _>>();
    let candidate_paths = candidate
        .entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<HashSet<_>>();
    let mut core_report = evaluate_function_changes(
        &baseline.files,
        &changed_files,
        &candidate_paths,
        &manifest.changed.iter().cloned().collect::<Vec<_>>(),
        &manifest
            .renamed_from
            .iter()
            .map(|(new, old)| (new.clone(), old.clone()))
            .collect(),
        &policy.config,
    );
    let baseline_entries = baseline
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect::<HashMap<_, _>>();
    let candidate_entries = candidate_index
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect::<HashMap<_, _>>();
    let baseline_suppressions =
        baseline
            .lint_suppressions
            .iter()
            .fold(HashMap::<_, Vec<_>>::new(), |mut grouped, fact| {
                grouped.entry(fact.path.as_str()).or_default().push(fact);
                grouped
            });
    let baseline_unsafe =
        baseline
            .unsafe_constructs
            .iter()
            .fold(HashMap::<_, Vec<_>>::new(), |mut grouped, fact| {
                grouped.entry(fact.path.as_str()).or_default().push(fact);
                grouped
            });
    let candidate_suppressions = candidate_index.lint_suppressions.iter().fold(
        HashMap::<_, Vec<_>>::new(),
        |mut grouped, fact| {
            grouped.entry(fact.path.as_str()).or_default().push(fact);
            grouped
        },
    );
    let candidate_unsafe = candidate_index.unsafe_constructs.iter().fold(
        HashMap::<_, Vec<_>>::new(),
        |mut grouped, fact| {
            grouped.entry(fact.path.as_str()).or_default().push(fact);
            grouped
        },
    );
    let mut diff_failures = HashMap::new();
    for path in manifest.changed.iter().filter(|path| path.ends_with(".rs")) {
        let old_path = manifest.renamed_from.get(path).unwrap_or(path);
        let old_lines = baseline_entries
            .get(old_path.as_str())
            .map_or(&[][..], |entry| entry.line_hashes.as_slice());
        let new_lines = candidate_entries
            .get(path.as_str())
            .map_or(&[][..], |entry| entry.line_hashes.as_slice());
        let added = match added_line_numbers(old_lines, new_lines, limits.max_diff_cells, cancelled)
        {
            Ok(lines) => ChangedLineSet::from_lines(lines),
            Err(error) => {
                diff_failures.insert(path.clone(), error.to_string());
                continue;
            }
        };
        let to_lint = |facts: &[&SnapshotLintSuppression]| {
            facts
                .iter()
                .map(|fact| crate::analysis::LintSuppression {
                    kind: fact.kind.clone(),
                    lint_paths: fact.lint_paths.clone(),
                    target_fingerprint: fact.target_fingerprint.clone(),
                    target_kind: fact.target_kind.clone(),
                    line: fact.line,
                })
                .collect::<Vec<_>>()
        };
        let to_unsafe = |facts: &[&SnapshotUnsafeConstruct]| {
            facts
                .iter()
                .map(|fact| crate::analysis::UnsafeSurface {
                    pattern_id: fact.pattern_id.clone(),
                    fingerprint: fact.fingerprint.clone(),
                    line: fact.line,
                })
                .collect::<Vec<_>>()
        };
        let head_lint = to_lint(
            candidate_suppressions
                .get(path.as_str())
                .map(Vec::as_slice)
                .unwrap_or_default(),
        );
        let base_lint = to_lint(
            baseline_suppressions
                .get(old_path.as_str())
                .map(Vec::as_slice)
                .unwrap_or_default(),
        );
        evaluate_lint_suppression_records(
            path,
            &head_lint,
            &base_lint,
            &added,
            &policy.config,
            &mut core_report,
        );
        let head_unsafe = to_unsafe(
            candidate_unsafe
                .get(path.as_str())
                .map(Vec::as_slice)
                .unwrap_or_default(),
        );
        let base_unsafe = to_unsafe(
            baseline_unsafe
                .get(old_path.as_str())
                .map(Vec::as_slice)
                .unwrap_or_default(),
        );
        evaluate_unsafe_surface_records(
            path,
            &head_unsafe,
            &base_unsafe,
            &added,
            &policy.config,
            &mut core_report,
        );
    }
    core_report.findings.sort_by(|left, right| {
        (&left.location.path, left.location.line, &left.rule_id).cmp(&(
            &right.location.path,
            right.location.line,
            &right.rule_id,
        ))
    });
    let findings = core_report
        .findings
        .into_iter()
        .map(|finding| {
            let mut fingerprint_input = format!(
                "{}\0{}\0{}\0{}",
                finding.rule_id,
                finding.location.path,
                finding
                    .properties
                    .get("qualified_name")
                    .map(String::as_str)
                    .unwrap_or(""),
                finding
                    .base_location
                    .as_ref()
                    .map(|location| location.path.as_str())
                    .unwrap_or("")
            );
            if let Some(scope) = finding.properties.get("clone_scope") {
                fingerprint_input.push('\0');
                fingerprint_input.push_str(scope);
            }
            ApiFinding {
                rule_id: finding.rule_id,
                severity: match finding.severity {
                    Severity::Warning => "warning",
                    Severity::Error => "error",
                }
                .into(),
                path: finding.location.path,
                line: finding.location.line,
                related_path: finding
                    .base_location
                    .as_ref()
                    .map(|location| location.path.clone()),
                related_line: finding.base_location.map(|location| location.line),
                fingerprint: blake3::hash(fingerprint_input.as_bytes())
                    .to_hex()
                    .to_string(),
                message: finding.message,
                properties: finding.properties,
            }
        })
        .collect::<Vec<_>>();
    let (rule_coverage, unimplemented_rules) =
        build_rule_coverage(&candidate_index, manifest, policy, &diff_failures);
    let mut reasons = unimplemented_rules
        .into_iter()
        .map(IncompleteReason::RuleNotImplemented)
        .collect::<Vec<_>>();
    reasons.extend(
        rule_coverage
            .iter()
            .filter(|entry| entry.applicable && !entry.completed)
            .filter_map(|entry| {
                let detail = entry.detail.as_deref().unwrap_or_default();
                if detail.contains("budget") {
                    Some(IncompleteReason::BudgetExceeded(entry.path.clone()))
                } else if detail.contains("cancelled") {
                    Some(IncompleteReason::Cancelled)
                } else if entry.status == CoverageStatus::Excluded {
                    Some(IncompleteReason::UnsupportedInput(entry.path.clone()))
                } else if entry.status == CoverageStatus::Failed {
                    Some(IncompleteReason::ParseFailure(entry.path.clone()))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>(),
    );
    Ok(EvaluationReport {
        schema_version: 1,
        baseline_id: baseline.source_id.clone(),
        candidate_id: candidate.id.clone(),
        baseline_snapshot_id: baseline.snapshot_id.clone(),
        candidate_snapshot_id: candidate_index.snapshot_id.clone(),
        policy_fingerprint: policy.fingerprint.clone(),
        analyzer_fingerprint: baseline.analyzer_fingerprint.clone(),
        findings,
        coverage: candidate_index.coverage,
        rule_coverage,
        completeness: Completeness {
            complete: reasons.is_empty(),
            reasons,
        },
    })
}

/// Returns functions extracted from a validated index in stable order.
pub fn function_records(index: &SnapshotIndex) -> Vec<&FunctionRecord> {
    index
        .files
        .iter()
        .flat_map(|file| file.functions.iter())
        .collect()
}

fn validate_snapshot(snapshot: &SourceSnapshot) -> Result<()> {
    if snapshot.id.trim().is_empty() {
        return Err(Error::InvalidSnapshot(
            "snapshot id must not be empty".into(),
        ));
    }
    let mut seen = BTreeSet::new();
    for entry in &snapshot.entries {
        if !is_relative_path(&entry.path)
            || entry.path.contains('\\')
            || !seen.insert(entry.path.clone())
        {
            return Err(Error::InvalidSnapshot(format!(
                "invalid or duplicate path {:?}",
                entry.path
            )));
        }
        let valid_mode = match entry.kind {
            EntryKind::File => matches!(entry.mode, 0o100644 | 0o100755),
            EntryKind::Symlink => entry.mode == 0o120000,
            EntryKind::Submodule => entry.mode == 0o160000,
        };
        if !valid_mode {
            return Err(Error::InvalidSnapshot(format!(
                "mode {:o} is invalid for {:?} entry {}",
                entry.mode, entry.kind, entry.path
            )));
        }
    }
    Ok(())
}
fn validate_manifest(
    base: &SnapshotIndex,
    candidate: &SourceSnapshot,
    manifest: &ChangeManifest,
) -> Result<()> {
    let base_paths = base
        .entries
        .iter()
        .map(|e| e.path.as_str())
        .collect::<BTreeSet<_>>();
    let candidate_paths = candidate
        .entries
        .iter()
        .map(|e| e.path.as_str())
        .collect::<BTreeSet<_>>();
    for path in &manifest.changed {
        if !candidate_paths.contains(path.as_str()) {
            return Err(Error::InvalidSnapshot(format!(
                "changed path {path:?} is absent from candidate"
            )));
        }
    }
    for path in &manifest.deleted {
        if !base_paths.contains(path.as_str()) || candidate_paths.contains(path.as_str()) {
            return Err(Error::InvalidSnapshot(format!(
                "deleted path {path:?} is inconsistent"
            )));
        }
    }
    for (new, old) in &manifest.renamed_from {
        if !manifest.changed.contains(new) || !base_paths.contains(old.as_str()) {
            return Err(Error::InvalidSnapshot(format!(
                "rename {old:?} -> {new:?} is inconsistent"
            )));
        }
    }
    let base_by_path = base
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect::<HashMap<_, _>>();
    let candidate_by_path = candidate
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect::<HashMap<_, _>>();
    let expected_changed = candidate_by_path
        .iter()
        .filter_map(|(path, entry)| {
            let previous = base_by_path.get(path);
            let differs = previous.is_none_or(|previous| {
                previous.kind != entry.kind
                    || previous.mode != entry.mode
                    || previous.content_hash != blake3::hash(&entry.bytes).to_hex().to_string()
            });
            differs.then_some(*path)
        })
        .collect::<BTreeSet<_>>();
    let actual_changed = manifest
        .changed
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if expected_changed != actual_changed {
        return Err(Error::InvalidSnapshot(
            "changed paths do not match snapshot additions, content changes, and mode changes"
                .into(),
        ));
    }
    let expected_deleted = base_by_path
        .keys()
        .filter(|path| !candidate_by_path.contains_key(**path))
        .copied()
        .collect::<BTreeSet<_>>();
    let actual_deleted = manifest
        .deleted
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if expected_deleted != actual_deleted {
        return Err(Error::InvalidSnapshot(
            "deleted paths do not match snapshot entries".into(),
        ));
    }
    Ok(())
}
fn snapshot_fingerprint(entries: &[SnapshotEntryMetadata]) -> String {
    let mut hasher = blake3::Hasher::new();
    for entry in entries {
        hasher.update(entry.path.as_bytes());
        hasher.update(&[0, entry.kind as u8]);
        hasher.update(&entry.mode.to_le_bytes());
        hasher.update(entry.content_hash.as_bytes());
        hasher.update(&[0]);
        for line_hash in &entry.line_hashes {
            hasher.update(&line_hash.to_le_bytes());
        }
        hasher.update(&[0xff]);
    }
    hasher.finalize().to_hex().to_string()
}

fn validate_index(index: &SnapshotIndex) -> Result<()> {
    for pair in index.entries.windows(2) {
        if pair[0].path >= pair[1].path {
            return Err(Error::IncompatibleBaseline(
                "index entries are not strictly sorted".into(),
            ));
        }
    }
    for entry in &index.entries {
        let valid_mode = match entry.kind {
            EntryKind::File => matches!(entry.mode, 0o100644 | 0o100755),
            EntryKind::Symlink => entry.mode == 0o120000,
            EntryKind::Submodule => entry.mode == 0o160000,
        };
        if !is_relative_path(&entry.path) || !valid_mode || entry.content_hash.len() != 64 {
            return Err(Error::IncompatibleBaseline(format!(
                "entry metadata for {:?} is invalid",
                entry.path
            )));
        }
    }
    if index.snapshot_id != snapshot_fingerprint(&index.entries) {
        return Err(Error::IncompatibleBaseline(
            "snapshot identity does not match its entry manifest".into(),
        ));
    }
    let metadata = index
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect::<HashMap<_, _>>();
    for pair in index.files.windows(2) {
        if pair[0].path >= pair[1].path {
            return Err(Error::IncompatibleBaseline(
                "analyzed files are not strictly sorted".into(),
            ));
        }
    }
    for file in &index.files {
        let Some(entry) = metadata.get(file.path.as_str()) else {
            return Err(Error::IncompatibleBaseline(format!(
                "analyzed file {:?} is absent from the entry manifest",
                file.path
            )));
        };
        if entry.kind != EntryKind::File || entry.content_hash != file.content_hash {
            return Err(Error::IncompatibleBaseline(format!(
                "analyzed file {:?} differs from its entry manifest",
                file.path
            )));
        }
    }
    for (path, line) in index
        .lint_suppressions
        .iter()
        .map(|fact| (fact.path.as_str(), fact.line))
        .chain(
            index
                .unsafe_constructs
                .iter()
                .map(|fact| (fact.path.as_str(), fact.line)),
        )
    {
        let Some(entry) = metadata.get(path) else {
            return Err(Error::IncompatibleBaseline(format!(
                "source fact path {path:?} is absent from the entry manifest"
            )));
        };
        if !path.ends_with(".rs") || line == 0 || line > entry.line_hashes.len() {
            return Err(Error::IncompatibleBaseline(format!(
                "source fact location {path}:{line} is invalid"
            )));
        }
    }
    let coverage_paths = index
        .coverage
        .iter()
        .map(|entry| entry.path.as_str())
        .collect::<BTreeSet<_>>();
    if coverage_paths.len() != index.entries.len()
        || index
            .entries
            .iter()
            .any(|entry| !coverage_paths.contains(entry.path.as_str()))
    {
        return Err(Error::IncompatibleBaseline(
            "coverage records do not match the entry manifest".into(),
        ));
    }
    Ok(())
}

fn validate_baseline_coverage(index: &SnapshotIndex, policy: &Policy) -> Result<()> {
    for file in &index.coverage {
        let required = (is_supported(&file.path)
            && (policy.config.rules.function_mass.severity != crate::config::RuleSeverity::Off
                || policy.config.rules.near_clone.severity != crate::config::RuleSeverity::Off))
            || (file.path.ends_with(".rs")
                && (policy.config.rules.lint_suppression.severity
                    != crate::config::RuleSeverity::Off
                    || policy.config.rules.unsafe_surface.severity
                        != crate::config::RuleSeverity::Off
                    || policy.config.rules.structural_erosion.severity
                        != crate::config::RuleSeverity::Off))
            || (file.path.ends_with("Cargo.toml")
                && policy.config.rules.dependency_surface.severity
                    != crate::config::RuleSeverity::Off);
        if !required {
            continue;
        }
        match file.status {
            CoverageStatus::Analyzed => {}
            CoverageStatus::Excluded => {
                return Err(Error::UnsupportedRequiredInput(file.path.clone()));
            }
            CoverageStatus::Failed
                if file
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("cancelled")) =>
            {
                return Err(Error::Cancelled);
            }
            CoverageStatus::Failed
                if file
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("budget")) =>
            {
                return Err(Error::BudgetExceeded(file.path.clone()));
            }
            CoverageStatus::Failed => {
                return Err(Error::ParseFailure {
                    path: file.path.clone(),
                    detail: file
                        .detail
                        .clone()
                        .unwrap_or_else(|| "analysis failed".into()),
                });
            }
        }
    }
    Ok(())
}

fn build_rule_coverage(
    index: &SnapshotIndex,
    manifest: &ChangeManifest,
    policy: &Policy,
    diff_failures: &HashMap<String, String>,
) -> (Vec<RuleCoverage>, Vec<String>) {
    let rules = [
        ("function-mass", policy.config.rules.function_mass.severity),
        ("near-clone", policy.config.rules.near_clone.severity),
        (
            "lint-suppression-growth",
            policy.config.rules.lint_suppression.severity,
        ),
        (
            "unsafe-surface-growth",
            policy.config.rules.unsafe_surface.severity,
        ),
        (
            "dependency-surface-growth",
            policy.config.rules.dependency_surface.severity,
        ),
        (
            "structural-erosion",
            policy.config.rules.structural_erosion.severity,
        ),
    ];
    let mut records = Vec::new();
    let mut unavailable = BTreeSet::new();
    for file in &index.coverage {
        for (rule_id, severity) in rules {
            let changed = manifest.changed.contains(&file.path);
            let applies = match rule_id {
                "function-mass" | "near-clone" => changed && is_supported(&file.path),
                "lint-suppression-growth" | "unsafe-surface-growth" => {
                    changed && file.path.ends_with(".rs")
                }
                "structural-erosion" => file.path.ends_with(".rs"),
                "dependency-surface-growth" => changed && file.path.ends_with("Cargo.toml"),
                _ => false,
            };
            let enabled = severity != crate::config::RuleSeverity::Off;
            let completed = if !applies || !enabled {
                true
            } else if matches!(rule_id, "function-mass" | "near-clone") {
                file.status == CoverageStatus::Analyzed
            } else if matches!(rule_id, "lint-suppression-growth" | "unsafe-surface-growth") {
                file.status == CoverageStatus::Analyzed && !diff_failures.contains_key(&file.path)
            } else {
                false
            };
            if applies
                && enabled
                && !completed
                && file.status == CoverageStatus::Analyzed
                && !diff_failures.contains_key(&file.path)
            {
                unavailable.insert(rule_id.to_string());
            }
            records.push(RuleCoverage {
                path: file.path.clone(),
                rule_id: rule_id.to_string(),
                applicable: applies && enabled,
                completed,
                status: if diff_failures.contains_key(&file.path) {
                    CoverageStatus::Failed
                } else {
                    file.status
                },
                detail: if !enabled {
                    Some("disabled by policy".into())
                } else if !applies {
                    Some("rule does not apply to this path".into())
                } else if completed {
                    Some("analysis completed".into())
                } else if let Some(error) = diff_failures.get(&file.path) {
                    Some(error.clone())
                } else if file.status != CoverageStatus::Analyzed {
                    file.detail.clone()
                } else {
                    Some("snapshot facade does not implement this rule yet".into())
                },
            });
        }
    }
    (records, unavailable.into_iter().collect())
}
fn coverage(entry: &SnapshotEntry, status: CoverageStatus, detail: &str) -> FileCoverage {
    FileCoverage {
        path: entry.path.clone(),
        status,
        detail: Some(detail.to_string()),
    }
}
fn is_supported(path: &str) -> bool {
    path.ends_with(".rs")
        || path.ends_with(".py")
        || path.ends_with(".ts")
        || path.ends_with(".tsx")
}
