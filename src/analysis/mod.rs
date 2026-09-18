// Rust guideline compliant 2026-09-12
//! Deterministic source analysis and baseline artifact support.

mod artifact;
mod extract;
mod model;

pub use artifact::{
    ARTIFACT_VERSION, IndexArtifact, analyzer_fingerprint, analyzer_fingerprint_with_policy,
};
pub use extract::analyze_rust_file;
pub(crate) use extract::dependency_edges;
pub(crate) use extract::lint_suppressions;
pub(crate) use extract::unsafe_surface;
pub(crate) use model::DependencyEdge;
pub(crate) use model::LintSuppression;
pub(crate) use model::UnsafeSurface;
pub use model::{
    AnalyzedFile, ContributorLocation, FunctionIdentity, FunctionKind, FunctionRecord,
    RepositorySummary,
};
