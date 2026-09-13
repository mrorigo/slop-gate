// Rust guideline compliant 2026-09-12
//! Command-line interface for `slop-gate`.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};

use crate::analysis::IndexArtifact;
use crate::config::GateConfig;
use crate::error::{Error, Result};
use crate::gate::{bind_policy, build_artifact, check_mass, render_human, scan_clones};
use crate::git::GitRepository;
use crate::sarif;

/// Parses arguments and executes the `slop-gate` command-line interface.
///
/// # Returns
///
/// Returns the process exit status: zero for a successful gate, one for gate
/// findings with error severity, and two for an operational failure.
pub fn main() -> ExitCode {
    let args = Args::parse();
    match args.command {
        Command::Index { ref_name, output } => run_index(&ref_name, &output),
        Command::Check {
            base,
            head,
            index,
            format,
        } => run_check(&base, &head, &index, format),
        Command::Scan { ref_name, format } => run_scan(&ref_name, format),
    }
}

/// Deterministic repository-aware code quality gates for CI.
#[derive(Debug, Parser)]
#[command(name = "slop-gate", version, about)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

/// Top-level product commands.
#[derive(Debug, Subcommand)]
enum Command {
    /// Build a baseline analysis artifact for a Git revision.
    Index {
        /// Git revision to analyze.
        #[arg(long = "ref")]
        ref_name: String,
        /// Destination for the versioned JSON artifact.
        #[arg(long)]
        output: PathBuf,
    },
    /// Evaluate changed functions against a baseline artifact.
    Check {
        /// Git base revision represented by the artifact.
        #[arg(long)]
        base: String,
        /// Git head revision under review.
        #[arg(long)]
        head: String,
        /// Versioned JSON artifact built for `--base`.
        #[arg(long)]
        index: PathBuf,
        /// Report encoding written to stdout.
        #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
        format: OutputFormat,
    },
    /// Audit one revision for structural near-clones.
    Scan {
        /// Git revision to audit.
        #[arg(long = "ref")]
        ref_name: String,
        /// Report encoding written to stdout.
        #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
        format: OutputFormat,
    },
}

/// Report encoding for completed gate commands.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    /// Stable human-readable diagnostics.
    Human,
    /// Structured JSON diagnostics.
    Json,
    /// SARIF 2.1.0 diagnostics for CI code-scanning integrations.
    Sarif,
}

/// Builds and writes a versioned baseline artifact.
fn run_index(ref_name: &str, output: &PathBuf) -> ExitCode {
    let result: Result<()> = (|| {
        let current_dir = std::env::current_dir().map_err(|source| Error::Io {
            operation: "determine current directory",
            path: PathBuf::from("."),
            source,
        })?;
        let repository = GitRepository::open(&current_dir)?;
        let config = GateConfig::load(repository.root())?;
        let mut artifact = build_artifact(&repository, ref_name)?;
        bind_policy(&mut artifact, &config)?;
        let json = artifact.to_json()?;
        if let Some(parent) = output.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| Error::Io {
                operation: "create artifact directory",
                path: parent.to_path_buf(),
                source,
            })?;
        }
        std::fs::write(output, json).map_err(|source| Error::Io {
            operation: "write artifact",
            path: output.clone(),
            source,
        })
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            write_error("index", &error);
            ExitCode::from(2)
        }
    }
}

/// Loads an artifact, evaluates function mass, and renders the report.
fn run_check(base: &str, head: &str, index: &PathBuf, format: OutputFormat) -> ExitCode {
    let result: Result<crate::gate::CheckReport> = (|| {
        let current_dir = std::env::current_dir().map_err(|source| Error::Io {
            operation: "determine current directory",
            path: PathBuf::from("."),
            source,
        })?;
        let repository = GitRepository::open(&current_dir)?;
        let config = GateConfig::load(repository.root())?;
        let raw = std::fs::read(index).map_err(|source| Error::Io {
            operation: "read artifact",
            path: index.clone(),
            source,
        })?;
        let artifact = IndexArtifact::from_json_with_fingerprint(
            &raw,
            &crate::analysis::analyzer_fingerprint_with_policy(&config.fingerprint()?),
        )?;
        check_mass(&repository, base, head, &artifact, &config)
    })();
    let report = match result {
        Ok(report) => report,
        Err(error) => {
            write_error("check", &error);
            return ExitCode::from(2);
        }
    };
    if let Err(error) = render_report(&report, format) {
        write_error("check", &error);
        return ExitCode::from(2);
    }
    if report.has_errors() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Builds a revision artifact in memory and audits it for near-clones.
fn run_scan(ref_name: &str, format: OutputFormat) -> ExitCode {
    let result: Result<crate::gate::CheckReport> = (|| {
        let current_dir = std::env::current_dir().map_err(|source| Error::Io {
            operation: "determine current directory",
            path: PathBuf::from("."),
            source,
        })?;
        let repository = GitRepository::open(&current_dir)?;
        let config = GateConfig::load(repository.root())?;
        let artifact = build_artifact(&repository, ref_name)?;
        Ok(scan_clones(&artifact, &config))
    })();
    let report = match result {
        Ok(report) => report,
        Err(error) => {
            write_error("scan", &error);
            return ExitCode::from(2);
        }
    };
    if let Err(error) = render_report(&report, format) {
        write_error("scan", &error);
        return ExitCode::from(2);
    }
    if report.has_errors() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Writes one report encoding to stdout.
fn render_report(report: &crate::gate::CheckReport, format: OutputFormat) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    render_report_to(&mut stdout, report, format)
}

/// Writes one report encoding to an injected output stream.
fn render_report_to(
    output: &mut impl Write,
    report: &crate::gate::CheckReport,
    format: OutputFormat,
) -> Result<()> {
    let rendered = match format {
        OutputFormat::Human => render_human(report),
        OutputFormat::Json => format!(
            "{}\n",
            serde_json::to_string_pretty(report)
                .map_err(|error| Error::invalid("JSON report", error))?
        ),
        OutputFormat::Sarif => format!(
            "{}\n",
            serde_json::to_string_pretty(&sarif::render(report))
                .map_err(|error| Error::invalid("SARIF report", error))?
        ),
    };
    output
        .write_all(rendered.as_bytes())
        .map_err(|source| Error::Io {
            operation: "write report",
            path: PathBuf::from("stdout"),
            source,
        })
}

/// Writes a best-effort operational diagnostic to the command's stderr stream.
fn write_error(command: &str, error: &Error) {
    let _ = writeln!(std::io::stderr().lock(), "slop-gate {command}: {error}");
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use clap::CommandFactory;

    use super::{Args, OutputFormat, render_report_to};
    use crate::gate::{CheckReport, Finding, Location, Severity};

    #[test]
    fn help_exposes_only_gate_commands() {
        let command = Args::command();
        let names: Vec<_> = command
            .get_subcommands()
            .map(|subcommand| subcommand.get_name())
            .collect();
        assert_eq!(names, ["index", "check", "scan"]);
    }

    #[test]
    fn writes_json_reports_to_an_injected_stream() {
        let mut output = Vec::new();
        render_report_to(&mut output, &CheckReport::default(), OutputFormat::Json).unwrap();
        assert_eq!(output, b"{\n  \"findings\": []\n}\n");
    }

    #[test]
    fn writes_lint_suppression_properties_to_json() {
        let mut output = Vec::new();
        let report = CheckReport {
            findings: vec![Finding {
                rule_id: "lint-suppression-growth".to_string(),
                severity: Severity::Warning,
                message: "new lint suppression introduced".to_string(),
                location: Location {
                    path: "src/lib.rs".to_string(),
                    line: 2,
                },
                base_location: None,
                base_mass: None,
                head_mass: None,
                delta: None,
                threshold: None,
                similarity: None,
                properties: BTreeMap::from([
                    ("pattern_id".to_string(), "allow".to_string()),
                    ("lint_paths".to_string(), "clippy::unwrap_used".to_string()),
                    ("target_kind".to_string(), "function_item".to_string()),
                ]),
            }],
        };
        render_report_to(&mut output, &report, OutputFormat::Json).unwrap();
        let document: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(document["findings"][0]["properties"]["pattern_id"], "allow");
    }
}
