// Rust guideline compliant 2026-09-12
//! Deterministic repository-aware code quality gates for CI.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod analysis;
pub mod cli;
mod config;
mod error;
mod gate;
mod git;
mod path;
mod sarif;

pub use error::{AppError, AppResult};
