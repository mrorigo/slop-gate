// Rust guideline compliant 2026-09-12
//! Rust function extraction and deterministic structural metrics.

use std::collections::BTreeSet;

use tree_sitter::{Node, Parser};

use crate::error::{Error, Result};

use super::{
    AnalyzedFile, DependencyEdge, FunctionIdentity, FunctionKind, FunctionRecord, LintSuppression,
    UnsafeSurface,
};

const LANGUAGE: &str = "rust";
const SHINGLE_SIZE: usize = 5;

/// Extracts direct production and build dependency edges from a Cargo manifest.
pub(crate) fn dependency_edges(source: &str) -> Result<Vec<DependencyEdge>> {
    let document = source
        .parse::<toml::Value>()
        .map_err(|error| Error::invalid("Cargo manifest", error))?;
    let mut edges = Vec::new();
    let Some(root) = document.as_table() else {
        return Err(Error::invalid("Cargo manifest", "root must be a table"));
    };
    for table_name in ["dependencies", "build-dependencies"] {
        if let Some(value) = root.get(table_name) {
            let table = value.as_table().ok_or_else(|| {
                Error::invalid("Cargo manifest", format!("{table_name} must be a table"))
            })?;
            collect_dependency_table(table, table_name, source, &mut edges)?;
        }
    }
    if let Some(value) = root.get("target") {
        let targets = value
            .as_table()
            .ok_or_else(|| Error::invalid("Cargo manifest", "target must be a table"))?;
        for (selector, target) in targets {
            let target = target.as_table().ok_or_else(|| {
                Error::invalid(
                    "Cargo manifest",
                    format!("target.{selector} must be a table"),
                )
            })?;
            for table_name in ["dependencies", "build-dependencies"] {
                if let Some(value) = target.get(table_name) {
                    let table = value.as_table().ok_or_else(|| {
                        Error::invalid(
                            "Cargo manifest",
                            format!("target.{selector}.{table_name} must be a table"),
                        )
                    })?;
                    collect_dependency_table(
                        table,
                        &format!("target.{selector}.{table_name}"),
                        source,
                        &mut edges,
                    )?;
                }
            }
        }
    }
    edges.sort_by(|left, right| {
        (&left.table_path, &left.dependency_key).cmp(&(&right.table_path, &right.dependency_key))
    });
    Ok(edges)
}

fn collect_dependency_table(
    table: &toml::map::Map<String, toml::Value>,
    table_path: &str,
    source: &str,
    edges: &mut Vec<DependencyEdge>,
) -> Result<()> {
    for (dependency_key, value) in table {
        let (package, source_kind, dependency_source, default_features, features, version) =
            match value {
                toml::Value::String(version) => (
                    None,
                    "registry".to_string(),
                    None,
                    true,
                    Vec::new(),
                    Some(version.clone()),
                ),
                toml::Value::Table(details) => {
                    let package = details
                        .get("package")
                        .and_then(toml::Value::as_str)
                        .map(str::to_string);
                    let (source_kind, dependency_source) = if let Some(git) =
                        details.get("git").and_then(toml::Value::as_str)
                    {
                        ("git".to_string(), Some(normalize_locator(git)))
                    } else if let Some(path) = details.get("path").and_then(toml::Value::as_str) {
                        ("path".to_string(), Some(normalize_locator(path)))
                    } else if details.get("version").is_some() {
                        ("registry".to_string(), None)
                    } else {
                        ("unspecified".to_string(), None)
                    };
                    let default_features = details
                        .get("default-features")
                        .and_then(toml::Value::as_bool)
                        .unwrap_or(true);
                    let mut features = details
                        .get("features")
                        .and_then(toml::Value::as_array)
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(toml::Value::as_str)
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    features.sort();
                    features.dedup();
                    let version = details
                        .get("version")
                        .and_then(toml::Value::as_str)
                        .map(str::to_string);
                    (
                        package,
                        source_kind,
                        dependency_source,
                        default_features,
                        features,
                        version,
                    )
                }
                _ => {
                    return Err(Error::invalid(
                        "Cargo dependency",
                        format!("{table_path}.{dependency_key} must be a string or table"),
                    ));
                }
            };
        edges.push(DependencyEdge {
            table_path: table_path.to_string(),
            dependency_key: dependency_key.clone(),
            package,
            source_kind,
            source: dependency_source,
            default_features,
            features,
            version,
            line: dependency_line(source, table_path, dependency_key),
        });
    }
    Ok(())
}

fn dependency_line(source: &str, table_path: &str, dependency_key: &str) -> usize {
    let expected_header = format!("[{table_path}]");
    let mut in_table = false;
    for (line_number, raw_line) in source.lines().enumerate() {
        let line = raw_line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_table = canonical_header(line) == expected_header;
            continue;
        }
        if in_table && let Some((key, _value)) = line.split_once('=') {
            let key = key.trim();
            if key == dependency_key || key == format!("\"{dependency_key}\"") {
                return line_number + 1;
            }
        }
    }
    1
}

fn canonical_header(header: &str) -> String {
    header
        .chars()
        .filter(|character| !matches!(character, '\'' | '"'))
        .collect()
}

fn normalize_locator(locator: &str) -> String {
    let locator = locator.trim();
    if locator == "." {
        return locator.to_string();
    }
    locator.trim_end_matches('/').to_string()
}

/// Analyzes all named Rust functions in one repository-relative source file.
///
/// The file must be valid UTF-8 because Rust source analysis operates on text.
/// A syntax-error tree is rejected rather than producing incomplete gate facts.
///
/// # Arguments
///
/// * `path` - A repository-relative path using `/` separators.
/// * `source` - Complete UTF-8 Rust source for that path.
///
/// # Returns
///
/// One deterministic file record whose functions are ordered by source location.
///
/// # Errors
///
/// Returns an error if the path is invalid, the parser is unavailable, or the
/// source contains Tree-sitter syntax errors.
pub fn analyze_rust_file(path: &str, source: &str) -> Result<AnalyzedFile> {
    if !is_relative_path(path) {
        return Err(Error::invalid(
            "repository-relative path",
            format!("{path:?}"),
        ));
    }
    let mut parser = Parser::new();
    let language = tree_sitter_rust::LANGUAGE.into();
    parser
        .set_language(&language)
        .map_err(|error| Error::invalid("Rust parser", error))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| Error::invalid("Rust parser", "did not produce a syntax tree"))?;
    if tree.root_node().has_error() {
        return Err(Error::invalid("Rust source", "contains syntax errors"));
    }

    let mut functions = Vec::new();
    collect_functions(tree.root_node(), source, path, &[], &mut functions)?;
    Ok(AnalyzedFile {
        path: path.to_string(),
        language: LANGUAGE.to_string(),
        content_hash: blake3::hash(source.as_bytes()).to_hex().to_string(),
        functions,
    })
}

/// Extracts normalized `allow`, `expect`, and conditional allow attributes.
pub(crate) fn lint_suppressions(path: &str, source: &str) -> Result<Vec<LintSuppression>> {
    if !is_relative_path(path) {
        return Err(Error::invalid(
            "repository-relative path",
            format!("{path:?}"),
        ));
    }
    let mut parser = Parser::new();
    let language = tree_sitter_rust::LANGUAGE.into();
    parser
        .set_language(&language)
        .map_err(|error| Error::invalid("Rust parser", error))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| Error::invalid("Rust parser", "did not produce a syntax tree"))?;
    if tree.root_node().has_error() {
        return Err(Error::invalid("Rust source", "contains syntax errors"));
    }
    let mut result = Vec::new();
    collect_lint_suppressions(tree.root_node(), source, &mut result)?;
    result.sort_by(|left, right| {
        (left.line, &left.kind, &left.lint_paths).cmp(&(right.line, &right.kind, &right.lint_paths))
    });
    Ok(result)
}

/// Extracts the five versioned Rust unsafe-surface syntax forms.
pub(crate) fn unsafe_surface(path: &str, source: &str) -> Result<Vec<UnsafeSurface>> {
    if !is_relative_path(path) {
        return Err(Error::invalid(
            "repository-relative path",
            format!("{path:?}"),
        ));
    }
    let mut parser = Parser::new();
    let language = tree_sitter_rust::LANGUAGE.into();
    parser
        .set_language(&language)
        .map_err(|error| Error::invalid("Rust parser", error))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| Error::invalid("Rust parser", "did not produce a syntax tree"))?;
    if tree.root_node().has_error() {
        return Err(Error::invalid("Rust source", "contains syntax errors"));
    }
    let mut result = Vec::new();
    collect_unsafe_surface(tree.root_node(), source, &mut result)?;
    result.sort_by_key(|fact| (fact.line, fact.pattern_id.clone()));
    Ok(result)
}

fn collect_unsafe_surface(
    node: Node<'_>,
    source: &str,
    result: &mut Vec<UnsafeSurface>,
) -> Result<()> {
    let pattern_id = match node.kind() {
        "unsafe_block" => Some("unsafe-block"),
        "function_item" if declaration_has_unsafe(node, source, "fn")? => Some("unsafe-function"),
        "trait_item" if declaration_has_unsafe(node, source, "trait")? => Some("unsafe-trait"),
        "impl_item" if declaration_has_unsafe(node, source, "impl")? => Some("unsafe-impl"),
        "foreign_mod_item" if declaration_has_unsafe(node, source, "extern")? => {
            Some("unsafe-extern-block")
        }
        _ => None,
    };
    if let Some(pattern_id) = pattern_id {
        result.push(UnsafeSurface {
            pattern_id: pattern_id.to_string(),
            fingerprint: blake3::hash(surface_tokens(node, source)?.join("\u{1f}").as_bytes())
                .to_hex()
                .to_string(),
            line: unsafe_token_line(node, source)?,
        });
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_unsafe_surface(child, source, result)?;
    }
    Ok(())
}

fn surface_tokens(node: Node<'_>, source: &str) -> Result<Vec<String>> {
    if matches!(node.kind(), "line_comment" | "block_comment") {
        return Ok(Vec::new());
    }
    if node.child_count() == 0 {
        if node.kind().contains("literal") || matches!(node.kind(), "true" | "false") {
            return Ok(vec!["LIT".to_string()]);
        }
        return Ok(vec![node_text(node, source)?.to_string()]);
    }
    let mut tokens = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        tokens.extend(surface_tokens(child, source)?);
    }
    Ok(tokens)
}

fn declaration_has_unsafe(node: Node<'_>, source: &str, keyword: &str) -> Result<bool> {
    let text = node_text(node, source)?;
    let Some(keyword_position) = find_word(text, keyword) else {
        return Ok(false);
    };
    Ok(find_word(&text[..keyword_position], "unsafe").is_some())
}

fn unsafe_token_line(node: Node<'_>, source: &str) -> Result<usize> {
    let text = node_text(node, source)?;
    let position = find_word(text, "unsafe")
        .ok_or_else(|| Error::invalid("Rust unsafe surface", "missing primary unsafe token"))?;
    Ok(node.start_position().row + text[..position].matches('\n').count() + 1)
}

fn find_word(text: &str, word: &str) -> Option<usize> {
    text.match_indices(word).find_map(|(position, _)| {
        let before = text[..position].bytes().next_back();
        let after = text[position + word.len()..].bytes().next();
        (before.is_none_or(|byte| !byte.is_ascii_alphanumeric() && byte != b'_')
            && after.is_none_or(|byte| !byte.is_ascii_alphanumeric() && byte != b'_'))
        .then_some(position)
    })
}

fn collect_lint_suppressions(
    node: Node<'_>,
    source: &str,
    result: &mut Vec<LintSuppression>,
) -> Result<()> {
    if matches!(node.kind(), "attribute_item" | "inner_attribute_item") {
        let text = node_text(node, source)?;
        if let Some((kind, lint_paths)) = parse_suppression(text) {
            let target = if node.kind() == "inner_attribute_item" {
                None
            } else {
                node.next_named_sibling()
            };
            let target_fingerprint = target
                .map(|target| normalized_tokens(target, source, true))
                .transpose()?
                .map(|tokens| {
                    blake3::hash(tokens.join("\u{1f}").as_bytes())
                        .to_hex()
                        .to_string()
                })
                .unwrap_or_else(|| "file".to_string());
            result.push(LintSuppression {
                kind,
                lint_paths,
                target_fingerprint,
                target_kind: target
                    .map_or_else(|| "file".to_string(), |node| node.kind().to_string()),
                line: node.start_position().row + 1,
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_lint_suppressions(child, source, result)?;
    }
    Ok(())
}

fn parse_suppression(text: &str) -> Option<(String, Vec<String>)> {
    let body = text
        .trim()
        .strip_prefix("#![")
        .or_else(|| text.trim().strip_prefix("#["))?;
    let body = body.strip_suffix("]")?.trim();
    let (kind, arguments) = if let Some(arguments) = body.strip_prefix("allow(") {
        ("allow", arguments.strip_suffix(')')?)
    } else if let Some(arguments) = body.strip_prefix("expect(") {
        ("expect", arguments.strip_suffix(')')?)
    } else {
        let arguments = body.strip_prefix("cfg_attr(")?;
        let allow = arguments.find("allow(")?;
        (
            "cfg-allow",
            arguments[allow + "allow(".len()..].strip_suffix("))")?,
        )
    };
    let mut lint_paths = arguments
        .split(',')
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    lint_paths.sort();
    lint_paths.dedup();
    (!lint_paths.is_empty()).then(|| (kind.to_string(), lint_paths))
}

fn collect_functions(
    node: Node<'_>,
    source: &str,
    path: &str,
    scopes: &[String],
    functions: &mut Vec<FunctionRecord>,
) -> Result<()> {
    let mut child_scopes = scopes.to_vec();
    if let Some(scope) = scope_name(node, source)? {
        child_scopes.push(scope);
    }

    if node.kind() == "function_item" {
        functions.push(record_function(node, source, path, scopes)?);
        return Ok(());
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_functions(child, source, path, &child_scopes, functions)?;
    }
    Ok(())
}

fn scope_name(node: Node<'_>, source: &str) -> Result<Option<String>> {
    let name = match node.kind() {
        "mod_item" | "trait_item" => node.child_by_field_name("name"),
        "impl_item" => node.child_by_field_name("type"),
        _ => None,
    };
    name.map(|name| node_text(name, source).map(String::from))
        .transpose()
}

fn record_function(
    node: Node<'_>,
    source: &str,
    path: &str,
    scopes: &[String],
) -> Result<FunctionRecord> {
    let name_node = node
        .child_by_field_name("name")
        .ok_or_else(|| Error::invalid("Rust function", "is missing a name"))?;
    let name = node_text(name_node, source)?.to_string();
    let mut qualified_parts = scopes.to_vec();
    qualified_parts.push(name.clone());
    let tokens = normalized_tokens(node, source, true)?;
    let token_count = tokens.len();
    let normalized_hash = blake3::hash(tokens.join("\u{1f}").as_bytes())
        .to_hex()
        .to_string();
    let shingle_hashes = shingle_hashes(&tokens);
    let sloc = physical_sloc(node, source)?;
    let cc = cyclomatic_complexity(node, source)?;

    Ok(FunctionRecord {
        identity: FunctionIdentity {
            language: LANGUAGE.to_string(),
            path: path.to_string(),
            qualified_name: qualified_parts.join("::"),
            kind: FunctionKind::Function,
        },
        name,
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        normalized_hash,
        token_count,
        sloc,
        cc,
        mass: f64::from(cc) * (sloc as f64).sqrt(),
        shingle_hashes,
    })
}

fn normalized_tokens(node: Node<'_>, source: &str, is_root: bool) -> Result<Vec<String>> {
    if !is_root && node.kind() == "function_item" {
        return Ok(Vec::new());
    }
    if node.kind() == "line_comment" || node.kind() == "block_comment" {
        return Ok(Vec::new());
    }
    if node.child_count() == 0 {
        return Ok(vec![normalize_leaf(node, source)?]);
    }
    let mut tokens = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        tokens.extend(normalized_tokens(child, source, false)?);
    }
    Ok(tokens)
}

fn normalize_leaf(node: Node<'_>, source: &str) -> Result<String> {
    let kind = node.kind();
    if kind.contains("identifier") || kind == "self" || kind == "super" || kind == "crate" {
        return Ok("ID".to_string());
    }
    if kind.contains("literal") || kind == "true" || kind == "false" {
        return Ok("LIT".to_string());
    }
    let text = node_text(node, source)?;
    if text.starts_with('"')
        || text.starts_with('\'')
        || text.as_bytes().first().is_some_and(u8::is_ascii_digit)
    {
        return Ok("LIT".to_string());
    }
    Ok(text.to_string())
}

fn shingle_hashes(tokens: &[String]) -> Vec<u64> {
    if tokens.len() < SHINGLE_SIZE {
        return Vec::new();
    }
    let mut hashes = BTreeSet::new();
    for window in tokens.windows(SHINGLE_SIZE) {
        let hash = blake3::hash(window.join("\u{1f}").as_bytes());
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(&hash.as_bytes()[..8]);
        hashes.insert(u64::from_le_bytes(bytes));
    }
    hashes.into_iter().collect()
}

fn physical_sloc(node: Node<'_>, source: &str) -> Result<usize> {
    let mut comments = Vec::new();
    collect_comment_ranges(node, true, &mut comments);
    let start = node.start_byte();
    let end = node.end_byte();
    let mut count = 0;
    let bytes = source.as_bytes();
    let mut line_start = start;
    while line_start < end {
        let line_end = bytes[line_start..end]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(end, |offset| line_start + offset);
        if line_has_code(&bytes[line_start..line_end], line_start, &comments) {
            count += 1;
        }
        line_start = line_end.saturating_add(1);
    }
    Ok(count)
}

fn collect_comment_ranges(node: Node<'_>, is_root: bool, comments: &mut Vec<(usize, usize)>) {
    if !is_root && node.kind() == "function_item" {
        return;
    }
    if matches!(node.kind(), "line_comment" | "block_comment") {
        comments.push((node.start_byte(), node.end_byte()));
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_comment_ranges(child, false, comments);
    }
}

fn line_has_code(line: &[u8], absolute_start: usize, comments: &[(usize, usize)]) -> bool {
    line.iter().enumerate().any(|(offset, byte)| {
        !byte.is_ascii_whitespace()
            && !comments.iter().any(|(start, end)| {
                let position = absolute_start + offset;
                *start <= position && position < *end
            })
    })
}

fn cyclomatic_complexity(node: Node<'_>, source: &str) -> Result<u32> {
    let mut complexity = 1_u32;
    collect_complexity(node, source, true, &mut complexity)?;
    Ok(complexity)
}

fn collect_complexity(
    node: Node<'_>,
    source: &str,
    is_root: bool,
    complexity: &mut u32,
) -> Result<()> {
    if !is_root && node.kind() == "function_item" {
        return Ok(());
    }
    match node.kind() {
        "if_expression" | "for_expression" | "while_expression" | "loop_expression"
        | "try_expression" => *complexity += 1,
        "match_arm" if !is_wildcard_match_arm(node, source)? => *complexity += 1,
        "&&" | "||" => *complexity += 1,
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_complexity(child, source, false, complexity)?;
    }
    Ok(())
}

fn is_wildcard_match_arm(node: Node<'_>, source: &str) -> Result<bool> {
    let Some(pattern) = node.child_by_field_name("pattern") else {
        return Ok(false);
    };
    Ok(node_text(pattern, source)?.trim() == "_")
}

fn node_text<'a>(node: Node<'_>, source: &'a str) -> Result<&'a str> {
    node.utf8_text(source.as_bytes())
        .map_err(|error| Error::Utf8 {
            subject: "Tree-sitter node range",
            detail: error.to_string(),
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
    use super::{analyze_rust_file, dependency_edges, lint_suppressions};

    #[test]
    fn extracts_scoped_function_and_metrics() {
        let source = r#"
mod parser {
    // This comment does not count as SLOC.
    fn decide(value: Option<bool>) -> bool {
        if let Some(true) = value && value.is_some() {
            true
        } else {
            match value {
                Some(false) => false,
                _ => true,
            }
        }
    }
}
"#;
        let file = analyze_rust_file("src/parser.rs", source).unwrap();
        assert_eq!(file.functions.len(), 1);
        let function = &file.functions[0];
        assert_eq!(function.identity.qualified_name, "parser::decide");
        assert_eq!(function.sloc, 10);
        assert_eq!(function.cc, 4);
        assert!(function.mass > 12.0);
        assert!(!function.shingle_hashes.is_empty());
    }

    #[test]
    fn ignores_comments_and_identifier_renames_in_fingerprint() {
        let first =
            analyze_rust_file("src/a.rs", "fn add(left: i32) -> i32 { left + 1 }\n").unwrap();
        let second = analyze_rust_file(
            "src/a.rs",
            "// cosmetic\nfn add(right: i32) -> i32 { right + 999 }\n",
        )
        .unwrap();
        assert_eq!(
            first.functions[0].normalized_hash,
            second.functions[0].normalized_hash
        );
    }

    #[test]
    fn rejects_invalid_rust() {
        assert!(analyze_rust_file("src/broken.rs", "fn broken( {").is_err());
    }

    #[test]
    fn analyzes_checked_in_fixture() {
        let fixture = include_str!("../../tests/fixtures/rust_analysis.rs");
        let file = analyze_rust_file("tests/fixtures/rust_analysis.rs", fixture).unwrap();
        assert_eq!(file.functions.len(), 1);
        assert_eq!(
            file.functions[0].identity.qualified_name,
            "utility::Parser::parse_tokens"
        );
    }

    #[test]
    fn extracts_outer_inner_and_conditional_suppressions() {
        let source = r#"#![allow(dead_code)]
#[allow(clippy::panic, clippy::unwrap_used)]
fn example() {}
#[cfg_attr(test, allow(unused_variables))]
fn conditional(value: bool) { let _ = value; }
"#;
        let facts = lint_suppressions("src/lib.rs", source).unwrap();
        assert_eq!(facts.len(), 3);
        assert_eq!(facts[0].kind, "allow");
        assert_eq!(
            facts[1].lint_paths,
            ["clippy::panic", "clippy::unwrap_used"]
        );
        assert_eq!(facts[2].kind, "cfg-allow");
        assert_eq!(facts[2].lint_paths, ["unused_variables"]);
    }

    #[test]
    fn extracts_all_unsafe_surface_forms() {
        let source = r#"unsafe fn function() {}
unsafe trait Trait {}
struct Type;
unsafe impl Trait for Type {}
unsafe extern "C" { fn external(); }
fn block() { unsafe { let _value = 1; } }
"#;
        let facts = super::unsafe_surface("src/lib.rs", source).unwrap();
        assert_eq!(
            facts
                .iter()
                .map(|fact| fact.pattern_id.as_str())
                .collect::<Vec<_>>(),
            [
                "unsafe-function",
                "unsafe-trait",
                "unsafe-impl",
                "unsafe-extern-block",
                "unsafe-block"
            ]
        );
    }

    #[test]
    fn extracts_normalized_direct_dependency_edges() {
        let source = r#"[dependencies]
serde = "1"
serde_json = { package = "serde_json", version = "1", features = ["std", "derive"], default-features = false }
workspace_dep = { workspace = true }

[dev-dependencies]
ignored = "1"

[target.'cfg(unix)'.build-dependencies]
cc = { git = "https://example.invalid/cc" }
"#;
        let edges = dependency_edges(source).unwrap();
        assert_eq!(edges.len(), 4);
        let serde_json = edges
            .iter()
            .find(|edge| edge.dependency_key == "serde_json")
            .unwrap();
        assert_eq!(serde_json.features, ["derive", "std"]);
        assert!(!serde_json.default_features);
        let cc = edges
            .iter()
            .find(|edge| edge.dependency_key == "cc")
            .unwrap();
        assert_eq!(cc.source_kind, "git");
        assert_eq!(cc.table_path, "target.cfg(unix).build-dependencies");
        assert_eq!(cc.line, 10);
        let workspace = edges
            .iter()
            .find(|edge| edge.dependency_key == "workspace_dep")
            .unwrap();
        assert_eq!(workspace.source_kind, "unspecified");
    }

    #[test]
    fn rejects_malformed_cargo_manifest() {
        assert!(dependency_edges("[dependencies\nserde = \"1\"\n").is_err());
    }

    #[test]
    fn rejects_structurally_invalid_dependency_tables() {
        assert!(dependency_edges("[dependencies]\nserde = true\n").is_err());
        assert!(dependency_edges("dependencies = \"invalid\"\n").is_err());
        assert!(dependency_edges("target = \"invalid\"\n").is_err());
        assert!(dependency_edges("[target]\nwasm = \"invalid\"\n").is_err());
    }

    #[test]
    fn finds_dependency_lines_with_optional_toml_whitespace() {
        let source = "[dependencies]\nserde=\"1\"\n\"serde_json\" = { version = \"1\" }\n";
        let edges = dependency_edges(source).unwrap();
        assert_eq!(
            edges
                .iter()
                .map(|edge| (edge.dependency_key.as_str(), edge.line))
                .collect::<Vec<_>>(),
            [("serde", 2), ("serde_json", 3)]
        );
    }
}
