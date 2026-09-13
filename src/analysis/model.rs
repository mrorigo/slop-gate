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
