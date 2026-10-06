// Rust guideline compliant 2026-10-06
//! Python named-function extraction and deterministic structural metrics.

use tree_sitter::{Node, Parser};

use crate::error::{Error, Result};
use crate::path::is_relative_path;

use super::{AnalyzedFile, FunctionIdentity, FunctionKind, FunctionRecord, FunctionRole};

const LANGUAGE: &str = "python";
const SHINGLE_SIZE: usize = 5;

/// Analyzes named Python functions and methods using the bundled Tree-sitter grammar.
///
/// Nested named functions are included as separate records. Lambdas are excluded.
/// Syntax errors are rejected because partial parser recovery cannot support a
/// trustworthy repository gate.
///
/// # Arguments
///
/// * `path` - Repository-relative path using `/` separators.
/// * `source` - Complete UTF-8 Python source text.
///
/// # Returns
///
/// Returns deterministic file and named-function facts.
///
/// # Errors
///
/// Returns an error for an invalid path, unavailable parser, or source syntax error.
pub fn analyze_python_file(path: &str, source: &str) -> Result<AnalyzedFile> {
    if !is_relative_path(path) {
        return Err(Error::invalid(
            "repository-relative path",
            format!("{path:?}"),
        ));
    }
    let mut parser = Parser::new();
    let language = tree_sitter_python::LANGUAGE.into();
    parser
        .set_language(&language)
        .map_err(|error| Error::invalid("Python parser", error))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| Error::invalid("Python parser", "did not produce a syntax tree"))?;
    if tree.root_node().has_error() {
        let error = first_error(tree.root_node());
        return Err(Error::invalid(
            "Python source",
            format!(
                "contains syntax errors near {}:{} ({})",
                error.start_position().row + 1,
                error.start_position().column + 1,
                error.kind()
            ),
        ));
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

fn first_error(node: Node<'_>) -> Node<'_> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.is_error() || child.is_missing() {
            return child;
        }
        if child.has_error() {
            return first_error(child);
        }
    }
    node
}

fn collect_functions(
    node: Node<'_>,
    source: &str,
    path: &str,
    scopes: &[String],
    output: &mut Vec<FunctionRecord>,
) -> Result<()> {
    if node.kind() == "function_definition" {
        let name_node = node
            .child_by_field_name("name")
            .ok_or_else(|| Error::invalid("Python function", "missing declared name"))?;
        let name = node_text(name_node, source)?.to_string();
        let mut qualified = scopes.to_vec();
        qualified.push(name.clone());
        output.push(make_record(node, source, path, &name, &qualified)?);
        let mut nested_scopes = scopes.to_vec();
        nested_scopes.push(name);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if matches!(
                child.kind(),
                "function_definition" | "class_definition" | "block"
            ) {
                collect_functions(child, source, path, &nested_scopes, output)?;
            }
        }
        return Ok(());
    }
    let mut next_scopes = scopes.to_vec();
    if node.kind() == "class_definition"
        && let Some(name) = node.child_by_field_name("name")
    {
        next_scopes.push(node_text(name, source)?.to_string());
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_functions(child, source, path, &next_scopes, output)?;
    }
    Ok(())
}

#[derive(Clone)]
struct Item {
    text: String,
    line: usize,
}

fn make_record(
    node: Node<'_>,
    source: &str,
    path: &str,
    name: &str,
    scopes: &[String],
) -> Result<FunctionRecord> {
    let mut tokens = Vec::new();
    normalized(node, source, true, false, &mut tokens)?;
    let mut ast = Vec::new();
    normalized(node, source, true, true, &mut ast)?;
    let cc = complexity(node, true);
    let sloc = physical_sloc(node, source);
    let token_strings = tokens
        .iter()
        .map(|item| item.text.as_str())
        .collect::<Vec<_>>();
    let ast_strings = ast
        .iter()
        .map(|item| item.text.as_str())
        .collect::<Vec<_>>();
    let token_sequence = shingles(&token_strings);
    let ast_sequence = shingles(&ast_strings);
    let mut token_unique = token_sequence.clone();
    token_unique.sort_unstable();
    token_unique.dedup();
    let mut ast_unique = ast_sequence.clone();
    ast_unique.sort_unstable();
    ast_unique.dedup();
    let role = if name.starts_with("test_")
        || path.split('/').any(|part| {
            part == "tests"
                || part == "test"
                || part.starts_with("test_")
                || part.ends_with("_test.py")
        }) {
        FunctionRole::Test
    } else {
        FunctionRole::General
    };
    Ok(FunctionRecord {
        identity: FunctionIdentity {
            language: LANGUAGE.to_string(),
            path: path.to_string(),
            qualified_name: scopes.join("."),
            kind: FunctionKind::Function,
        },
        name: name.to_string(),
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        normalized_hash: hash_tokens(&token_strings),
        ast_hash: hash_tokens(&ast_strings),
        ast_node_count: ast
            .iter()
            .filter(|item| !item.text.starts_with("/N:"))
            .count(),
        token_count: tokens.len(),
        sloc,
        cc,
        mass: f64::from(cc) * (sloc as f64).sqrt(),
        shingle_hashes: token_unique,
        ast_shingle_hashes: ast_unique,
        token_shingle_sequence: token_sequence,
        ast_shingle_sequence: ast_sequence,
        token_line_runs: line_runs(&tokens),
        ast_line_runs: line_runs(&ast),
        role,
    })
}

fn normalized(
    node: Node<'_>,
    source: &str,
    root: bool,
    ast: bool,
    output: &mut Vec<Item>,
) -> Result<()> {
    if (!root
        && (matches!(node.kind(), "function_definition" | "lambda")
            || (node.kind() == "decorated_definition" && has_function_definition(node))))
        || node.kind() == "comment"
    {
        return Ok(());
    }
    if node.child_count() == 0 {
        let text = node_text(node, source)?;
        let value = if node.kind() == "identifier" {
            "ID"
        } else if matches!(
            node.kind(),
            "integer" | "float" | "string" | "true" | "false" | "none"
        ) {
            "LIT"
        } else {
            text
        };
        output.push(Item {
            text: if ast {
                format!("L:{value}")
            } else {
                value.to_string()
            },
            line: node.start_position().row + 1,
        });
        return Ok(());
    }
    if ast {
        output.push(Item {
            text: format!("N:{}", node.kind()),
            line: node.start_position().row + 1,
        });
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        normalized(child, source, false, ast, output)?;
    }
    if ast {
        output.push(Item {
            text: format!("/N:{}", node.kind()),
            line: node.end_position().row + 1,
        });
    }
    Ok(())
}

fn complexity(node: Node<'_>, root: bool) -> u32 {
    if !root
        && (node.kind() == "function_definition"
            || (node.kind() == "decorated_definition" && has_function_definition(node)))
    {
        return 0;
    }
    let mut value = u32::from(matches!(
        node.kind(),
        "if_statement"
            | "elif_clause"
            | "for_statement"
            | "while_statement"
            | "except_clause"
            | "conditional_expression"
            | "and"
            | "or"
    ));
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        value += complexity(child, false);
    }
    if root { value + 1 } else { value }
}

fn shingles(tokens: &[&str]) -> Vec<u32> {
    if tokens.len() < SHINGLE_SIZE {
        return Vec::new();
    }
    tokens
        .windows(SHINGLE_SIZE)
        .map(|window| {
            let hash = blake3::hash(window.join("\u{1f}").as_bytes());
            u32::from_le_bytes(hash.as_bytes()[..4].try_into().unwrap_or([0; 4]))
        })
        .collect()
}

fn hash_tokens(tokens: &[&str]) -> String {
    blake3::hash(tokens.join("\u{1f}").as_bytes())
        .to_hex()
        .to_string()
}

fn line_runs(items: &[Item]) -> Vec<u32> {
    let count = items.len().saturating_sub(SHINGLE_SIZE - 1);
    let mut runs: Vec<u32> = Vec::new();
    for line in items.iter().take(count).map(|item| item.line as u32) {
        if runs.len() >= 2 && runs[runs.len() - 1] == line {
            let index = runs.len() - 2;
            runs[index] += 1;
        } else {
            runs.extend_from_slice(&[1, line]);
        }
    }
    runs
}

fn physical_sloc(node: Node<'_>, source: &str) -> usize {
    let mut excluded = Vec::new();
    collect_excluded_ranges(node, true, &mut excluded);
    let bytes = source.as_bytes();
    let mut count = 0;
    let mut line_start = node.start_byte();
    let end = node.end_byte();
    while line_start < end {
        let line_end = bytes[line_start..end]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(end, |offset| line_start + offset);
        if bytes[line_start..line_end]
            .iter()
            .enumerate()
            .any(|(offset, byte)| {
                !byte.is_ascii_whitespace()
                    && !excluded.iter().any(|(start, end)| {
                        let position = line_start + offset;
                        *start <= position && position < *end
                    })
            })
        {
            count += 1;
        }
        line_start = line_end.saturating_add(1);
    }
    count
}

fn collect_excluded_ranges(node: Node<'_>, root: bool, ranges: &mut Vec<(usize, usize)>) {
    if node.kind() == "comment" {
        ranges.push((node.start_byte(), node.end_byte()));
        return;
    }
    if !root
        && (node.kind() == "function_definition"
            || (node.kind() == "decorated_definition" && has_function_definition(node)))
    {
        ranges.push((node.start_byte(), node.end_byte()));
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_excluded_ranges(child, false, ranges);
    }
}

fn has_function_definition(node: Node<'_>) -> bool {
    if node.kind() == "function_definition" {
        return true;
    }
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| has_function_definition(child))
}

fn node_text<'a>(node: Node<'_>, source: &'a str) -> Result<&'a str> {
    node.utf8_text(source.as_bytes())
        .map_err(|error| Error::Utf8 {
            subject: "Tree-sitter node range",
            detail: error.to_string(),
        })
}
