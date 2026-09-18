// Rust guideline compliant 2026-09-15
//! Shared repository-relative path validation.

/// Returns whether a path is a non-empty, normalized relative repository path.
pub(crate) fn is_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}
