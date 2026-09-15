// Rust guideline compliant 2026-09-12
//! Command-line interface for `slop-gate`.

use serde::Serialize;
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
        Command::Init { force } => run_init(force),
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
        Command::History {
            ref_name,
            count,
            format,
        } => run_history(&ref_name, count, format),
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
    /// Create a warning-only repository policy file.
    Init {
        /// Replace an existing `.slop-gate.toml`.
        #[arg(long)]
        force: bool,
    },
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
    /// Report complexity erosion across a bounded first-parent history.
    History {
        /// Revision at the end of the history window.
        #[arg(long = "ref", default_value = "HEAD")]
        ref_name: String,
        /// Number of commits to include, from 1 through 100.
        #[arg(long, default_value_t = 10)]
        count: usize,
        /// Report encoding written to stdout.
        #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
        format: OutputFormat,
    },
}

#[derive(Debug, Serialize)]
struct HistoryEntry {
    commit: String,
    total_mass: f64,
    erosion_ratio: f64,
    high_complexity_function_count: usize,
    maximum_function_cc: u32,
    maximum_function_mass: f64,
}

const POLICY_TEMPLATE: &str = r#"# Slop Gate policy. Missing values use warning-only defaults.
version = 1

[rules.function_mass]
severity = "warn"          # off | warn | error
new_function_limit = 80.0
delta_limit = 20.0

[rules.near_clone]
severity = "warn"          # off | warn | error
minimum_sloc = 8
minimum_tokens = 40
similarity_threshold = 0.85
max_candidates = 64

[rules.lint_suppression]
severity = "warn"

[rules.unsafe_surface]
severity = "warn"

[rules.dependency_surface]
severity = "warn"

[rules.structural_erosion]
severity = "warn"
erosion_limit = 0.50
delta_limit = 0.08
complexity_cutoff = 10
top_contributors = 3

# Add an exact-location exception only after review.
# [[suppressions]]
# rule = "near-clone"
# path = "src/compat.rs"
# line = 42
# reason = "Protocol compatibility requires this implementation."
"#;

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

/// Creates the repository policy file without overwriting it by default.
fn run_init(force: bool) -> ExitCode {
    let result = (|| {
        let path = init_policy(force)?;
        write_init_message(&path)
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            write_error("init", &error);
            ExitCode::from(2)
        }
    }
}

fn init_policy(force: bool) -> Result<PathBuf> {
    let current_dir = std::env::current_dir().map_err(|source| Error::Io {
        operation: "determine current directory",
        path: PathBuf::from("."),
        source,
    })?;
    let repository = GitRepository::open(&current_dir)?;
    let path = repository.root().join(".slop-gate.toml");
    GateConfig::from_toml(POLICY_TEMPLATE)?;
    write_policy_file(&path, force)?;
    Ok(path)
}

fn write_policy_file(path: &std::path::Path, force: bool) -> Result<()> {
    if !force && path.exists() {
        return Err(Error::invalid(
            "configuration file",
            format!(
                "{} already exists; pass --force to replace it",
                path.display()
            ),
        ));
    }
    if force {
        return std::fs::write(path, POLICY_TEMPLATE).map_err(|source| Error::Io {
            operation: "write configuration",
            path: path.to_path_buf(),
            source,
        });
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| Error::Io {
            operation: "create configuration",
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(POLICY_TEMPLATE.as_bytes())
        .map_err(|source| Error::Io {
            operation: "write configuration",
            path: path.to_path_buf(),
            source,
        })
}

fn write_init_message(path: &std::path::Path) -> Result<()> {
    writeln!(std::io::stdout().lock(), "created {}", path.display()).map_err(|source| Error::Io {
        operation: "write command output",
        path: PathBuf::from("stdout"),
        source,
    })
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
    let result = run_scan_result(&options);
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

fn run_history(ref_name: &str, count: usize, format: OutputFormat) -> ExitCode {
    let result = (|| {
        if !(1..=100).contains(&count) {
            return Err(Error::invalid("history count", "must be within 1..=100"));
        }
        let current_dir = std::env::current_dir().map_err(|source| Error::Io {
            operation: "determine current directory",
            path: PathBuf::from("."),
            source,
        })?;
        let repository = GitRepository::open(&current_dir)?;
        let config = GateConfig::load(repository.root())?;
        let cutoff = config.rules.structural_erosion.complexity_cutoff;
        repository
            .revision_history(ref_name, count)?
            .into_iter()
            .map(|revision| {
                let (commit, summary) =
                    crate::gate::summarize_revision(&repository, &revision, cutoff)?;
                Ok(HistoryEntry {
                    commit,
                    total_mass: summary.total_mass,
                    erosion_ratio: summary.erosion_ratio,
                    high_complexity_function_count: summary.high_complexity_function_count,
                    maximum_function_cc: summary.maximum_function_cc,
                    maximum_function_mass: summary.maximum_function_mass,
                })
            })
            .collect::<Result<Vec<_>>>()
    })();
    match result {
        Ok(entries) => {
            let rendered = match format {
                OutputFormat::Human => entries
                    .iter()
                    .map(|entry| {
                        format!(
                            "{}: erosion {:.2}% (mass {:.2}, high-CC {})\n",
                            entry.commit,
                            entry.erosion_ratio * 100.0,
                            entry.total_mass,
                            entry.high_complexity_function_count
                        )
                    })
                    .collect::<String>(),
                OutputFormat::Json => match serde_json::to_string_pretty(&entries) {
                    Ok(json) => json + "\n",
                    Err(error) => {
                        write_error("history", &Error::invalid("history JSON", error));
                        return ExitCode::from(2);
                    }
                },
                OutputFormat::Sarif => {
                    write_error(
                        "history",
                        &Error::invalid("history format", "supports only human and json"),
                    );
                    return ExitCode::from(2);
                }
            };
            print!("{rendered}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            write_error("history", &error);
            ExitCode::from(2)
        }
    }
}

fn run_scan_result(options: &ScanOptions<'_>) -> Result<crate::gate::CheckReport> {
    let current_dir = std::env::current_dir().map_err(|source| Error::Io {
        operation: "determine current directory",
        path: PathBuf::from("."),
        source,
    })?;
    let repository = GitRepository::open(&current_dir)?;
    let config = scan_config(options, repository.root())?;
    let mut artifact = scan_artifact(options, &repository)?;
    filter_artifact_paths(&mut artifact, repository.root(), options.paths)?;
    let mut report = scan_clones(&artifact, &config);
    if let Some(top) = options.top {
        limit_scan_report(&mut report, top)?;
    }
    Ok(report)
}

fn scan_config(options: &ScanOptions<'_>, root: &std::path::Path) -> Result<GateConfig> {
    let mut config = GateConfig::load(root)?;
    if let Some(value) = options.threshold {
        config.rules.near_clone.similarity_threshold = value;
    }
    if let Some(value) = options.min_sloc {
        config.rules.near_clone.minimum_sloc = value;
    }
    config.validate()?;
    Ok(config)
}

fn scan_artifact(options: &ScanOptions<'_>, repository: &GitRepository) -> Result<IndexArtifact> {
    if options.working_tree {
        build_working_tree_artifact(repository, options.no_ignore)
    } else {
        build_artifact(repository, options.ref_name.unwrap_or("HEAD"))
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
        assert_eq!(names, ["init", "index", "check", "scan", "history"]);
    }

    #[test]
    fn policy_template_is_valid_configuration() {
        super::GateConfig::from_toml(super::POLICY_TEMPLATE).unwrap();
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
