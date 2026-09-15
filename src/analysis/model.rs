// Rust guideline compliant 2026-09-12
//! Serializable analysis facts independent of source-search infrastructure.

use serde::{Deserialize, Serialize};

/// A source file analyzed into gate-relevant function facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalyzedFile {
    /// Repository-relative path using `/` separators.
    pub path: String,
    /// Analyzer language identifier.
    pub language: String,
    /// BLAKE3 hash of the complete source file.
    pub content_hash: String,
    /// Functions extracted from the file in source order.
    pub functions: Vec<FunctionRecord>,
}

/// A stable matching hint for one declaration across two revisions.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FunctionIdentity {
    /// Analyzer language identifier.
    pub language: String,
    /// Repository-relative declaration path.
    pub path: String,
    /// Lexical module, trait, and implementation scopes followed by the name.
    pub qualified_name: String,
    /// The declaration category.
    pub kind: FunctionKind,
}

/// The category of a function declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FunctionKind {
    /// A free, associated, or trait function declaration.
    Function,
}

/// Deterministic structural facts about one named function.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionRecord {
    /// Matching identity for this function.
    pub identity: FunctionIdentity,
    /// Unqualified declared name as written in source.
    pub name: String,
    /// First declaration line, one-based.
    pub start_line: usize,
    /// Last declaration line, one-based.
    pub end_line: usize,
    /// BLAKE3 hash of the normalized token stream.
    pub normalized_hash: String,
    /// BLAKE3 hash of the normalized AST-shape stream.
    pub ast_hash: String,
    /// Number of nodes in the normalized AST-shape stream.
    pub ast_node_count: usize,
    /// Number of normalized tokens.
    pub token_count: usize,
    /// Non-blank, non-comment-only physical lines of source.
    pub sloc: usize,
    /// Cyclomatic complexity using the versioned Rust decision-node map.
    pub cc: u32,
    /// `cc * sqrt(sloc)`.
    pub mass: f64,
    /// Sorted unique BLAKE3-truncated hashes of five-token shingles.
    pub shingle_hashes: Vec<u64>,
    /// Sorted unique BLAKE3-truncated hashes of five-node AST shingles.
    pub ast_shingle_hashes: Vec<u64>,
}

/// Aggregate complexity facts for one analyzed repository revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepositorySummary {
    /// Sum of function mass across the analyzed revision.
    pub total_mass: f64,
    /// Sum of mass for functions with complexity above the configured cutoff.
    pub high_complexity_mass: f64,
    /// Share of total mass held by high-complexity functions.
    pub erosion_ratio: f64,
    /// Number of functions above the complexity cutoff.
    pub high_complexity_function_count: usize,
    /// Highest cyclomatic complexity among analyzed functions.
    pub maximum_function_cc: u32,
    /// Highest function mass among analyzed functions.
    pub maximum_function_mass: f64,
}

impl RepositorySummary {
    /// Computes aggregate complexity facts using the default erosion cutoff.
    pub fn from_files(files: &[AnalyzedFile]) -> Self {
        Self::from_files_with_cutoff(files, 10)
    }

    /// Computes aggregate complexity facts using a caller-supplied cutoff.
    pub fn from_files_with_cutoff(files: &[AnalyzedFile], complexity_cutoff: u32) -> Self {
        let functions = files.iter().flat_map(|file| file.functions.iter());
        let mut total_mass = 0.0;
        let mut high_complexity_mass = 0.0;
        let mut high_complexity_function_count = 0;
        let mut maximum_function_cc = 0;
        let mut maximum_function_mass: f64 = 0.0;

        for function in functions {
            total_mass += function.mass;
            maximum_function_cc = maximum_function_cc.max(function.cc);
            maximum_function_mass = maximum_function_mass.max(function.mass);
            if function.cc > complexity_cutoff {
                high_complexity_mass += function.mass;
                high_complexity_function_count += 1;
            }
        }

        let erosion_ratio = if total_mass == 0.0 {
            0.0
        } else {
            high_complexity_mass / total_mass
        };

        Self {
            total_mass,
            high_complexity_mass,
            erosion_ratio,
            high_complexity_function_count,
            maximum_function_cc,
            maximum_function_mass,
        }
    }
}

/// A normalized Rust lint-suppression observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LintSuppression {
    /// Suppression syntax category.
    pub(crate) kind: String,
    /// Sorted lint paths named by the attribute.
    pub(crate) lint_paths: Vec<String>,
    /// Stable fingerprint of the suppressed target.
    pub(crate) target_fingerprint: String,
    /// Target syntax kind used for reporting.
    pub(crate) target_kind: String,
    /// One-based attribute line.
    pub(crate) line: usize,
}

/// A Rust unsafe-surface construct and its primary token location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnsafeSurface {
    /// Versioned syntax pattern identifier.
    pub(crate) pattern_id: String,
    /// Normalized construct fingerprint used for base/head matching.
    pub(crate) fingerprint: String,
    /// One-based line containing the primary `unsafe` token.
    pub(crate) line: usize,
}

/// A normalized direct Cargo dependency declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DependencyEdge {
    /// Manifest-relative table path containing the declaration.
    pub(crate) table_path: String,
    /// Dependency key used by Cargo.
    pub(crate) dependency_key: String,
    /// Optional renamed package name.
    pub(crate) package: Option<String>,
    /// Source kind: registry, git, path, or unspecified.
    pub(crate) source_kind: String,
    /// Normalized source locator, when present.
    pub(crate) source: Option<String>,
    /// Whether Cargo default features are enabled.
    pub(crate) default_features: bool,
    /// Sorted explicitly enabled feature names.
    pub(crate) features: Vec<String>,
    /// Normalized version requirement, when present.
    pub(crate) version: Option<String>,
    /// One-based declaration line in the source manifest.
    pub(crate) line: usize,
}
