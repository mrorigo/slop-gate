// Rust guideline compliant 2026-09-12
//! Typed recoverable errors for the Slop Gate application.

use std::fmt::Display;
use std::path::PathBuf;

use thiserror::Error;

/// Result type returned by Slop Gate's fallible APIs.
pub type AppResult<T> = std::result::Result<T, AppError>;

pub(crate) type Error = AppError;
pub(crate) type Result<T> = AppResult<T>;

/// Recoverable errors that prevent trustworthy analysis.
#[derive(Debug, Error)]
pub enum AppError {
    /// A filesystem operation failed.
    #[error("failed to {operation} {path}: {source}")]
    Io {
        /// The attempted operation.
        operation: &'static str,
        /// The affected filesystem path.
        path: PathBuf,
        /// The underlying operating-system failure.
        #[source]
        source: std::io::Error,
    },
    /// A source, artifact, configuration, or Git value violates a documented invariant.
    #[error("invalid {subject}: {detail}")]
    Invalid {
        /// The invalid value category.
        subject: &'static str,
        /// A concrete explanation suitable for a CLI diagnostic.
        detail: String,
    },
    /// A caller-supplied policy is malformed or violates policy constraints.
    #[error("invalid policy: {0}")]
    InvalidPolicy(String),
    /// A caller-supplied source snapshot or change manifest is inconsistent.
    #[error("invalid snapshot: {0}")]
    InvalidSnapshot(String),
    /// A baseline cannot be reused under the current schema or fingerprints.
    #[error("incompatible baseline: {0}")]
    IncompatibleBaseline(String),
    /// Snapshot index persistence failed.
    #[error("snapshot index persistence failed: {0}")]
    Persistence(String),
    /// A relevant source file failed parsing during analysis.
    #[error("source parse failed for {path}: {detail}")]
    ParseFailure {
        /// Repository-relative source path.
        path: String,
        /// Parser or decoder diagnostic.
        detail: String,
    },
    /// Required input could not be analyzed under its declared entry kind.
    #[error("unsupported required input: {0}")]
    UnsupportedRequiredInput(String),
    /// A caller-supplied analysis budget was exhausted.
    #[error("analysis budget exceeded: {0}")]
    BudgetExceeded(String),
    /// Analysis was cancelled by the caller.
    #[error("analysis cancelled")]
    Cancelled,
    /// A parser, serializer, or external command produced invalid UTF-8 text.
    #[error("invalid UTF-8 in {subject}: {detail}")]
    Utf8 {
        /// The output field that failed decoding.
        subject: &'static str,
        /// The conversion failure description.
        detail: String,
    },
    /// A Git command could not start or exited unsuccessfully.
    #[error("git {operation} failed: {detail}")]
    Git {
        /// The attempted Git operation.
        operation: &'static str,
        /// The command or process diagnostic.
        detail: String,
    },
}

impl AppError {
    /// Constructs an invariant error with a stable subject label.
    pub(crate) fn invalid(subject: &'static str, detail: impl Display) -> Self {
        Self::Invalid {
            subject,
            detail: detail.to_string(),
        }
    }
}
