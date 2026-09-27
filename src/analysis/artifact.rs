// Rust guideline compliant 2026-09-12
//! Portable, versioned baseline artifact serialization.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::path::is_relative_path;

use super::{AnalyzedFile, RepositorySummary};

/// Current on-disk schema version for [`IndexArtifact`].
///
/// Version 5 adds ordered shingle sequences, shingle line runs, and function
/// roles required by block-level duplicate detection.
pub const ARTIFACT_VERSION: u32 = 5;

const ANALYZER_RULESET: &str = "slop-gate-analysis-v3|rust-function-item|rust-cc-v1|normalized-token-v1|shingle-v1|ordered-shingle-v1|normalized-ast-v1|ast-shingle-v1|repository-summary-v1|function-role-v1";

/// Fingerprint of the analyzer sources, emitted by the build script.
const ANALYZER_SOURCE_FINGERPRINT: &str = env!("SLOP_GATE_ANALYZER_SOURCE_FINGERPRINT");

/// A portable baseline index for one exact Git revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexArtifact {
    /// Artifact schema version.
    pub artifact_version: u32,
    /// Crate version that created this artifact.
    pub tool_version: String,
    /// BLAKE3 fingerprint of analyzer rules whose changes invalidate artifacts.
    pub analyzer_fingerprint: String,
    /// Forty- or sixty-four-character hexadecimal Git object ID.
    pub repository_commit: String,
    /// Per-file analysis facts, sorted by repository-relative path.
    pub files: Vec<AnalyzedFile>,
    /// Aggregate complexity facts for the analyzed revision.
    pub summary: RepositorySummary,
}

impl IndexArtifact {
    /// Constructs and validates an artifact for a single Git revision.
    ///
    /// # Arguments
    ///
    /// * `repository_commit` - Complete Git object ID for the analyzed revision.
    /// * `files` - Function facts extracted from that revision.
    ///
    /// # Returns
    ///
    /// Returns a schema-v4 artifact with files in stable path order.
    ///
    /// # Errors
    ///
    /// Returns an error when the commit ID or analyzed file paths are invalid.
    pub fn new(repository_commit: String, files: Vec<AnalyzedFile>) -> Result<Self> {
        Self::new_with_fingerprint(repository_commit, files, analyzer_fingerprint())
    }

    /// Constructs an artifact with a caller-supplied analyzer-policy fingerprint.
    ///
    /// # Arguments
    ///
    /// * `repository_commit` - Complete Git object ID for the analyzed revision.
    /// * `files` - Function facts extracted from that revision.
    /// * `expected_fingerprint` - Compiled analyzer and repository-policy fingerprint.
    ///
    /// # Returns
    ///
    /// Returns a schema-v2 artifact with files in stable path order.
    ///
    /// # Errors
    ///
    /// Returns an error when the commit ID or analyzed file paths are invalid.
    pub fn new_with_fingerprint(
        repository_commit: String,
        mut files: Vec<AnalyzedFile>,
        expected_fingerprint: String,
    ) -> Result<Self> {
        files.sort_by(|left, right| left.path.cmp(&right.path));
        let summary = RepositorySummary::from_files(&files);
        let artifact = Self {
            artifact_version: ARTIFACT_VERSION,
            tool_version: tool_version(),
            analyzer_fingerprint: expected_fingerprint.clone(),
            repository_commit,
            files,
            summary,
        };
        artifact.validate_with_fingerprint(&expected_fingerprint)?;
        Ok(artifact)
    }

    /// Serializes this artifact as deterministic compact JSON.
    ///
    /// # Returns
    ///
    /// Returns UTF-8 JSON suitable for storage as a baseline artifact.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails.
    pub fn to_json(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(|error| Error::invalid("artifact JSON", error))
    }

    /// Deserializes and validates a baseline artifact.
    ///
    /// # Arguments
    ///
    /// * `bytes` - UTF-8 JSON artifact content.
    ///
    /// # Returns
    ///
    /// Returns the validated baseline artifact.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed JSON, an unsupported schema, or invalid
    /// artifact content.
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        Self::from_json_with_fingerprint(bytes, &analyzer_fingerprint())
    }

    /// Deserializes and validates an artifact under a repository policy.
    ///
    /// # Arguments
    ///
    /// * `bytes` - UTF-8 JSON artifact content.
    /// * `expected_fingerprint` - Compiled analyzer and repository-policy fingerprint.
    ///
    /// # Returns
    ///
    /// Returns the validated baseline artifact.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed JSON, an unsupported schema, or invalid
    /// artifact content.
    pub fn from_json_with_fingerprint(bytes: &[u8], expected_fingerprint: &str) -> Result<Self> {
        Self::reject_foreign_schema(bytes)?;
        let artifact = serde_json::from_slice::<Self>(bytes)
            .map_err(|error| Error::invalid("artifact JSON", error))?;
        artifact.validate_with_fingerprint(expected_fingerprint)?;
        Ok(artifact)
    }

    /// Rejects an artifact written for a different schema before decoding it.
    ///
    /// Deserializing a foreign artifact surfaces as a field-level decode error
    /// that names an internal type rather than the schema the caller has to
    /// rebuild, so the schema is checked first.
    fn reject_foreign_schema(bytes: &[u8]) -> Result<()> {
        #[derive(Deserialize)]
        struct SchemaHeader {
            artifact_version: Option<u32>,
            tool_version: Option<String>,
        }
        let header = serde_json::from_slice::<SchemaHeader>(bytes)
            .map_err(|error| Error::invalid("artifact JSON", error))?;
        if header.artifact_version == Some(ARTIFACT_VERSION) {
            return Ok(());
        }
        Err(Error::invalid(
            "artifact version",
            format!(
                "index schema {} was built by slop-gate {} and is not readable by slop-gate {}, which expects schema {}; rebuild the index with `slop-gate index`",
                header
                    .artifact_version
                    .map_or_else(|| "unknown".to_string(), |version| version.to_string()),
                header
                    .tool_version
                    .as_deref()
                    .unwrap_or("an unknown version"),
                tool_version(),
                ARTIFACT_VERSION
            ),
        ))
    }

    /// Validates compatibility and integrity constraints required for use.
    ///
    /// # Returns
    ///
    /// Returns successfully when this artifact is valid for the compiled analyzer.
    ///
    /// # Errors
    ///
    /// Returns an error when a schema, analyzer, commit, file path, or file
    /// ordering constraint is violated.
    pub fn validate(&self) -> Result<()> {
        self.validate_with_fingerprint(&analyzer_fingerprint())
    }

    /// Validates compatibility against a caller-supplied analyzer-policy fingerprint.
    ///
    /// # Arguments
    ///
    /// * `expected_fingerprint` - Compiled analyzer and repository-policy fingerprint.
    ///
    /// # Returns
    ///
    /// Returns successfully when this artifact is valid for that fingerprint.
    ///
    /// # Errors
    ///
    /// Returns an error when a schema, analyzer, commit, file path, or file
    /// ordering constraint is violated.
    pub fn validate_with_fingerprint(&self, expected_fingerprint: &str) -> Result<()> {
        if self.artifact_version != ARTIFACT_VERSION {
            return Err(Error::invalid(
                "artifact version",
                format!(
                    "index schema {} is not readable by slop-gate {} (expects schema {}); rebuild the index with `slop-gate index`",
                    self.artifact_version,
                    tool_version(),
                    ARTIFACT_VERSION
                ),
            ));
        }
        if self.analyzer_fingerprint != expected_fingerprint {
            return Err(Error::invalid(
                "artifact analyzer fingerprint",
                format!(
                    "index was built by slop-gate {} with a different analyzer or policy; this is slop-gate {}; rebuild the index with `slop-gate index`",
                    self.tool_version,
                    tool_version()
                ),
            ));
        }
        if self.tool_version != tool_version() {
            return Err(Error::invalid(
                "artifact tool version",
                format!(
                    "index was built by slop-gate {}, this is slop-gate {}; rebuild the index with `slop-gate index`",
                    self.tool_version,
                    tool_version()
                ),
            ));
        }
        if !is_git_object_id(&self.repository_commit) {
            return Err(Error::invalid(
                "artifact repository_commit",
                "must be a 40- or 64-character hexadecimal Git object ID",
            ));
        }
        for pair in self.files.windows(2) {
            if pair[0].path >= pair[1].path {
                return Err(Error::invalid(
                    "artifact files",
                    "must be strictly sorted by path",
                ));
            }
        }
        for file in &self.files {
            if !is_relative_path(&file.path) {
                return Err(Error::invalid(
                    "artifact file path",
                    format!("{:?}", file.path),
                ));
            }
            if file.language != "rust" {
                return Err(Error::invalid(
                    "artifact language",
                    format!("unsupported {:?}", file.language),
                ));
            }
        }
        let expected_summary = RepositorySummary::from_files(&self.files);
        if !self.summary.equivalent_to(&expected_summary) {
            return Err(Error::invalid(
                "artifact summary",
                format!(
                    "aggregate facts disagree with the indexed functions (index built by slop-gate {}); rebuild the index with `slop-gate index`",
                    self.tool_version
                ),
            ));
        }
        Ok(())
    }
}

/// Returns the compatibility fingerprint for the compiled analyzer rules.
///
/// The fingerprint combines the declared rule set with a build-script digest of
/// the analyzer sources, so an analyzer change cannot ship without
/// invalidating previously built artifacts.
///
/// # Returns
///
/// Returns a stable BLAKE3 hexadecimal digest for the analyzer rule set.
pub fn analyzer_fingerprint() -> String {
    blake3::hash(format!("{ANALYZER_RULESET}|{ANALYZER_SOURCE_FINGERPRINT}").as_bytes())
        .to_hex()
        .to_string()
}

/// Returns the running tool version.
///
/// # Returns
///
/// Returns the crate version compiled into this binary.
pub fn tool_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Combines compiled analyzer rules with a repository policy fingerprint.
///
/// # Arguments
///
/// * `policy` - Stable fingerprint of the active repository policy.
///
/// # Returns
///
/// Returns a stable BLAKE3 hexadecimal digest for the combined policy.
pub fn analyzer_fingerprint_with_policy(policy: &str) -> String {
    blake3::hash(format!("{ANALYZER_RULESET}|{policy}").as_bytes())
        .to_hex()
        .to_string()
}

fn is_git_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::{
        ANALYZER_SOURCE_FINGERPRINT, ARTIFACT_VERSION, IndexArtifact, analyzer_fingerprint,
        analyzer_fingerprint_with_policy, tool_version,
    };
    use crate::analysis::RepositorySummary;
    use crate::analysis::analyze_rust_file;

    #[test]
    fn artifact_round_trip_is_valid() {
        let file = analyze_rust_file("src/lib.rs", "fn answer() -> u8 { 42 }\n").unwrap();
        let artifact = IndexArtifact::new("a".repeat(40), vec![file]).unwrap();
        let encoded = artifact.to_json().unwrap();
        assert_eq!(artifact.to_json().unwrap(), encoded);
        assert_eq!(IndexArtifact::from_json(&encoded).unwrap(), artifact);
    }

    #[test]
    fn artifact_rejects_parent_path() {
        let mut file = analyze_rust_file("src/lib.rs", "fn answer() {}\n").unwrap();
        file.path = "../lib.rs".to_string();
        assert!(IndexArtifact::new("a".repeat(40), vec![file]).is_err());
    }

    #[test]
    fn artifact_rejects_the_previous_schema_version() {
        let file = analyze_rust_file("src/lib.rs", "fn answer() {}\n").unwrap();
        let artifact = IndexArtifact::new("a".repeat(40), vec![file]).unwrap();
        let mut document = serde_json::to_value(artifact).unwrap();
        document["artifact_version"] = serde_json::json!(1);
        let encoded = serde_json::to_vec(&document).unwrap();
        assert!(IndexArtifact::from_json(&encoded).is_err());
    }

    #[test]
    fn artifact_contains_deterministic_repository_summary() {
        let file = analyze_rust_file("src/lib.rs", "fn simple() {}\n").unwrap();
        let artifact = IndexArtifact::new("a".repeat(40), vec![file]).unwrap();

        assert_eq!(
            artifact.summary,
            RepositorySummary::from_files(&artifact.files)
        );
        assert_eq!(artifact.to_json().unwrap(), artifact.to_json().unwrap());
        assert!(artifact.summary.total_mass > 0.0);
        assert_eq!(artifact.summary.high_complexity_function_count, 0);
    }

    #[test]
    fn a_written_artifact_validates_against_its_own_facts() {
        // Floating point addition is not associative, so a sum recomputed after
        // a JSON round trip can differ from the stored sum by one unit in the
        // last place. Artifact validation must tolerate that, or `check` fails
        // on an artifact `index` just wrote.
        let mut functions = Vec::new();
        for index in 0..64 {
            let body =
                std::iter::repeat_n("    if flag { total += 1; }\n", 3 + index).collect::<String>();
            functions.push(analyze_rust_file(
                &format!("src/module_{index}.rs"),
                &format!("fn handler_{index}(flag: bool) -> usize {{\n    let mut total = 0;\n{body}    total\n}}\n"),
            )
            .unwrap());
        }
        let artifact = IndexArtifact::new("b".repeat(40), functions).unwrap();
        let encoded = artifact.to_json().unwrap();

        if let Err(error) = IndexArtifact::from_json(&encoded) {
            panic!("a freshly written artifact must load: {error}");
        }
    }

    #[test]
    fn a_foreign_schema_is_rejected_before_decoding() {
        // A schema change must not surface as a decode error naming an internal
        // type; it must name the schema and the rebuild.
        let encoded = br#"{"artifact_version":4,"tool_version":"0.4.0","files":[]}"#;
        let error = IndexArtifact::from_json(encoded).unwrap_err().to_string();

        assert!(error.contains("index schema 4"), "unhelpful: {error}");
        assert!(error.contains("0.4.0"), "unhelpful: {error}");
        assert!(error.contains(&tool_version()), "unhelpful: {error}");
        assert!(error.contains("rebuild the index"), "unhelpful: {error}");
    }

    #[test]
    fn artifact_mismatch_names_the_tool_version_on_both_sides() {
        let file = analyze_rust_file("src/lib.rs", "fn answer() -> u8 { 42 }\n").unwrap();
        let artifact = IndexArtifact::new("a".repeat(40), vec![file]).unwrap();
        let mut document = serde_json::to_value(artifact).unwrap();
        document["tool_version"] = serde_json::json!("0.4.0");
        let encoded = serde_json::to_vec(&document).unwrap();

        let error = IndexArtifact::from_json(&encoded).unwrap_err().to_string();
        assert!(error.contains("0.4.0"), "unhelpful: {error}");
        assert!(error.contains(&tool_version()), "unhelpful: {error}");
        assert!(error.contains("rebuild the index"), "unhelpful: {error}");
    }

    #[test]
    fn stale_schema_names_the_expected_schema() {
        let file = analyze_rust_file("src/lib.rs", "fn answer() -> u8 { 42 }\n").unwrap();
        let artifact = IndexArtifact::new("a".repeat(40), vec![file]).unwrap();
        let mut document = serde_json::to_value(artifact).unwrap();
        document["artifact_version"] = serde_json::json!(ARTIFACT_VERSION - 1);
        let encoded = serde_json::to_vec(&document).unwrap();

        let error = IndexArtifact::from_json(&encoded).unwrap_err().to_string();
        assert!(error.contains("index schema"), "unhelpful: {error}");
        assert!(
            error.contains(&ARTIFACT_VERSION.to_string()),
            "unhelpful: {error}"
        );
    }

    #[test]
    fn analyzer_fingerprint_covers_the_analyzer_sources() {
        // The compatibility key must change whenever analyzer code changes, so
        // a parser or normalization edit cannot ship without invalidating
        // previously built artifacts.
        assert_eq!(
            analyzer_fingerprint(),
            analyzer_fingerprint(),
            "fingerprint must be stable within a build"
        );
        assert_ne!(
            analyzer_fingerprint(),
            analyzer_fingerprint_with_policy("severity = \"error\""),
            "policy changes must change the fingerprint"
        );
        assert!(!ANALYZER_SOURCE_FINGERPRINT.is_empty());
    }

    #[test]
    fn artifact_rejects_inconsistent_repository_summary() {
        let file = analyze_rust_file("src/lib.rs", "fn answer() {}\n").unwrap();
        let artifact = IndexArtifact::new("a".repeat(40), vec![file]).unwrap();
        let mut document = serde_json::to_value(artifact).unwrap();
        document["summary"]["total_mass"] = serde_json::json!(999.0);
        let encoded = serde_json::to_vec(&document).unwrap();

        assert!(IndexArtifact::from_json(&encoded).is_err());
    }
}
