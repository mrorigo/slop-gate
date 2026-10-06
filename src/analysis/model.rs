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

/// The coarse structural role of a function, used to rank duplication risk.
///
/// Roles are deliberately conservative. A function is only classified as
/// boilerplate when its normalized shape is unambiguous, because a wrong
/// classification would hide a real duplicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FunctionRole {
    /// Test or test-support code, where duplication is usually acceptable.
    Test,
    /// A field or element accessor, where duplication is idiomatic.
    Accessor,
    /// A constructor or configuration mapper, where duplication is idiomatic.
    Constructor,
    /// Production logic with no recognized boilerplate shape.
    General,
}

impl FunctionRole {
    /// Returns whether duplication of this role is normally acceptable.
    ///
    /// # Returns
    ///
    /// Returns `true` for [`FunctionRole::Test`] and [`FunctionRole::Accessor`].
    pub fn is_low_risk(self) -> bool {
        matches!(self, Self::Test | Self::Accessor)
    }

    /// Returns the stable lowercase name used in findings.
    ///
    /// # Returns
    ///
    /// Returns one of `test`, `accessor`, `constructor`, or `general`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::Accessor => "accessor",
            Self::Constructor => "constructor",
            Self::General => "general",
        }
    }
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
    /// Cyclomatic complexity using the versioned decision-node map for its language.
    pub cc: u32,
    /// `cc * sqrt(sloc)`.
    pub mass: f64,
    /// Sorted unique BLAKE3-truncated hashes of five-token shingles.
    ///
    /// Hashes are truncated to 32 bits so a shingle can be compared against
    /// [`FunctionRecord::token_shingle_sequence`] without widening storage.
    /// Similarity is aggregated over hundreds of shingles, so a single
    /// collision cannot move a score materially.
    pub shingle_hashes: Vec<u32>,
    /// Sorted unique BLAKE3-truncated hashes of five-node AST shingles.
    pub ast_shingle_hashes: Vec<u32>,
    /// Ordered five-token shingle hashes, one per window start position.
    ///
    /// Unlike [`FunctionRecord::shingle_hashes`], this sequence preserves
    /// position so a contiguous island can be located inside a large function.
    #[serde(with = "hex_sequence")]
    pub token_shingle_sequence: Vec<u32>,
    /// Ordered five-node AST shingle hashes, one per window start position.
    #[serde(with = "hex_sequence")]
    pub ast_shingle_sequence: Vec<u32>,
    /// Run-length encoded start lines for [`FunctionRecord::token_shingle_sequence`].
    ///
    /// Each consecutive pair is a shingle count and the one-based line of that
    /// run's first shingle.
    pub token_line_runs: Vec<u32>,
    /// Run-length encoded start lines for [`FunctionRecord::ast_shingle_sequence`].
    pub ast_line_runs: Vec<u32>,
    /// Coarse structural role used to rank duplication risk.
    pub role: FunctionRole,
}

/// Compact hexadecimal encoding for ordered shingle sequences.
///
/// Ordered sequences are the largest artifact field by volume, so they are
/// stored as one hexadecimal string per function rather than a JSON integer
/// array. The decoded value is identical either way.
mod hex_sequence {
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(
        sequence: &[u32],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut encoded = String::with_capacity(sequence.len() * 8);
        for hash in sequence {
            encoded.push_str(&format!("{hash:08x}"));
        }
        serializer.serialize_str(&encoded)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u32>, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        if !encoded.len().is_multiple_of(8) {
            return Err(D::Error::custom(
                "shingle sequence length must be a multiple of eight hexadecimal characters",
            ));
        }
        encoded
            .as_bytes()
            .as_chunks::<8>()
            .0
            .iter()
            .map(|chunk| {
                std::str::from_utf8(chunk.as_slice())
                    .ok()
                    .and_then(|text| u32::from_str_radix(text, 16).ok())
                    .ok_or_else(|| D::Error::custom("shingle sequence is not hexadecimal"))
            })
            .collect()
    }
}

impl FunctionRecord {
    ///
    /// # Arguments
    ///
    /// * `index` - Zero-based position in [`FunctionRecord::token_shingle_sequence`].
    ///
    /// # Returns
    ///
    /// Returns the recorded line, or the function start line when the position
    /// falls outside the encoded runs.
    pub fn token_line_at(&self, index: usize) -> usize {
        run_encoded_line(&self.token_line_runs, index).unwrap_or(self.start_line)
    }

    /// Returns the one-based start line of an AST shingle position.
    ///
    /// # Arguments
    ///
    /// * `index` - Zero-based position in [`FunctionRecord::ast_shingle_sequence`].
    ///
    /// # Returns
    ///
    /// Returns the recorded line, or the function start line when the position
    /// falls outside the encoded runs.
    pub fn ast_line_at(&self, index: usize) -> usize {
        run_encoded_line(&self.ast_line_runs, index).unwrap_or(self.start_line)
    }
}

fn run_encoded_line(runs: &[u32], index: usize) -> Option<usize> {
    let mut remaining = index;
    for [count, line] in runs.as_chunks::<2>().0 {
        if remaining < *count as usize {
            return Some(*line as usize);
        }
        remaining -= *count as usize;
    }
    None
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
    /// Largest high-complexity functions, ordered by descending mass.
    pub top_contributors: Vec<ContributorLocation>,
}

/// A deterministic location contributing to structural erosion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContributorLocation {
    /// Repository-relative source path.
    pub path: String,
    /// Qualified declaration name.
    pub qualified_name: String,
    /// One-based declaration line.
    pub line: usize,
    /// Cyclomatic complexity.
    pub cc: u32,
    /// Function mass.
    pub mass: f64,
}

impl RepositorySummary {
    /// Returns whether two summaries agree to within floating-point noise.
    ///
    /// Artifact validation must not fail on a last-bit difference. Floating
    /// point addition is not associative, so a sum recomputed after a JSON
    /// round trip can differ from the stored sum by one unit in the last place
    /// even when nothing is wrong with the artifact. The comparison is exact
    /// for every count and identifier, and relative for every mass.
    ///
    /// # Arguments
    ///
    /// * `other` - Summary recomputed from the same function facts.
    ///
    /// # Returns
    ///
    /// Returns `true` when the two summaries describe the same measurements.
    pub fn equivalent_to(&self, other: &Self) -> bool {
        fn close(left: f64, right: f64) -> bool {
            const RELATIVE_TOLERANCE: f64 = 1e-9;
            let scale = left.abs().max(right.abs()).max(1.0);
            (left - right).abs() <= RELATIVE_TOLERANCE * scale
        }
        fn same_contributors(left: &[ContributorLocation], right: &[ContributorLocation]) -> bool {
            left.len() == right.len()
                && left.iter().zip(right).all(|(left, right)| {
                    left.path == right.path
                        && left.qualified_name == right.qualified_name
                        && left.line == right.line
                        && left.cc == right.cc
                        && close(left.mass, right.mass)
                })
        }
        close(self.total_mass, other.total_mass)
            && close(self.high_complexity_mass, other.high_complexity_mass)
            && close(self.erosion_ratio, other.erosion_ratio)
            && close(self.maximum_function_mass, other.maximum_function_mass)
            && self.high_complexity_function_count == other.high_complexity_function_count
            && self.maximum_function_cc == other.maximum_function_cc
            && same_contributors(&self.top_contributors, &other.top_contributors)
    }

    /// Computes aggregate complexity facts using the default erosion cutoff.
    pub fn from_files(files: &[AnalyzedFile]) -> Self {
        Self::from_files_with_cutoff(files, 10)
    }

    /// Computes aggregate complexity facts using a caller-supplied cutoff.
    pub fn from_files_with_cutoff(files: &[AnalyzedFile], complexity_cutoff: u32) -> Self {
        let functions = files.iter().flat_map(|file| file.functions.iter());
        let mut contributors = Vec::new();
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
                contributors.push(ContributorLocation {
                    path: function.identity.path.clone(),
                    qualified_name: function.identity.qualified_name.clone(),
                    line: function.start_line,
                    cc: function.cc,
                    mass: function.mass,
                });
            }
        }

        contributors.sort_by(|left, right| {
            right
                .mass
                .total_cmp(&left.mass)
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| left.line.cmp(&right.line))
                .then_with(|| left.qualified_name.cmp(&right.qualified_name))
        });
        contributors.truncate(3);

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
            top_contributors: contributors,
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
