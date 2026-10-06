// Rust guideline compliant 2026-10-06
//! TypeScript function extraction and deterministic structural metrics.

use std::collections::BTreeSet;

use tree_sitter::{Language, Node, Parser};

use crate::error::{Error, Result};
use crate::path::is_relative_path;

use super::{AnalyzedFile, FunctionIdentity, FunctionKind, FunctionRecord, FunctionRole};

const LANGUAGE_NAME: &str = "typescript";
const SHINGLE_SIZE: usize = 5;

/// Analyzes named TypeScript functions and class methods in one source file.
///
/// Function declarations and method definitions with bodies are recorded.
/// Arrow functions, anonymous function expressions, overload signatures, and
/// declarations without bodies are intentionally excluded. Invalid or
/// incomplete syntax is rejected so callers never receive partial analysis.
///
/// # Arguments
///
/// * `path` - Repository-relative path using `/` separators.
/// * `source` - Complete UTF-8 TypeScript source.
///
/// # Returns
///
/// A deterministic file record with functions ordered by source location.
///
/// # Errors
///
/// Returns an error for an invalid path, parser failure, or syntax errors.
pub fn analyze_typescript_file(path: &str, source: &str) -> Result<AnalyzedFile> {
    if !is_relative_path(path) {
        return Err(Error::invalid(
            "repository-relative path",
            format!("{path:?}"),
        ));
    }
    let mut parser = Parser::new();
    let grammar = if path.ends_with(".tsx") {
        tree_sitter_typescript::LANGUAGE_TSX
    } else {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT
    };
    let language: Language = grammar.into();
    parser
        .set_language(&language)
        .map_err(|error| Error::invalid("TypeScript parser", error))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| Error::invalid("TypeScript parser", "did not produce a syntax tree"))?;
    if tree.root_node().has_error() {
        let node = first_error(tree.root_node());
        return Err(Error::invalid(
            "TypeScript source",
            format!(
                "contains syntax errors near {}:{} ({})",
                node.start_position().row + 1,
                node.start_position().column + 1,
                node.kind()
            ),
        ));
    }

    let mut functions = Vec::new();
    collect_functions(tree.root_node(), source, path, &[], &mut functions)?;
    Ok(AnalyzedFile {
        path: path.to_string(),
        language: LANGUAGE_NAME.to_string(),
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
    result: &mut Vec<FunctionRecord>,
) -> Result<()> {
    let mut child_scopes = scopes.to_vec();
    if matches!(
        node.kind(),
        "class_declaration"
            | "abstract_class_declaration"
            | "interface_declaration"
            | "internal_module"
    ) && let Some(name) = node.child_by_field_name("name")
    {
        child_scopes.push(text(name, source)?.to_string());
    }
    if matches!(node.kind(), "function_declaration" | "method_definition")
        && node.child_by_field_name("body").is_some()
    {
        result.push(record_function(node, source, path, scopes)?);
        let mut nested_scopes = child_scopes;
        if let Some(name) = node.child_by_field_name("name") {
            nested_scopes.push(text(name, source)?.to_string());
        }
        if let Some(body) = node.child_by_field_name("body") {
            collect_functions(body, source, path, &nested_scopes, result)?;
        }
        return Ok(());
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_functions(child, source, path, &child_scopes, result)?;
    }
    Ok(())
}

fn record_function(
    node: Node<'_>,
    source: &str,
    path: &str,
    scopes: &[String],
) -> Result<FunctionRecord> {
    let name_node = node
        .child_by_field_name("name")
        .ok_or_else(|| Error::invalid("TypeScript function", "is missing a name"))?;
    let name = text(name_node, source)?.to_string();
    let mut qualified = scopes.to_vec();
    qualified.push(name.clone());
    let tokens = normalized_tokens(node, source, true)?;
    let ast = normalized_ast(node, source, true)?;
    let token_text = tokens
        .iter()
        .map(|token| token.text.as_str())
        .collect::<Vec<_>>()
        .join("\u{1f}");
    let ast_text = ast
        .iter()
        .map(|token| token.text.as_str())
        .collect::<Vec<_>>()
        .join("\u{1f}");
    let cc = cyclomatic_complexity(node);
    let sloc = physical_sloc(node, source);
    let start_line = node.start_position().row + 1;
    let role = classify_role(&name, path, scopes, cc);
    Ok(FunctionRecord {
        identity: FunctionIdentity {
            language: LANGUAGE_NAME.to_string(),
            path: path.to_string(),
            qualified_name: qualified.join("::"),
            kind: FunctionKind::Function,
        },
        name,
        start_line,
        end_line: node.end_position().row + 1,
        normalized_hash: blake3::hash(token_text.as_bytes()).to_hex().to_string(),
        ast_hash: blake3::hash(ast_text.as_bytes()).to_hex().to_string(),
        ast_node_count: ast
            .iter()
            .filter(|token| !token.text.starts_with("/N:"))
            .count(),
        token_count: tokens.len(),
        sloc,
        cc,
        mass: f64::from(cc) * (sloc as f64).sqrt(),
        shingle_hashes: unique_shingles(&tokens),
        ast_shingle_hashes: unique_shingles(&ast),
        token_shingle_sequence: shingles(&tokens),
        ast_shingle_sequence: shingles(&ast),
        token_line_runs: line_runs(&tokens),
        ast_line_runs: line_runs(&ast),
        role,
    })
}

struct Token {
    text: String,
    line: usize,
}

fn normalized_tokens(node: Node<'_>, source: &str, root: bool) -> Result<Vec<Token>> {
    if omitted(node, root) {
        return Ok(Vec::new());
    }
    if node.child_count() == 0 {
        return Ok(vec![Token {
            text: normalize_leaf(node, source)?,
            line: node.start_position().row + 1,
        }]);
    }
    let mut result = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        result.extend(normalized_tokens(child, source, false)?);
    }
    Ok(result)
}

fn normalized_ast(node: Node<'_>, source: &str, root: bool) -> Result<Vec<Token>> {
    if omitted(node, root) {
        return Ok(Vec::new());
    }
    if node.child_count() == 0 {
        return Ok(vec![Token {
            text: format!("L:{}", normalize_leaf(node, source)?),
            line: node.start_position().row + 1,
        }]);
    }
    let mut result = vec![Token {
        text: format!("N:{}", node.kind()),
        line: node.start_position().row + 1,
    }];
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        result.extend(normalized_ast(child, source, false)?);
    }
    result.push(Token {
        text: format!("/N:{}", node.kind()),
        line: node.end_position().row + 1,
    });
    Ok(result)
}

fn omitted(node: Node<'_>, root: bool) -> bool {
    (!root && matches!(node.kind(), "function_declaration" | "method_definition"))
        || matches!(node.kind(), "comment" | "line_comment" | "block_comment")
}

fn normalize_leaf(node: Node<'_>, source: &str) -> Result<String> {
    let kind = node.kind();
    if kind.contains("identifier") || kind == "this" || kind == "super" {
        return Ok("ID".to_string());
    }
    if kind.contains("number")
        || kind.contains("string")
        || kind.contains("template")
        || matches!(kind, "true" | "false" | "null" | "undefined")
    {
        return Ok("LIT".to_string());
    }
    Ok(text(node, source)?.to_string())
}

fn text<'a>(node: Node<'_>, source: &'a str) -> Result<&'a str> {
    source
        .get(node.byte_range())
        .ok_or_else(|| Error::invalid("TypeScript source range", "is not valid UTF-8"))
}

fn physical_sloc(node: Node<'_>, source: &str) -> usize {
    let mut comments = Vec::new();
    collect_comment_ranges(node, true, &mut comments);
    let start = node.start_byte();
    let end = node.end_byte();
    let bytes = source.as_bytes();
    let mut count = 0;
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
    count
}

fn collect_comment_ranges(node: Node<'_>, is_root: bool, comments: &mut Vec<(usize, usize)>) {
    if !is_root && matches!(node.kind(), "function_declaration" | "method_definition") {
        return;
    }
    if matches!(node.kind(), "comment" | "line_comment" | "block_comment") {
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

fn shingles(tokens: &[Token]) -> Vec<u32> {
    if tokens.len() < SHINGLE_SIZE {
        return Vec::new();
    }
    tokens
        .windows(SHINGLE_SIZE)
        .map(|window| {
            let text = window
                .iter()
                .map(|token| token.text.as_str())
                .collect::<Vec<_>>()
                .join("\u{1f}");
            let hash = blake3::hash(text.as_bytes());
            let mut bytes = [0_u8; 4];
            bytes.copy_from_slice(&hash.as_bytes()[..4]);
            u32::from_le_bytes(bytes)
        })
        .collect()
}

fn unique_shingles(tokens: &[Token]) -> Vec<u32> {
    shingles(tokens)
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn line_runs(tokens: &[Token]) -> Vec<u32> {
    let count = tokens.len().saturating_sub(SHINGLE_SIZE - 1);
    let mut runs: Vec<u32> = Vec::new();
    for line in tokens.iter().take(count).map(|token| token.line as u32) {
        if runs.last() == Some(&line) {
            let index = runs.len() - 2;
            runs[index] += 1;
        } else {
            runs.extend_from_slice(&[1, line]);
        }
    }
    runs
}

fn cyclomatic_complexity(node: Node<'_>) -> u32 {
    let mut cc = 1;
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.id() != node.id()
            && matches!(current.kind(), "function_declaration" | "method_definition")
        {
            continue;
        }
        if matches!(
            current.kind(),
            "if_statement"
                | "for_statement"
                | "for_in_statement"
                | "while_statement"
                | "do_statement"
                | "catch_clause"
                | "ternary_expression"
                | "switch_case"
        ) {
            cc += 1;
        } else if current.kind() == "binary_expression" {
            let mut cursor = current.walk();
            if current
                .children(&mut cursor)
                .any(|child| matches!(child.kind(), "&&" | "||" | "??"))
            {
                cc += 1;
            }
        }
        let mut cursor = current.walk();
        stack.extend(current.named_children(&mut cursor));
    }
    cc
}

fn classify_role(name: &str, path: &str, scopes: &[String], cc: u32) -> FunctionRole {
    if scopes
        .iter()
        .any(|scope| scope == "test" || scope == "tests")
        || path.split('/').any(|segment| {
            segment == "tests" || segment.ends_with(".test.ts") || segment.ends_with(".spec.ts")
        })
        || name.starts_with("test")
    {
        FunctionRole::Test
    } else if cc == 1 && (name == "get" || name.starts_with("get_")) {
        FunctionRole::Accessor
    } else if cc <= 3 && (name == "new" || name.starts_with("create") || name.starts_with("from")) {
        FunctionRole::Constructor
    } else {
        FunctionRole::General
    }
}
