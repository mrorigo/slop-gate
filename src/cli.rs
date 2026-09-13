// Rust guideline compliant 2026-09-12
//! Command-line interface for `slop-gate`.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};

use crate::analysis::IndexArtifact;
use crate::config::GateConfig;
use crate::error::{Error, Result};
use crate::gate::{
    bind_policy, build_artifact, build_working_tree_artifact, check_mass, limit_scan_report,
    render_human, scan_clones,
};
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
        Command::Scan {
            ref_name,
            working_tree,
            path,
            no_ignore,
            threshold,
            min_sloc,
            top,
            format,
        } => run_scan(ScanOptions {
            ref_name: ref_name.as_deref(),
            working_tree,
            paths: &path,
            no_ignore,
            threshold,
            min_sloc,
            top,
            format,
        }),
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
        /// Git revision to audit; defaults to HEAD.
        #[arg(long = "ref", conflicts_with = "working_tree")]
        ref_name: Option<String>,
        /// Audit the current working tree, including untracked non-ignored files.
        #[arg(long, conflicts_with = "ref_name")]
        working_tree: bool,
        /// Restrict analysis to a file or directory. Repeat for multiple paths.
        #[arg(long, value_name = "PATH", action = clap::ArgAction::Append)]
        path: Vec<PathBuf>,
        /// Include files ignored by Git.
        #[arg(long)]
        no_ignore: bool,
        /// Override the near-clone similarity threshold for this scan.
        #[arg(long, value_name = "0.0..=1.0")]
        threshold: Option<f64>,
        /// Override the minimum function SLOC for this scan.
        #[arg(long)]
        min_sloc: Option<usize>,
        /// Limit output to the top N clone pairs.
        #[arg(long, value_name = "N")]
        top: Option<usize>,
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
struct ScanOptions<'a> {
    ref_name: Option<&'a str>,
    working_tree: bool,
    paths: &'a [PathBuf],
    no_ignore: bool,
    threshold: Option<f64>,
    min_sloc: Option<usize>,
    top: Option<usize>,
    format: OutputFormat,
}

fn run_scan(options: ScanOptions<'_>) -> ExitCode {
    let result: Result<crate::gate::CheckReport> = (|| {
        let current_dir = std::env::current_dir().map_err(|source| Error::Io {
            operation: "determine current directory",
            path: PathBuf::from("."),
            source,
        })?;
        let repository = GitRepository::open(&current_dir)?;
        let mut config = GateConfig::load(repository.root())?;
        if let Some(value) = options.threshold {
            config.rules.near_clone.similarity_threshold = value;
        }
        if let Some(value) = options.min_sloc {
            config.rules.near_clone.minimum_sloc = value;
        }
        config.validate()?;
        let mut artifact = if options.working_tree {
            build_working_tree_artifact(&repository, options.no_ignore)?
        } else {
            build_artifact(&repository, options.ref_name.unwrap_or("HEAD"))?
        };
        filter_artifact_paths(&mut artifact, repository.root(), options.paths)?;
        let mut report = scan_clones(&artifact, &config);
        if let Some(top) = options.top {
            limit_scan_report(&mut report, top)?;
        }
        Ok(report)
    })();
    let report = match result {
        Ok(report) => report,
        Err(error) => {
            write_error("scan", &error);
            return ExitCode::from(2);
        }
    };
    if let Err(error) = render_report(&report, options.format) {
        write_error("scan", &error);
        return ExitCode::from(2);
    }
    if report.has_errors() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn filter_artifact_paths(
    artifact: &mut IndexArtifact,
    root: &std::path::Path,
    paths: &[PathBuf],
) -> Result<()> {
    let filters = paths
        .iter()
        .map(|path| {
            let absolute = if path.is_absolute() {
                path.clone()
            } else {
                root.join(path)
            };
            let relative = absolute.strip_prefix(root).map_err(|_| {
                Error::invalid(
                    "scan path",
                    format!("{} is outside the repository", path.display()),
                )
            })?;
            if !absolute.exists() {
                return Err(Error::invalid(
                    "scan path",
                    format!("{} does not exist", path.display()),
                ));
            }
            Ok(relative.to_string_lossy().replace('\\', "/"))
        })
        .collect::<Result<Vec<_>>>()?;
    if filters.is_empty() || filters.iter().any(String::is_empty) {
        return Ok(());
    }
    artifact.files.retain(|file| {
        filters.iter().any(|filter| {
            file.path == *filter
                || file.path.starts_with(filter)
                    && file.path.as_bytes().get(filter.len()) == Some(&b'/')
        })
    });
    Ok(())
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
