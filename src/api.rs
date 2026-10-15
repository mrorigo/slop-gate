// Rust guideline compliant 2026-10-15
//! Snapshot-based application API that does not inspect a repository.
//!
//! Calls use only caller-owned values and do not mutate process-global state.
//! Policies and completed indexes can be shared between threads; callers own
//! cancellation tokens and should coordinate writes to them with atomic operations.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::analysis::{
    AnalyzedFile, DependencyEdge, FunctionRecord, analyze_source_file, analyzer_fingerprint,
    dependency_edges,
};
use crate::config::GateConfig;
use crate::diff::{added_line_numbers, line_hashes};
use crate::error::{Error, Result};
use crate::gate::{
    Severity, evaluate_dependency_edges, evaluate_function_changes,
    evaluate_lint_suppression_records, evaluate_structural_erosion,
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
///
/// The value is immutable after construction and can be shared across concurrent calls.
#[derive(Debug, Clone)]
pub struct Policy {
    config: GateConfig,
    fingerprint: String,
}

/// A validated in-memory analysis index for an explicit snapshot.
///
/// Indexes can be shared across concurrent evaluations. Persisted indexes are
/// disposable and must pass schema, analyzer, and policy checks before reuse.
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
    /// Direct Cargo dependency facts retained for baseline comparison.
    pub dependency_edges: Vec<SnapshotDependencyEdge>,
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

/// A normalized Cargo dependency edge retained in a baseline index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotDependencyEdge {
    /// Repository-relative manifest path.
    pub path: String,
    /// Manifest-relative table path.
    pub table_path: String,
    /// Dependency key used by Cargo.
    pub dependency_key: String,
    /// Optional renamed package name.
    pub package: Option<String>,
    /// Source kind such as `registry`, `git`, `path`, or `unspecified`.
    pub source_kind: String,
    /// Normalized source locator, when present.
    pub source: Option<String>,
    /// Whether Cargo default features are enabled.
    pub default_features: bool,
    /// Sorted explicitly enabled features.
    pub features: Vec<String>,
    /// Normalized version requirement, when present.
    pub version: Option<String>,
    /// One-based dependency declaration line.
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
/// No input is read from the working directory. Calls are independent and may
/// run concurrently when callers use distinct or properly synchronized tokens.
pub fn build_index(
    snapshot: &SourceSnapshot,
    policy: &Policy,
    cancelled: &AtomicBool,
    limits: ResourceLimits,
) -> Result<SnapshotIndex> {
    validate_snapshot(snapshot)?;
    let mut entries = snapshot.entries.clone();
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let metadata = entries.iter().map(entry_metadata).collect::<Vec<_>>();
    let snapshot_id = snapshot_fingerprint(&metadata);
    let mut files = Vec::new();
    let mut coverage_records = Vec::new();
    let mut consumed = 0usize;
    let mut lint_suppressions = Vec::new();
    let mut unsafe_constructs = Vec::new();
    let mut dependency_facts = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let interrupted = index_failure(index, entry, consumed, cancelled, limits);
        if let Some(detail) = interrupted {
            coverage_records.extend(
                entries[index..]
                    .iter()
                    .map(|entry| coverage(entry, CoverageStatus::Failed, detail)),
            );
            break;
        }
        consumed = consumed.saturating_add(entry.bytes.len());
        let facts = analyze_entry(entry);
        files.extend(facts.file);
        if let Some(record) = facts.coverage {
            coverage_records.push(record);
        }
        lint_suppressions.extend(facts.lint_suppressions);
        unsafe_constructs.extend(facts.unsafe_constructs);
        dependency_facts.extend(facts.dependency_edges);
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
        dependency_edges: dependency_facts,
    })
}

fn entry_metadata(entry: &SnapshotEntry) -> SnapshotEntryMetadata {
    SnapshotEntryMetadata {
        path: entry.path.clone(),
        kind: entry.kind,
        mode: entry.mode,
        content_hash: blake3::hash(&entry.bytes).to_hex().to_string(),
        line_hashes: line_hashes(&entry.bytes),
    }
}

fn index_failure(
    index: usize,
    entry: &SnapshotEntry,
    consumed: usize,
    cancelled: &AtomicBool,
    limits: ResourceLimits,
) -> Option<&'static str> {
    if cancelled.load(Ordering::Relaxed) {
        Some("cancelled")
    } else if limits.max_files.is_some_and(|limit| index >= limit)
        || limits
            .max_source_bytes
            .is_some_and(|limit| consumed.saturating_add(entry.bytes.len()) > limit)
    {
        Some("resource budget exceeded")
    } else {
        None
    }
}

#[derive(Default)]
struct EntryFacts {
    file: Option<AnalyzedFile>,
    coverage: Option<FileCoverage>,
    lint_suppressions: Vec<SnapshotLintSuppression>,
    unsafe_constructs: Vec<SnapshotUnsafeConstruct>,
    dependency_edges: Vec<SnapshotDependencyEdge>,
}

fn analyze_entry(entry: &SnapshotEntry) -> EntryFacts {
    if entry.kind != EntryKind::File {
        return EntryFacts {
            coverage: Some(coverage(
                entry,
                CoverageStatus::Excluded,
                if entry.kind == EntryKind::Submodule {
                    "Git submodule is unsupported"
                } else {
                    "symbolic links are declared but not followed"
                },
            )),
            ..EntryFacts::default()
        };
    }
    let Ok(source) = std::str::from_utf8(&entry.bytes) else {
        return failed_entry(entry, "source is not UTF-8");
    };
    if entry.path.ends_with(".rs") {
        return analyze_rust_entry(entry, source);
    }
    if entry.path.ends_with("Cargo.toml") {
        return analyze_cargo_entry(entry, source);
    }
    analyze_source_entry(entry, source)
}

fn analyze_rust_entry(entry: &SnapshotEntry, source: &str) -> EntryFacts {
    let suppressions = match crate::analysis::lint_suppressions(&entry.path, source) {
        Ok(facts) => facts,
        Err(error) => return failed_entry(entry, &error.to_string()),
    };
    let unsafe_facts = match crate::analysis::unsafe_surface(&entry.path, source) {
        Ok(facts) => facts,
        Err(error) => return failed_entry(entry, &error.to_string()),
    };
    let mut result = analyze_source_entry(entry, source);
    if result
        .coverage
        .as_ref()
        .is_some_and(|record| record.status == CoverageStatus::Analyzed)
    {
        result.lint_suppressions = suppressions
            .into_iter()
            .map(|fact| SnapshotLintSuppression {
                path: entry.path.clone(),
                kind: fact.kind,
                lint_paths: fact.lint_paths,
                target_fingerprint: fact.target_fingerprint,
                target_kind: fact.target_kind,
                line: fact.line,
            })
            .collect();
        result.unsafe_constructs = unsafe_facts
            .into_iter()
            .map(|fact| SnapshotUnsafeConstruct {
                path: entry.path.clone(),
                pattern_id: fact.pattern_id,
                fingerprint: fact.fingerprint,
                line: fact.line,
            })
            .collect();
    }
    result
}

fn analyze_cargo_entry(entry: &SnapshotEntry, source: &str) -> EntryFacts {
    match dependency_edges(source) {
        Ok(edges) => EntryFacts {
            coverage: Some(coverage(
                entry,
                CoverageStatus::Analyzed,
                "Cargo manifest parsed",
            )),
            dependency_edges: edges
                .into_iter()
                .map(|edge| SnapshotDependencyEdge {
                    path: entry.path.clone(),
                    table_path: edge.table_path,
                    dependency_key: edge.dependency_key,
                    package: edge.package,
                    source_kind: edge.source_kind,
                    source: edge.source,
                    default_features: edge.default_features,
                    features: edge.features,
                    version: edge.version,
                    line: edge.line,
                })
                .collect(),
            ..EntryFacts::default()
        },
        Err(error) => failed_entry(entry, &error.to_string()),
    }
}

fn analyze_source_entry(entry: &SnapshotEntry, source: &str) -> EntryFacts {
    match analyze_source_file(&entry.path, source) {
        Ok(file) => EntryFacts {
            file: Some(file),
            coverage: Some(coverage(
                entry,
                CoverageStatus::Analyzed,
                "analysis completed",
            )),
            ..EntryFacts::default()
        },
        Err(error) if is_supported(&entry.path) => failed_entry(entry, &error.to_string()),
        Err(_) => EntryFacts {
            coverage: Some(coverage(
                entry,
                CoverageStatus::Excluded,
                "unsupported language",
            )),
            ..EntryFacts::default()
        },
    }
}

fn failed_entry(entry: &SnapshotEntry, detail: &str) -> EntryFacts {
    EntryFacts {
        coverage: Some(coverage(entry, CoverageStatus::Failed, detail)),
        ..EntryFacts::default()
    }
}

/// Evaluates a candidate snapshot against an explicit baseline index.
///
/// Enabled rules use the same evaluators as the CLI. Baseline compatibility
/// and manifest consistency are checked before candidate bytes are analyzed.
/// Cancellation and resource limits are cooperative and are checked between
/// bounded units of work. Candidate analysis returns accumulated coverage when
/// interrupted; an unusable baseline returns a typed error before evaluation.
/// Calls do not access or mutate process-global state and may run concurrently.
pub fn evaluate(
    baseline: &SnapshotIndex,
    candidate: &SourceSnapshot,
    manifest: &ChangeManifest,
    policy: &Policy,
    cancelled: &AtomicBool,
    limits: ResourceLimits,
) -> Result<EvaluationReport> {
    validate_evaluation_inputs(baseline, candidate, manifest, policy)?;
    let candidate_index = build_index(candidate, policy, cancelled, limits)?;
    let mut core_report =
        evaluate_function_rules(baseline, candidate, &candidate_index, manifest, policy);
    let rust_diffs = evaluate_rust_rules(
        baseline,
        &candidate_index,
        manifest,
        policy,
        cancelled,
        limits,
        &mut core_report,
    );
    evaluate_dependency_rules(
        baseline,
        &candidate_index,
        manifest,
        policy,
        &mut core_report,
    );
    let structural_complete =
        rust_analysis_complete(&candidate_index, manifest, &rust_diffs.failures);
    if structural_complete {
        evaluate_snapshot_erosion(
            baseline,
            &candidate_index,
            manifest,
            policy,
            &rust_diffs.added,
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
    let findings = convert_findings(core_report);
    let rule_coverage = build_rule_coverage(
        &candidate_index,
        manifest,
        policy,
        &rust_diffs.failures,
        structural_complete,
    );
    let reasons = incompleteness_reasons(&rule_coverage);
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

fn validate_evaluation_inputs(
    baseline: &SnapshotIndex,
    candidate: &SourceSnapshot,
    manifest: &ChangeManifest,
    policy: &Policy,
) -> Result<()> {
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
    validate_baseline_coverage(baseline, policy)
}

fn evaluate_function_rules(
    baseline: &SnapshotIndex,
    candidate: &SourceSnapshot,
    candidate_index: &SnapshotIndex,
    manifest: &ChangeManifest,
    policy: &Policy,
) -> crate::gate::CheckReport {
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
    let renamed = manifest
        .renamed_from
        .iter()
        .map(|(new, old)| (new.clone(), old.clone()))
        .collect();
    evaluate_function_changes(
        &baseline.files,
        &changed_files,
        &candidate_paths,
        &manifest.changed.iter().cloned().collect::<Vec<_>>(),
        &renamed,
        &policy.config,
    )
}

#[derive(Default)]
struct RustDiffs {
    failures: HashMap<String, String>,
    added: HashMap<String, ChangedLineSet>,
}

fn evaluate_rust_rules(
    baseline: &SnapshotIndex,
    candidate: &SnapshotIndex,
    manifest: &ChangeManifest,
    policy: &Policy,
    cancelled: &AtomicBool,
    limits: ResourceLimits,
    report: &mut crate::gate::CheckReport,
) -> RustDiffs {
    let base_entries = entry_map(&baseline.entries);
    let candidate_entries = entry_map(&candidate.entries);
    let base_lint = group_facts(&baseline.lint_suppressions, |fact| fact.path.as_str());
    let head_lint = group_facts(&candidate.lint_suppressions, |fact| fact.path.as_str());
    let base_unsafe = group_facts(&baseline.unsafe_constructs, |fact| fact.path.as_str());
    let head_unsafe = group_facts(&candidate.unsafe_constructs, |fact| fact.path.as_str());
    let context = RustPathContext {
        base_entries: &base_entries,
        candidate_entries: &candidate_entries,
        base_lint: &base_lint,
        head_lint: &head_lint,
        base_unsafe: &base_unsafe,
        head_unsafe: &head_unsafe,
        policy,
        cancelled,
        limits,
    };
    let mut diffs = RustDiffs::default();
    for path in manifest.changed.iter().filter(|path| path.ends_with(".rs")) {
        let old_path = manifest.renamed_from.get(path).unwrap_or(path);
        let result = evaluate_rust_path(path, old_path, &context, report);
        match result {
            Ok(added) => {
                diffs.added.insert(path.clone(), added);
            }
            Err(error) => {
                diffs.failures.insert(path.clone(), error.to_string());
            }
        }
    }
    diffs
}

type EntryMap<'a> = HashMap<&'a str, &'a SnapshotEntryMetadata>;
type LintFactMap<'a> = HashMap<&'a str, Vec<&'a SnapshotLintSuppression>>;
type UnsafeFactMap<'a> = HashMap<&'a str, Vec<&'a SnapshotUnsafeConstruct>>;

fn entry_map(entries: &[SnapshotEntryMetadata]) -> EntryMap<'_> {
    entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect()
}

fn group_facts<'a, T>(
    facts: &'a [T],
    path_of: impl Fn(&'a T) -> &'a str,
) -> HashMap<&'a str, Vec<&'a T>> {
    let mut grouped = HashMap::new();
    for fact in facts {
        grouped
            .entry(path_of(fact))
            .or_insert_with(Vec::new)
            .push(fact);
    }
    grouped
}

struct RustPathContext<'a> {
    base_entries: &'a EntryMap<'a>,
    candidate_entries: &'a EntryMap<'a>,
    base_lint: &'a LintFactMap<'a>,
    head_lint: &'a LintFactMap<'a>,
    base_unsafe: &'a UnsafeFactMap<'a>,
    head_unsafe: &'a UnsafeFactMap<'a>,
    policy: &'a Policy,
    cancelled: &'a AtomicBool,
    limits: ResourceLimits,
}

fn evaluate_rust_path(
    path: &str,
    old_path: &str,
    context: &RustPathContext<'_>,
    report: &mut crate::gate::CheckReport,
) -> Result<ChangedLineSet> {
    let old_lines = context
        .base_entries
        .get(old_path)
        .map_or(&[][..], |entry| entry.line_hashes.as_slice());
    let new_lines = context
        .candidate_entries
        .get(path)
        .map_or(&[][..], |entry| entry.line_hashes.as_slice());
    let added = ChangedLineSet::from_lines(added_line_numbers(
        old_lines,
        new_lines,
        context.limits.max_diff_cells,
        context.cancelled,
    )?);
    let (head_lint, base_lint) = surface_records(
        path,
        old_path,
        context.head_lint,
        context.base_lint,
        lint_record,
    );
    evaluate_lint_suppression_records(
        path,
        &head_lint,
        &base_lint,
        &added,
        &context.policy.config,
        report,
    );
    let (head_unsafe, base_unsafe) = surface_records(
        path,
        old_path,
        context.head_unsafe,
        context.base_unsafe,
        unsafe_record,
    );
    evaluate_unsafe_surface_records(
        path,
        &head_unsafe,
        &base_unsafe,
        &added,
        &context.policy.config,
        report,
    );
    Ok(added)
}

fn surface_records<T, R>(
    path: &str,
    old_path: &str,
    head: &HashMap<&str, Vec<&T>>,
    base: &HashMap<&str, Vec<&T>>,
    convert: fn(&T) -> R,
) -> (Vec<R>, Vec<R>) {
    let convert_facts = |facts: Option<&Vec<&T>>| {
        facts
            .into_iter()
            .flatten()
            .map(|fact| convert(fact))
            .collect()
    };
    (
        convert_facts(head.get(path)),
        convert_facts(base.get(old_path)),
    )
}

fn lint_record(fact: &SnapshotLintSuppression) -> crate::analysis::LintSuppression {
    crate::analysis::LintSuppression {
        kind: fact.kind.clone(),
        lint_paths: fact.lint_paths.clone(),
        target_fingerprint: fact.target_fingerprint.clone(),
        target_kind: fact.target_kind.clone(),
        line: fact.line,
    }
}

fn unsafe_record(fact: &SnapshotUnsafeConstruct) -> crate::analysis::UnsafeSurface {
    crate::analysis::UnsafeSurface {
        pattern_id: fact.pattern_id.clone(),
        fingerprint: fact.fingerprint.clone(),
        line: fact.line,
    }
}

fn evaluate_dependency_rules(
    baseline: &SnapshotIndex,
    candidate: &SnapshotIndex,
    manifest: &ChangeManifest,
    policy: &Policy,
    report: &mut crate::gate::CheckReport,
) {
    let base_edges = group_facts(&baseline.dependency_edges, |fact| fact.path.as_str());
    let head_edges = group_facts(&candidate.dependency_edges, |fact| fact.path.as_str());
    for path in manifest
        .changed
        .iter()
        .filter(|path| path.ends_with("Cargo.toml"))
    {
        let old_path = manifest.renamed_from.get(path).unwrap_or(path);
        let base = dependency_records(base_edges.get(old_path.as_str()));
        let head = dependency_records(head_edges.get(path.as_str()));
        evaluate_dependency_edges(path, &head, &base, &policy.config, report);
    }
}

fn dependency_records(facts: Option<&Vec<&SnapshotDependencyEdge>>) -> Vec<DependencyEdge> {
    facts
        .into_iter()
        .flatten()
        .map(|fact| DependencyEdge {
            table_path: fact.table_path.clone(),
            dependency_key: fact.dependency_key.clone(),
            package: fact.package.clone(),
            source_kind: fact.source_kind.clone(),
            source: fact.source.clone(),
            default_features: fact.default_features,
            features: fact.features.clone(),
            version: fact.version.clone(),
            line: fact.line,
        })
        .collect()
}

fn rust_analysis_complete(
    candidate: &SnapshotIndex,
    manifest: &ChangeManifest,
    diff_failures: &HashMap<String, String>,
) -> bool {
    candidate
        .coverage
        .iter()
        .filter(|file| file.path.ends_with(".rs"))
        .all(|file| file.status == CoverageStatus::Analyzed)
        && manifest
            .changed
            .iter()
            .filter(|path| path.ends_with(".rs"))
            .all(|path| !diff_failures.contains_key(path))
}

fn evaluate_snapshot_erosion(
    baseline: &SnapshotIndex,
    candidate: &SnapshotIndex,
    manifest: &ChangeManifest,
    policy: &Policy,
    added_lines: &HashMap<String, ChangedLineSet>,
    report: &mut crate::gate::CheckReport,
) {
    let head_rust = rust_files(&candidate.files);
    let base_rust = rust_files(&baseline.files);
    let renamed = manifest
        .renamed_from
        .iter()
        .map(|(new, old)| (new.clone(), old.clone()))
        .collect();
    evaluate_structural_erosion(
        &head_rust,
        &base_rust,
        &renamed,
        added_lines,
        &policy.config,
        report,
    );
}

fn rust_files(files: &[AnalyzedFile]) -> Vec<AnalyzedFile> {
    files
        .iter()
        .filter(|file| file.path.ends_with(".rs"))
        .cloned()
        .collect()
}

fn convert_findings(report: crate::gate::CheckReport) -> Vec<ApiFinding> {
    report.findings.into_iter().map(convert_finding).collect()
}

fn convert_finding(finding: crate::gate::Finding) -> ApiFinding {
    let fingerprint_input = finding_fingerprint_input(&finding);
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
}

fn finding_fingerprint_input(finding: &crate::gate::Finding) -> String {
    let mut input = format!(
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
        input.push('\0');
        input.push_str(scope);
    }
    input
}

fn incompleteness_reasons(rule_coverage: &[RuleCoverage]) -> Vec<IncompleteReason> {
    let mut reasons = Vec::new();
    for record in rule_coverage
        .iter()
        .filter(|record| record.applicable && !record.completed)
    {
        if let Some(reason) = incomplete_reason(record)
            && !reasons.contains(&reason)
        {
            reasons.push(reason);
        }
    }
    reasons
}

fn incomplete_reason(record: &RuleCoverage) -> Option<IncompleteReason> {
    let detail = record.detail.as_deref().unwrap_or_default();
    if detail.contains("budget") {
        Some(IncompleteReason::BudgetExceeded(record.path.clone()))
    } else if detail.contains("cancelled") {
        Some(IncompleteReason::Cancelled)
    } else if record.status == CoverageStatus::Excluded {
        Some(IncompleteReason::UnsupportedInput(record.path.clone()))
    } else if record.status == CoverageStatus::Failed {
        Some(IncompleteReason::ParseFailure(record.path.clone()))
    } else {
        None
    }
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
    validate_manifest_paths(base, candidate, manifest)?;
    validate_manifest_delta(base, candidate, manifest)
}

fn validate_manifest_paths(
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
        if new == old || !manifest.changed.contains(new) || !base_paths.contains(old.as_str()) {
            return Err(Error::InvalidSnapshot(format!(
                "rename {old:?} -> {new:?} is inconsistent"
            )));
        }
    }
    Ok(())
}

fn validate_manifest_delta(
    base: &SnapshotIndex,
    candidate: &SourceSnapshot,
    manifest: &ChangeManifest,
) -> Result<()> {
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
    validate_index_entries(index)?;
    let metadata = index
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect::<HashMap<_, _>>();
    validate_index_files(index, &metadata)?;
    validate_source_facts(index, &metadata)?;
    validate_dependency_facts(index, &metadata)?;
    validate_index_coverage(index)
}

fn validate_index_entries(index: &SnapshotIndex) -> Result<()> {
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
        if !is_relative_path(&entry.path)
            || entry.path.contains('\\')
            || !valid_mode
            || entry.content_hash.len() != 64
        {
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
    Ok(())
}

fn validate_index_files(
    index: &SnapshotIndex,
    metadata: &HashMap<&str, &SnapshotEntryMetadata>,
) -> Result<()> {
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
    Ok(())
}

fn validate_source_facts(
    index: &SnapshotIndex,
    metadata: &HashMap<&str, &SnapshotEntryMetadata>,
) -> Result<()> {
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
        validate_fact_location(path, line, metadata, ".rs", "source")?;
    }
    Ok(())
}

fn validate_dependency_facts(
    index: &SnapshotIndex,
    metadata: &HashMap<&str, &SnapshotEntryMetadata>,
) -> Result<()> {
    for fact in &index.dependency_edges {
        let Some(entry) = metadata.get(fact.path.as_str()) else {
            return Err(Error::IncompatibleBaseline(format!(
                "dependency fact path {:?} is absent from the entry manifest",
                fact.path
            )));
        };
        if entry.kind != EntryKind::File
            || !fact.path.ends_with("Cargo.toml")
            || fact.line == 0
            || fact.line > entry.line_hashes.len()
            || fact.table_path.is_empty()
            || fact.dependency_key.is_empty()
        {
            return Err(Error::IncompatibleBaseline(format!(
                "dependency fact location or identity for {:?} is invalid",
                fact.path
            )));
        }
    }
    Ok(())
}

fn validate_fact_location(
    path: &str,
    line: usize,
    metadata: &HashMap<&str, &SnapshotEntryMetadata>,
    suffix: &str,
    fact_kind: &str,
) -> Result<()> {
    let Some(entry) = metadata.get(path) else {
        return Err(Error::IncompatibleBaseline(format!(
            "{fact_kind} fact path {path:?} is absent from the entry manifest"
        )));
    };
    if !path.ends_with(suffix) || line == 0 || line > entry.line_hashes.len() {
        return Err(Error::IncompatibleBaseline(format!(
            "{fact_kind} fact location {path}:{line} is invalid"
        )));
    }
    Ok(())
}

fn validate_index_coverage(index: &SnapshotIndex) -> Result<()> {
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
        if baseline_rule_requires_path(policy, &file.path) {
            validate_baseline_file(file)?;
        }
    }
    Ok(())
}

fn baseline_rule_requires_path(policy: &Policy, path: &str) -> bool {
    let rules = &policy.config.rules;
    (is_supported(path)
        && (rules.function_mass.severity != crate::config::RuleSeverity::Off
            || rules.near_clone.severity != crate::config::RuleSeverity::Off))
        || (path.ends_with(".rs")
            && (rules.lint_suppression.severity != crate::config::RuleSeverity::Off
                || rules.unsafe_surface.severity != crate::config::RuleSeverity::Off
                || rules.structural_erosion.severity != crate::config::RuleSeverity::Off))
        || (path.ends_with("Cargo.toml")
            && rules.dependency_surface.severity != crate::config::RuleSeverity::Off)
}

fn validate_baseline_file(file: &FileCoverage) -> Result<()> {
    match file.status {
        CoverageStatus::Analyzed => Ok(()),
        CoverageStatus::Excluded => Err(Error::UnsupportedRequiredInput(file.path.clone())),
        CoverageStatus::Failed
            if file
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("cancelled")) =>
        {
            Err(Error::Cancelled)
        }
        CoverageStatus::Failed
            if file
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("budget")) =>
        {
            Err(Error::BudgetExceeded(file.path.clone()))
        }
        CoverageStatus::Failed => Err(Error::ParseFailure {
            path: file.path.clone(),
            detail: file
                .detail
                .clone()
                .unwrap_or_else(|| "analysis failed".into()),
        }),
    }
}

fn build_rule_coverage(
    index: &SnapshotIndex,
    manifest: &ChangeManifest,
    policy: &Policy,
    diff_failures: &HashMap<String, String>,
    structural_complete: bool,
) -> Vec<RuleCoverage> {
    let rules = rule_severities(policy);
    index
        .coverage
        .iter()
        .flat_map(|file| {
            rule_coverage_for_file(file, manifest, diff_failures, structural_complete, &rules)
        })
        .collect()
}

type RuleSeverities = [(&'static str, crate::config::RuleSeverity); 6];

fn rule_severities(policy: &Policy) -> RuleSeverities {
    [
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
    ]
}

fn rule_coverage_for_file(
    file: &FileCoverage,
    manifest: &ChangeManifest,
    diff_failures: &HashMap<String, String>,
    structural_complete: bool,
    rules: &RuleSeverities,
) -> Vec<RuleCoverage> {
    rules
        .iter()
        .map(|(rule_id, severity)| {
            rule_coverage_record(
                file,
                manifest.changed.contains(&file.path),
                diff_failures,
                structural_complete,
                rule_id,
                *severity,
            )
        })
        .collect()
}

fn rule_coverage_record(
    file: &FileCoverage,
    changed: bool,
    diff_failures: &HashMap<String, String>,
    structural_complete: bool,
    rule_id: &str,
    severity: crate::config::RuleSeverity,
) -> RuleCoverage {
    let applies = rule_applies(rule_id, changed, &file.path);
    let enabled = severity != crate::config::RuleSeverity::Off;
    let completed = rule_completed(
        rule_id,
        applies && enabled,
        file,
        diff_failures,
        structural_complete,
    );
    RuleCoverage {
        path: file.path.clone(),
        rule_id: rule_id.to_string(),
        applicable: applies && enabled,
        completed,
        status: rule_status(file, diff_failures),
        detail: rule_detail(file, diff_failures, applies, enabled, completed),
    }
}

fn rule_applies(rule_id: &str, changed: bool, path: &str) -> bool {
    match rule_id {
        "function-mass" | "near-clone" => changed && is_supported(path),
        "lint-suppression-growth" | "unsafe-surface-growth" => changed && path.ends_with(".rs"),
        "structural-erosion" => path.ends_with(".rs"),
        "dependency-surface-growth" => changed && path.ends_with("Cargo.toml"),
        _ => false,
    }
}

fn rule_completed(
    rule_id: &str,
    applicable: bool,
    file: &FileCoverage,
    diff_failures: &HashMap<String, String>,
    structural_complete: bool,
) -> bool {
    if !applicable {
        return true;
    }
    match rule_id {
        "function-mass" | "near-clone" | "dependency-surface-growth" => {
            file.status == CoverageStatus::Analyzed
        }
        "lint-suppression-growth" | "unsafe-surface-growth" => {
            file.status == CoverageStatus::Analyzed && !diff_failures.contains_key(&file.path)
        }
        "structural-erosion" => structural_complete,
        _ => false,
    }
}

fn rule_status(file: &FileCoverage, diff_failures: &HashMap<String, String>) -> CoverageStatus {
    if diff_failures.contains_key(&file.path) {
        CoverageStatus::Failed
    } else {
        file.status
    }
}

fn rule_detail(
    file: &FileCoverage,
    diff_failures: &HashMap<String, String>,
    applies: bool,
    enabled: bool,
    completed: bool,
) -> Option<String> {
    if !enabled {
        return Some("disabled by policy".into());
    }
    if !applies {
        return Some("rule does not apply to this path".into());
    }
    if completed {
        return Some("analysis completed".into());
    }
    if let Some(error) = diff_failures.get(&file.path) {
        return Some(error.clone());
    }
    if file.status != CoverageStatus::Analyzed {
        return file.detail.clone();
    }
    Some("analysis did not complete for this rule".into())
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        ChangeManifest, CoverageStatus, EntryKind, IncompleteReason, ResourceLimits, SnapshotEntry,
        SourceSnapshot, build_index, evaluate, function_records, parse_policy,
    };

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    fn entry(path: &str, bytes: &[u8]) -> SnapshotEntry {
        SnapshotEntry {
            path: path.into(),
            kind: EntryKind::File,
            mode: 0o100644,
            bytes: bytes.to_vec(),
        }
    }

    fn snapshot(id: &str, entries: Vec<SnapshotEntry>) -> SourceSnapshot {
        SourceSnapshot {
            id: id.into(),
            entries,
        }
    }

    fn index(source: &SourceSnapshot, policy: &super::Policy) -> super::SnapshotIndex {
        build_index(
            source,
            policy,
            &AtomicBool::new(false),
            ResourceLimits::default(),
        )
        .unwrap()
    }

    fn changed(path: &str) -> ChangeManifest {
        ChangeManifest {
            changed: BTreeSet::from([path.into()]),
            ..ChangeManifest::default()
        }
    }

    #[test]
    fn supplied_candidate_bytes_produce_structural_findings() {
        let policy = parse_policy(
            b"[rules.structural_erosion]\nseverity = \"error\"\nerosion_limit = 1.0\ndelta_limit = 1.0\nmass_growth_limit = 0.0\n",
        )
        .unwrap();
        let baseline = snapshot("base", vec![entry("src/lib.rs", b"fn small() {}\n")]);
        let body = std::iter::repeat_n("    if flag {}\n", 30).collect::<String>();
        let candidate_source = format!("fn large(flag: bool) {{\n{body}}}\nfn small() {{}}\n");
        let candidate = snapshot(
            "candidate",
            vec![entry("src/lib.rs", candidate_source.as_bytes())],
        );
        let baseline_index = index(&baseline, &policy);

        let report = evaluate(
            &baseline_index,
            &candidate,
            &changed("src/lib.rs"),
            &policy,
            &AtomicBool::new(false),
            ResourceLimits::default(),
        )
        .unwrap();

        assert!(report.completeness.complete);
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.rule_id == "structural-erosion")
        );
    }

    #[test]
    fn captured_candidate_is_independent_of_later_caller_changes() {
        let policy = parse_policy(b"").unwrap();
        let baseline = snapshot("base", vec![entry("src/lib.rs", b"fn original() {}\n")]);
        let baseline_index = index(&baseline, &policy);
        let captured = snapshot("captured", vec![entry("src/lib.rs", b"fn captured() {}\n")]);
        let candidate_id = index(&captured, &policy).snapshot_id;
        let mut later = captured.clone();
        later.entries[0].bytes = b"fn changed_after_capture() {}\n".to_vec();

        let report = evaluate(
            &baseline_index,
            &captured,
            &changed("src/lib.rs"),
            &policy,
            &AtomicBool::new(false),
            ResourceLimits::default(),
        )
        .unwrap();

        assert_eq!(report.candidate_snapshot_id, candidate_id);
        assert_ne!(
            index(&later, &policy).snapshot_id,
            report.candidate_snapshot_id
        );
    }

    #[test]
    fn untracked_source_entries_are_indexed() {
        let policy = parse_policy(b"[rules.function_mass]\nnew_function_limit = 0.0\n").unwrap();
        let baseline = snapshot("base", vec![]);
        let source = snapshot(
            "candidate",
            vec![entry(
                "src/new.rs",
                b"fn new_function(flag: bool) { if flag {} }\n",
            )],
        );
        let candidate_index = index(&source, &policy);
        assert!(
            function_records(&candidate_index)
                .iter()
                .any(|record| record.name == "new_function")
        );
        let report = evaluate(
            &index(&baseline, &policy),
            &source,
            &changed("src/new.rs"),
            &policy,
            &AtomicBool::new(false),
            ResourceLimits::default(),
        )
        .unwrap();
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.rule_id == "function-mass")
        );
    }

    #[test]
    fn deleted_source_functions_are_absent_from_candidate_records() {
        let policy = parse_policy(b"").unwrap();
        let baseline = snapshot("base", vec![entry("src/removed.rs", b"fn removed() {}\n")]);
        let candidate = snapshot("candidate", vec![]);
        let baseline_index = index(&baseline, &policy);
        assert!(function_records(&index(&candidate, &policy)).is_empty());
        let manifest = ChangeManifest {
            deleted: BTreeSet::from(["src/removed.rs".into()]),
            ..ChangeManifest::default()
        };

        let report = evaluate(
            &baseline_index,
            &candidate,
            &manifest,
            &policy,
            &AtomicBool::new(false),
            ResourceLimits::default(),
        )
        .unwrap();

        assert!(report.completeness.complete);
        assert!(report.coverage.is_empty());
    }

    #[test]
    fn malformed_changed_rust_marks_required_coverage_incomplete() {
        let policy = parse_policy(b"").unwrap();
        let baseline = snapshot("base", vec![entry("src/lib.rs", b"fn valid() {}\n")]);
        let candidate = snapshot("candidate", vec![entry("src/lib.rs", b"fn broken( {\n")]);
        let report = evaluate(
            &index(&baseline, &policy),
            &candidate,
            &changed("src/lib.rs"),
            &policy,
            &AtomicBool::new(false),
            ResourceLimits::default(),
        )
        .unwrap();

        assert_eq!(report.coverage[0].status, CoverageStatus::Failed);
        assert!(!report.completeness.complete);
        assert!(report.completeness.reasons.iter().any(|reason| matches!(
            reason,
            IncompleteReason::ParseFailure(path) if path == "src/lib.rs"
        )));
    }

    #[test]
    fn incompatible_policy_rejects_a_cached_baseline() {
        let policy = parse_policy(b"").unwrap();
        let other_policy = parse_policy(b"[rules.near_clone]\nminimum_tokens = 41\n").unwrap();
        let baseline = snapshot("base", vec![entry("src/lib.rs", b"fn valid() {}\n")]);
        let baseline_index = index(&baseline, &policy);
        let serialized = baseline_index.to_json().unwrap();
        let cached_error = super::SnapshotIndex::from_json(&serialized, &other_policy).unwrap_err();
        assert!(matches!(
            cached_error,
            crate::error::AppError::IncompatibleBaseline(_)
        ));
        let error = evaluate(
            &baseline_index,
            &baseline,
            &ChangeManifest::default(),
            &other_policy,
            &AtomicBool::new(false),
            ResourceLimits::default(),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            crate::error::AppError::IncompatibleBaseline(_)
        ));
    }

    #[test]
    fn cancellation_returns_accumulated_candidate_coverage() {
        let policy = parse_policy(b"").unwrap();
        let baseline = snapshot("base", vec![entry("src/lib.rs", b"fn valid() {}\n")]);
        let baseline_index = index(&baseline, &policy);
        let candidate = snapshot("candidate", vec![entry("src/lib.rs", b"fn changed() {}\n")]);
        let cancelled = AtomicBool::new(true);

        let report = evaluate(
            &baseline_index,
            &candidate,
            &changed("src/lib.rs"),
            &policy,
            &cancelled,
            ResourceLimits::default(),
        )
        .unwrap();

        assert_eq!(report.coverage[0].status, CoverageStatus::Failed);
        assert!(!report.completeness.complete);
        assert!(
            report
                .completeness
                .reasons
                .contains(&IncompleteReason::Cancelled)
        );
    }

    #[test]
    fn git_and_snapshot_adapters_report_the_same_findings() {
        let policy = parse_policy(
            b"[rules.structural_erosion]\nseverity = \"error\"\nerosion_limit = 1.0\ndelta_limit = 1.0\nmass_growth_limit = 0.0\n",
        )
        .unwrap();
        let base_source = "fn small() {}\n";
        let body = std::iter::repeat_n("    if flag {}\n", 30).collect::<String>();
        let head_source = format!("fn large(flag: bool) {{\n{body}}}\nfn small() {{}}\n");
        let path = test_repository(base_source, &head_source);
        let repository = crate::git::GitRepository::open(&path).unwrap();
        let mut artifact = crate::gate::build_artifact(&repository, "HEAD~1").unwrap();
        crate::gate::bind_policy(&mut artifact, &policy.config).unwrap();
        let cli_report =
            crate::gate::check_mass(&repository, "HEAD~1", "HEAD", &artifact, &policy.config)
                .unwrap();
        let base = snapshot("base", vec![entry("src/lib.rs", base_source.as_bytes())]);
        let candidate = snapshot(
            "candidate",
            vec![entry("src/lib.rs", head_source.as_bytes())],
        );
        let api_report = evaluate(
            &index(&base, &policy),
            &candidate,
            &changed("src/lib.rs"),
            &policy,
            &AtomicBool::new(false),
            ResourceLimits::default(),
        )
        .unwrap();

        let cli_findings = cli_report
            .findings
            .iter()
            .map(|finding| {
                (
                    finding.rule_id.as_str(),
                    finding.location.path.as_str(),
                    finding.location.line,
                    finding.properties.clone(),
                )
            })
            .collect::<Vec<_>>();
        let api_findings = api_report
            .findings
            .iter()
            .map(|finding| {
                (
                    finding.rule_id.as_str(),
                    finding.path.as_str(),
                    finding.line,
                    finding.properties.clone(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(api_findings, cli_findings);
        fs::remove_dir_all(path).unwrap();
    }

    fn test_repository(base_source: &str, head_source: &str) -> PathBuf {
        let nonce = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("slop-gate-api-test-{}-{nonce}", std::process::id()));
        fs::create_dir_all(path.join("src")).unwrap();
        run_git(&path, &["init", "--quiet"]);
        run_git(&path, &["config", "user.email", "test@example.invalid"]);
        run_git(&path, &["config", "user.name", "Slop Gate Test"]);
        fs::write(path.join("src/lib.rs"), base_source).unwrap();
        run_git(&path, &["add", "src/lib.rs"]);
        run_git(&path, &["commit", "--quiet", "-m", "base"]);
        fs::write(path.join("src/lib.rs"), head_source).unwrap();
        run_git(&path, &["add", "src/lib.rs"]);
        run_git(&path, &["commit", "--quiet", "-m", "head"]);
        path
    }

    fn run_git(path: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success(), "git command failed: {args:?}");
    }
}
