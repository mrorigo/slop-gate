// Rust guideline compliant 2026-09-12
//! Deterministic source analysis and baseline artifact support.

mod artifact;
mod extract;
mod model;
mod python;
mod typescript;

use crate::error::{Error, Result};

pub use artifact::{
    ARTIFACT_VERSION, IndexArtifact, analyzer_fingerprint, analyzer_fingerprint_with_policy,
    tool_version,
};
pub use extract::SHINGLE_SIZE;
pub use extract::analyze_rust_file;
pub(crate) use extract::dependency_edges;
pub(crate) use extract::lint_suppressions;
pub(crate) use extract::unsafe_surface;
pub(crate) use model::DependencyEdge;
pub(crate) use model::LintSuppression;
pub(crate) use model::UnsafeSurface;
pub use model::{
    AnalyzedFile, ContributorLocation, FunctionIdentity, FunctionKind, FunctionRecord,
    FunctionRole, RepositorySummary,
};
pub use python::analyze_python_file;
pub use typescript::analyze_typescript_file;

/// Analyzes a supported source file using the parser selected by its extension.
pub(crate) fn analyze_source_file(path: &str, source: &str) -> Result<AnalyzedFile> {
    if path.ends_with(".rs") {
        analyze_rust_file(path, source)
    } else if path.ends_with(".py") {
        analyze_python_file(path, source)
    } else if path.ends_with(".ts") || path.ends_with(".tsx") {
        analyze_typescript_file(path, source)
    } else {
        Err(Error::invalid(
            "source language",
            format!("unsupported path {path:?}"),
        ))
    }
}
