// Rust guideline compliant 2026-09-12
//! Narrow, fallible boundary around the local `git` executable.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

use crate::error::{Error, Result};

/// Executes Git commands for a repository boundary.
pub(crate) trait GitRunner: Send + Sync {
    /// Runs Git with the supplied working directory and arguments.
    fn run(&self, directory: &Path, arguments: &[&str]) -> Result<Output>;
}

/// Executes Git through the operating system process interface.
#[derive(Debug, Default)]
pub(crate) struct SystemGitRunner;

impl GitRunner for SystemGitRunner {
    fn run(&self, directory: &Path, arguments: &[&str]) -> Result<Output> {
        Command::new("git")
            .args(arguments)
            .current_dir(directory)
            .output()
            .map_err(|error| Error::Git {
                operation: "execute",
                detail: error.to_string(),
            })
    }
}

/// A local Git repository used as a source of immutable revision blobs.
#[derive(Clone)]
pub(crate) struct GitRepository {
    root: PathBuf,
    runner: Arc<dyn GitRunner>,
}

impl std::fmt::Debug for GitRepository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GitRepository")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl GitRepository {
    /// Opens the Git repository containing `path`.
    pub(crate) fn open(path: &Path) -> Result<Self> {
        Self::open_with_runner(path, Arc::new(SystemGitRunner))
    }

    /// Opens a repository through an injected Git runner.
    pub(crate) fn open_with_runner(path: &Path, runner: Arc<dyn GitRunner>) -> Result<Self> {
        let root = run_git(&*runner, path, &["rev-parse", "--show-toplevel"])?;
        let root = PathBuf::from(root.trim());
        if !root.is_dir() {
            return Err(Error::invalid(
                "Git repository root",
                format!("Git reported a non-directory root: {}", root.display()),
            ));
        }
        Ok(Self { root, runner })
    }

    /// Returns the canonical repository root reported by Git.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves a revision expression to its complete commit object ID.
    pub(crate) fn resolve_revision(&self, revision: &str) -> Result<String> {
        let expression = format!("{revision}^{{commit}}");
        self.run_git(&["rev-parse", "--verify", &expression])
            .map(|value| value.trim().to_string())
    }

    /// Lists Rust source paths tracked by one revision in stable Git order.
    pub(crate) fn rust_files(&self, revision: &str) -> Result<Vec<String>> {
        let output = self.run_git(&["ls-tree", "-r", "--name-only", revision])?;
        Ok(output
            .lines()
            .filter(|path| path.ends_with(".rs") && is_relative_path(path))
            .map(String::from)
            .collect())
    }

    /// Reads a UTF-8 blob from a revision and repository-relative path.
    pub(crate) fn read_blob(&self, revision: &str, path: &str) -> Result<String> {
        if !is_relative_path(path) {
            return Err(Error::invalid("Git path", format!("{path:?}")));
        }
        let specification = format!("{revision}:{path}");
        self.run_git(&["show", &specification])
    }

    /// Lists Rust files visible in the working tree, including untracked files.
    pub(crate) fn working_tree_rust_files(&self, include_ignored: bool) -> Result<Vec<String>> {
        let mut paths = self
            .run_git(&["ls-files", "--cached", "--others", "--exclude-standard"])?
            .lines()
            .filter(|path| path.ends_with(".rs") && is_relative_path(path))
            .map(String::from)
            .collect::<BTreeSet<_>>();
        if include_ignored {
            paths.extend(
                self.run_git(&["ls-files", "--others", "--ignored", "--exclude-standard"])?
                    .lines()
                    .filter(|path| path.ends_with(".rs") && is_relative_path(path))
                    .map(String::from),
            );
        }
        Ok(paths.into_iter().collect())
    }

    /// Reads a UTF-8 Rust source file from the working tree.
    pub(crate) fn read_working_tree(&self, path: &str) -> Result<String> {
        if !is_relative_path(path) {
            return Err(Error::invalid("Git path", format!("{path:?}")));
        }
        let absolute = self.root.join(path);
        std::fs::read_to_string(&absolute).map_err(|source| Error::Io {
            operation: "read working-tree source",
            path: absolute,
            source,
        })
    }

    /// Reads a revision blob, returning `None` when the path is absent.
    pub(crate) fn read_blob_if_exists(&self, revision: &str, path: &str) -> Result<Option<String>> {
        if !is_relative_path(path) {
            return Err(Error::invalid("Git path", format!("{path:?}")));
        }
        let specification = format!("{revision}:{path}");
        let output = self
            .runner
            .run(&self.root, &["cat-file", "-p", &specification])?;
        if output.status.success() {
            return String::from_utf8(output.stdout)
                .map(Some)
                .map_err(|error| Error::Utf8 {
                    subject: "Git blob",
                    detail: error.to_string(),
                });
        }
        if is_missing_blob_diagnostic(&output.stderr) {
            return Ok(None);
        }
        Err(Error::Git {
            operation: "read optional blob",
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }

    /// Returns changed head paths and rename mappings for `base..head`.
    pub(crate) fn changed_rust_files(&self, base: &str, head: &str) -> Result<ChangedFiles> {
        let output = self.run_git_bytes(&[
            "diff",
            "--name-status",
            "-z",
            "--find-renames",
            "--diff-filter=ACMR",
            base,
            head,
            "--",
        ])?;
        let mut changed = Vec::new();
        let mut cargo_paths = Vec::new();
        let mut renamed_from = std::collections::HashMap::new();
        let mut fields = output.split(|byte| *byte == b'\0');
        while let Some(status) = fields.next() {
            if status.is_empty() {
                continue;
            }
            let status = std::str::from_utf8(status).map_err(|error| Error::Utf8 {
                subject: "Git status",
                detail: error.to_string(),
            })?;
            if status.starts_with('R') || status.starts_with('C') {
                let Some(old_path) = fields.next() else {
                    return Err(Error::invalid("Git diff", "missing rename source path"));
                };
                let Some(new_path) = fields.next() else {
                    return Err(Error::invalid(
                        "Git diff",
                        "missing rename destination path",
                    ));
                };
                let old_path = std::str::from_utf8(old_path).map_err(|error| Error::Utf8 {
                    subject: "Git path",
                    detail: error.to_string(),
                })?;
                let new_path = std::str::from_utf8(new_path).map_err(|error| Error::Utf8 {
                    subject: "Git path",
                    detail: error.to_string(),
                })?;
                if is_supported_changed_path(new_path) {
                    changed.push(new_path.to_string());
                    if new_path.ends_with(".rs") {
                        if status.starts_with('R') && old_path.ends_with(".rs") {
                            renamed_from.insert(new_path.to_string(), old_path.to_string());
                        }
                    } else {
                        cargo_paths.push(new_path.to_string());
                    }
                    if status.starts_with('R') && is_supported_changed_path(old_path) {
                        renamed_from.insert(new_path.to_string(), old_path.to_string());
                    }
                }
            } else if let Some(path) = fields.next() {
                let path = std::str::from_utf8(path).map_err(|error| Error::Utf8 {
                    subject: "Git path",
                    detail: error.to_string(),
                })?;
                if is_supported_changed_path(path) {
                    changed.push(path.to_string());
                    if path.ends_with("Cargo.toml") {
                        cargo_paths.push(path.to_string());
                    }
                }
            }
        }
        changed.sort();
        changed.dedup();
        cargo_paths.sort();
        cargo_paths.dedup();
        changed.retain(|path| path.ends_with(".rs"));
        let added_lines = changed
            .iter()
            .chain(cargo_paths.iter())
            .map(|path| {
                self.added_lines(base, head, path)
                    .map(|lines| (path.clone(), lines))
            })
            .collect::<Result<std::collections::HashMap<_, _>>>()?;
        Ok(ChangedFiles {
            paths: changed,
            cargo_paths,
            renamed_from,
            added_lines,
        })
    }

    /// Returns head line numbers added by a zero-context Git diff.
    pub fn added_lines(&self, base: &str, head: &str, path: &str) -> Result<ChangedLineSet> {
        if !is_relative_path(path) {
            return Err(Error::invalid("Git path", format!("{path:?}")));
        }
        let output = self.run_git(&[
            "diff",
            "--unified=0",
            "--no-ext-diff",
            base,
            head,
            "--",
            path,
        ])?;
        parse_added_lines(&output)
    }

    fn run_git(&self, arguments: &[&str]) -> Result<String> {
        run_git(&*self.runner, &self.root, arguments)
    }

    fn run_git_bytes(&self, arguments: &[&str]) -> Result<Vec<u8>> {
        run_git_bytes(&*self.runner, &self.root, arguments)
    }
}

fn is_missing_blob_diagnostic(stderr: &[u8]) -> bool {
    let detail = String::from_utf8_lossy(stderr);
    detail.lines().any(|line| {
        let line = line.trim();
        line.starts_with("fatal: path '")
            && (line.contains(" does not exist in '")
                || line.contains(" exists on disk, but not in '"))
    })
}

/// Changed head paths and old-path mappings for detected renames.
#[derive(Debug, Default)]
pub(crate) struct ChangedFiles {
    /// Repository-relative paths in the head revision.
    pub(crate) paths: Vec<String>,
    /// Repository-relative `Cargo.toml` paths present in the head revision.
    pub(crate) cargo_paths: Vec<String>,
    /// Maps a renamed head path to its base path.
    pub(crate) renamed_from: std::collections::HashMap<String, String>,
    /// Maps each changed head path to its added head lines.
    pub(crate) added_lines: std::collections::HashMap<String, ChangedLineSet>,
}

/// One-based head-revision lines added by a Git diff.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangedLineSet {
    lines: BTreeSet<usize>,
}

impl ChangedLineSet {
    /// Returns whether a one-based head line is included.
    pub fn contains(&self, line: usize) -> bool {
        self.lines.contains(&line)
    }

    /// Returns the number of added lines.
    #[cfg_attr(not(test), expect(dead_code))]
    pub fn len(&self) -> usize {
        self.lines.len()
    }
}

fn parse_added_lines(diff: &str) -> Result<ChangedLineSet> {
    let mut lines = BTreeSet::new();
    for line in diff.lines().filter(|line| line.starts_with("@@")) {
        let Some(header) = line.strip_prefix("@@ ") else {
            return Err(Error::invalid("Git diff hunk", line));
        };
        let Some((ranges, _section)) = header.split_once(" @@") else {
            return Err(Error::invalid("Git diff hunk", line));
        };
        let mut fields = ranges.split(' ');
        let old_range = fields.next().filter(|value| value.starts_with('-'));
        let new_range = fields.next().filter(|value| value.starts_with('+'));
        if old_range.is_none() || new_range.is_none() || fields.next().is_some() {
            return Err(Error::invalid("Git diff hunk", line));
        }
        let Some(old_range) = old_range else {
            return Err(Error::invalid("Git diff hunk", line));
        };
        let Some(new_range) = new_range else {
            return Err(Error::invalid("Git diff hunk", line));
        };
        let (_old_start, _old_count) = parse_diff_range(old_range, line)?;
        let (new_start, new_count) = parse_diff_range(new_range, line)?;
        let end = new_start
            .checked_add(new_count)
            .ok_or_else(|| Error::invalid("Git diff hunk", line))?;
        lines.extend(new_start..end);
    }
    Ok(ChangedLineSet { lines })
}

fn parse_diff_range(range: &str, line: &str) -> Result<(usize, usize)> {
    let range = range
        .get(1..)
        .ok_or_else(|| Error::invalid("Git diff hunk", line))?;
    let (start, count) = range
        .split_once(',')
        .map_or((range, "1"), |(start, count)| (start, count));
    let start = start
        .parse::<usize>()
        .map_err(|_| Error::invalid("Git diff hunk", line))?;
    let count = count
        .parse::<usize>()
        .map_err(|_| Error::invalid("Git diff hunk", line))?;
    Ok((start, count))
}

fn is_supported_changed_path(path: &str) -> bool {
    is_relative_path(path) && (path.ends_with(".rs") || path.ends_with("Cargo.toml"))
}

fn run_git(runner: &dyn GitRunner, directory: &Path, arguments: &[&str]) -> Result<String> {
    String::from_utf8(run_git_bytes(runner, directory, arguments)?).map_err(|error| Error::Utf8 {
        subject: "Git output",
        detail: error.to_string(),
    })
}

fn run_git_bytes(runner: &dyn GitRunner, directory: &Path, arguments: &[&str]) -> Result<Vec<u8>> {
    let output = runner.run(directory, arguments)?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    Err(Error::Git {
        operation: "command",
        detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
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
    use std::fs;
    use std::os::unix::process::ExitStatusExt;
    use std::process::Command;
    use std::sync::Arc;

    use super::{
        ChangedLineSet, GitRepository, GitRunner, is_missing_blob_diagnostic, parse_added_lines,
    };

    struct FakeGitRunner;

    impl GitRunner for FakeGitRunner {
        fn run(
            &self,
            _directory: &std::path::Path,
            arguments: &[&str],
        ) -> crate::error::Result<std::process::Output> {
            assert_eq!(arguments, ["rev-parse", "--show-toplevel"]);
            Ok(std::process::Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: b".\n".to_vec(),
                stderr: Vec::new(),
            })
        }
    }

    #[test]
    fn opens_with_an_injected_git_runner() {
        let repository =
            GitRepository::open_with_runner(std::path::Path::new("."), Arc::new(FakeGitRunner))
                .unwrap();
        assert_eq!(repository.root(), std::path::Path::new("."));
    }

    #[test]
    fn parses_added_lines_from_multiple_hunks() {
        let lines = parse_added_lines("@@ -2,0 +3,2 @@\n@@ -8,2 +10,1 @@ fn example\n").unwrap();
        assert_eq!(lines, ChangedLineSet::from([3, 4, 10]));
    }

    #[test]
    fn new_file_hunk_contains_every_head_line() {
        let lines = parse_added_lines("@@ -0,0 +1,3 @@\n").unwrap();
        assert_eq!(lines.len(), 3);
        assert!(lines.contains(1));
        assert!(lines.contains(3));
    }

    #[test]
    fn deleted_and_unchanged_rename_hunks_are_empty() {
        assert_eq!(parse_added_lines("@@ -3,2 +3,0 @@\n").unwrap().len(), 0);
        assert_eq!(parse_added_lines("").unwrap().len(), 0);
    }

    #[test]
    fn accepts_git_headers_with_implicit_one_counts() {
        let lines = parse_added_lines("@@ -4 +5 @@\n").unwrap();
        assert!(lines.contains(5));
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn rejects_malformed_hunks_without_partial_results() {
        let result = parse_added_lines("@@ -1,1 +1,1 @@\n@@ malformed\n");
        assert!(result.is_err());
    }

    #[test]
    fn working_tree_files_include_untracked_but_respect_ignore_rules() {
        let path = std::env::temp_dir().join(format!("slop-gate-git-test-{}", std::process::id()));
        fs::create_dir_all(path.join("src")).unwrap();
        fs::write(path.join("src/tracked.rs"), "fn tracked() {}\n").unwrap();
        fs::write(path.join("src/untracked.rs"), "fn untracked() {}\n").unwrap();
        fs::write(path.join(".gitignore"), "ignored.rs\n").unwrap();
        fs::write(path.join("ignored.rs"), "fn ignored() {}\n").unwrap();
        run_test_git(&path, &["init", "--quiet"]);
        run_test_git(&path, &["config", "user.email", "test@example.invalid"]);
        run_test_git(&path, &["config", "user.name", "Slop Gate Test"]);
        run_test_git(&path, &["add", "src/tracked.rs", ".gitignore"]);
        run_test_git(&path, &["commit", "--quiet", "-m", "base"]);
        let repository = GitRepository::open(&path).unwrap();

        let visible = repository.working_tree_rust_files(false).unwrap();
        assert_eq!(visible, ["src/tracked.rs", "src/untracked.rs"]);
        let all = repository.working_tree_rust_files(true).unwrap();
        assert_eq!(all, ["ignored.rs", "src/tracked.rs", "src/untracked.rs"]);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn optional_blob_returns_none_for_a_missing_path() {
        let path = std::env::temp_dir().join(format!(
            "slop-gate-optional-blob-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join("src")).unwrap();
        fs::write(path.join("src/lib.rs"), "fn existing() {}\n").unwrap();
        run_test_git(&path, &["init", "--quiet"]);
        run_test_git(&path, &["config", "user.email", "test@example.invalid"]);
        run_test_git(&path, &["config", "user.name", "Slop Gate Test"]);
        run_test_git(&path, &["add", "src/lib.rs"]);
        run_test_git(&path, &["commit", "--quiet", "-m", "base"]);
        let repository = GitRepository::open(&path).unwrap();

        assert_eq!(
            repository
                .read_blob_if_exists("HEAD", "src/missing.rs")
                .unwrap(),
            None
        );
        assert!(
            repository
                .read_blob_if_exists("not-a-revision", "src/missing.rs")
                .is_err()
        );
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn missing_blob_detection_does_not_match_other_git_errors() {
        assert!(is_missing_blob_diagnostic(
            b"fatal: path 'tests/common/mod.rs' exists on disk, but not in 'abc'\n"
        ));
        assert!(is_missing_blob_diagnostic(
            b"fatal: path 'tests/common/mod.rs' does not exist in 'abc'\n"
        ));
        assert!(!is_missing_blob_diagnostic(
            b"fatal: invalid object name 'not-a-revision:src/missing.rs'.\n"
        ));
    }

    fn run_test_git(path: &std::path::Path, arguments: &[&str]) {
        let status = Command::new("git")
            .args(arguments)
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn added_lines_uses_the_narrow_zero_context_git_command() {
        struct AddedLinesRunner;

        impl GitRunner for AddedLinesRunner {
            fn run(
                &self,
                _directory: &std::path::Path,
                arguments: &[&str],
            ) -> crate::error::Result<std::process::Output> {
                assert_eq!(
                    arguments,
                    [
                        "diff",
                        "--unified=0",
                        "--no-ext-diff",
                        "base",
                        "head",
                        "--",
                        "src/lib.rs"
                    ]
                );
                Ok(std::process::Output {
                    status: std::process::ExitStatus::from_raw(0),
                    stdout: b"@@ -1 +2,2 @@\n".to_vec(),
                    stderr: Vec::new(),
                })
            }
        }

        let repository = GitRepository {
            root: std::path::PathBuf::from("."),
            runner: Arc::new(AddedLinesRunner),
        };
        let lines = repository
            .added_lines("base", "head", "src/lib.rs")
            .unwrap();
        assert_eq!(lines, ChangedLineSet::from([2, 3]));
    }

    impl<const N: usize> From<[usize; N]> for ChangedLineSet {
        fn from(lines: [usize; N]) -> Self {
            Self {
                lines: lines.into_iter().collect(),
            }
        }
    }
}
