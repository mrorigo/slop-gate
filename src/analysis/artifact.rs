// Rust guideline compliant 2026-09-12
//! Portable, versioned baseline artifact serialization.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

use super::AnalyzedFile;

/// Current on-disk schema version for [`IndexArtifact`].
pub const ARTIFACT_VERSION: u32 = 2;

const ANALYZER_RULESET: &str = "slop-gate-analysis-v2|rust-function-item|rust-cc-v1|normalized-token-v1|shingle-v1|normalized-ast-v1|ast-shingle-v1";

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
    /// Returns a schema-v2 artifact with files in stable path order.
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
        let artifact = Self {
            artifact_version: ARTIFACT_VERSION,
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            analyzer_fingerprint: expected_fingerprint.clone(),
            repository_commit,
            files,
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
        let artifact = serde_json::from_slice::<Self>(bytes)
            .map_err(|error| Error::invalid("artifact JSON", error))?;
        artifact.validate_with_fingerprint(expected_fingerprint)?;
        Ok(artifact)
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
                format!("{}; expected {}", self.artifact_version, ARTIFACT_VERSION),
            ));
        }
        if self.analyzer_fingerprint != expected_fingerprint {
            return Err(Error::invalid(
                "artifact analyzer fingerprint",
                "does not match this binary",
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
        Ok(())
    }
}

/// Returns the compatibility fingerprint for the compiled analyzer rules.
///
/// # Returns
///
/// Returns a stable BLAKE3 hexadecimal digest for the analyzer rule set.
pub fn analyzer_fingerprint() -> String {
    blake3::hash(ANALYZER_RULESET.as_bytes())
        .to_hex()
        .to_string()
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

fn is_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

#[cfg(test)]
mod tests {
    use super::IndexArtifact;
    use crate::analysis::analyze_rust_file;

    #[test]
    fn artifact_round_trip_is_valid() {
        let file = analyze_rust_file("src/lib.rs", "fn answer() -> u8 { 42 }\n").unwrap();
        let artifact = IndexArtifact::new("a".repeat(40), vec![file]).unwrap();
        let encoded = artifact.to_json().unwrap();
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
}
