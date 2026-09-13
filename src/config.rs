// Rust guideline compliant 2026-09-12
//! Repository-owned gate policy and suppression configuration.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The only supported configuration schema version.
const CONFIG_VERSION: u32 = 1;

/// Validated repository policy for gate rules.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GateConfig {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    pub(crate) rules: Rules,
    #[serde(default)]
    suppressions: Vec<Suppression>,
}

/// Per-rule thresholds and severities.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Rules {
    #[serde(default)]
    pub(crate) function_mass: FunctionMassRule,
    #[serde(default)]
    pub(crate) near_clone: NearCloneRule,
    #[serde(default)]
    pub(crate) lint_suppression: LintSuppressionRule,
    #[serde(default)]
    pub(crate) unsafe_surface: UnsafeSurfaceRule,
    #[serde(default)]
    pub(crate) dependency_surface: DependencySurfaceRule,
}

/// Lint-suppression growth policy.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LintSuppressionRule {
    /// Severity assigned to newly introduced or broadened suppressions.
    #[serde(default)]
    pub(crate) severity: RuleSeverity,
}

/// Unsafe-surface growth policy.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UnsafeSurfaceRule {
    /// Severity assigned to newly introduced unsafe constructs.
    #[serde(default)]
    pub(crate) severity: RuleSeverity,
}

/// Dependency-surface growth policy.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DependencySurfaceRule {
    /// Severity assigned to new or expanded dependency edges.
    #[serde(default)]
    pub(crate) severity: RuleSeverity,
}

/// Function-mass rule policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FunctionMassRule {
    #[serde(default)]
    pub(crate) severity: RuleSeverity,
    #[serde(default = "default_new_mass_limit")]
    pub(crate) new_function_limit: f64,
    #[serde(default = "default_delta_limit")]
    pub(crate) delta_limit: f64,
}

/// Near-clone rule policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NearCloneRule {
    #[serde(default)]
    pub(crate) severity: RuleSeverity,
    #[serde(default = "default_minimum_sloc")]
    pub(crate) minimum_sloc: usize,
    #[serde(default = "default_minimum_tokens")]
    pub(crate) minimum_tokens: usize,
    #[serde(default = "default_similarity_threshold")]
    pub(crate) similarity_threshold: f64,
    #[serde(default = "default_max_candidates")]
    pub(crate) max_candidates: usize,
}

/// Whether a rule is disabled, advisory, or merge-blocking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuleSeverity {
    Off,
    #[default]
    Warn,
    Error,
}

/// A justified exact-location exception to a rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Suppression {
    rule: String,
    path: String,
    line: Option<usize>,
    reason: String,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            rules: Rules::default(),
            suppressions: Vec::new(),
        }
    }
}

impl Default for FunctionMassRule {
    fn default() -> Self {
        Self {
            severity: RuleSeverity::Warn,
            new_function_limit: default_new_mass_limit(),
            delta_limit: default_delta_limit(),
        }
    }
}

impl Default for NearCloneRule {
    fn default() -> Self {
        Self {
            severity: RuleSeverity::Warn,
            minimum_sloc: default_minimum_sloc(),
            minimum_tokens: default_minimum_tokens(),
            similarity_threshold: default_similarity_threshold(),
            max_candidates: default_max_candidates(),
        }
    }
}

impl GateConfig {
    /// Loads `.slop-gate.toml` from the repository root, or warning defaults.
    pub(crate) fn load(root: &Path) -> Result<Self> {
        let path = root.join(".slop-gate.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path).map_err(|source| Error::Io {
            operation: "read configuration",
            path: path.clone(),
            source,
        })?;
        Self::from_toml(&raw).map_err(|error| {
            Error::invalid("configuration file", format!("{}: {error}", path.display()))
        })
    }

    /// Parses and validates one TOML configuration document.
    pub(crate) fn from_toml(raw: &str) -> Result<Self> {
        let config =
            toml::from_str::<Self>(raw).map_err(|error| Error::invalid("configuration", error))?;
        config.validate()?;
        Ok(config)
    }

    /// Returns whether a finding is suppressed by an exact configured exception.
    pub(crate) fn is_suppressed(&self, rule: &str, path: &str, line: usize) -> bool {
        self.suppressions.iter().any(|suppression| {
            suppression.rule == rule
                && suppression.path == path
                && suppression.line.is_none_or(|value| value == line)
        })
    }

    /// Returns a stable policy fingerprint for artifact compatibility checks.
    pub(crate) fn fingerprint(&self) -> Result<String> {
        let encoded = serde_json::to_vec(self)
            .map_err(|error| Error::invalid("configuration serialization", error))?;
        Ok(blake3::hash(&encoded).to_hex().to_string())
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.version != CONFIG_VERSION {
            return Err(Error::invalid(
                "configuration version",
                format!("{}; expected {}", self.version, CONFIG_VERSION),
            ));
        }
        if !self.rules.function_mass.new_function_limit.is_finite()
            || self.rules.function_mass.new_function_limit < 0.0
            || !self.rules.function_mass.delta_limit.is_finite()
            || self.rules.function_mass.delta_limit < 0.0
        {
            return Err(Error::invalid(
                "function-mass limits",
                "must be finite non-negative numbers",
            ));
        }
        let clone = &self.rules.near_clone;
        if !(0.0..=1.0).contains(&clone.similarity_threshold)
            || clone.minimum_sloc == 0
            || clone.minimum_tokens == 0
            || clone.max_candidates == 0
        {
            return Err(Error::invalid(
                "near-clone thresholds",
                "must be positive and similarity must be within 0.0..=1.0",
            ));
        }
        for suppression in &self.suppressions {
            if !matches!(
                suppression.rule.as_str(),
                "function-mass"
                    | "near-clone"
                    | "lint-suppression-growth"
                    | "unsafe-surface-growth"
                    | "dependency-surface-growth"
            ) || !is_relative_path(&suppression.path)
                || suppression.reason.trim().is_empty()
                || suppression.line == Some(0)
            {
                return Err(Error::invalid(
                    "suppression",
                    "requires a supported rule, relative path, positive optional line, and reason",
                ));
            }
        }
        Ok(())
    }
}

fn default_version() -> u32 {
    CONFIG_VERSION
}
fn default_new_mass_limit() -> f64 {
    80.0
}
fn default_delta_limit() -> f64 {
    20.0
}
fn default_minimum_sloc() -> usize {
    8
}
fn default_minimum_tokens() -> usize {
    40
}
fn default_similarity_threshold() -> f64 {
    0.85
}
fn default_max_candidates() -> usize {
    64
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
    use super::{GateConfig, RuleSeverity};

    #[test]
    fn defaults_are_warning_only() {
        let config = GateConfig::default();
        assert_eq!(config.rules.function_mass.severity, RuleSeverity::Warn);
        assert_eq!(config.rules.near_clone.severity, RuleSeverity::Warn);
        assert_eq!(config.rules.lint_suppression.severity, RuleSeverity::Warn);
    }

    #[test]
    fn rejects_unknown_configuration_keys() {
        let config = toml::from_str::<GateConfig>("unknown = true");
        assert!(config.is_err());
    }
}
