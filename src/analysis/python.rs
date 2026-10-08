// Rust guideline compliant 2026-10-06
//! Python named-function extraction and deterministic structural metrics.

use tree_sitter::{Node, Parser};

use crate::error::{Error, Result};
use crate::path::is_relative_path;

use super::{
    AnalyzedFile, FunctionIdentity, FunctionKind, FunctionRecord, FunctionRole,
    extract::{first_error_node, token_shingle_hashes},
};

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
        let error = first_error_node(tree.root_node());
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

fn collect_functions(
    node: Node<'_>,
    source: &str,
    path: &str,
    scopes: &[String],
    output: &mut Vec<FunctionRecord>,
) -> Result<()> {
    if node.kind() == "function_definition" {
        if is_overload_stub(node, source) {
            return Ok(());
        }
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

fn is_overload_stub(node: Node<'_>, source: &str) -> bool {
    let Some(parent) = node
        .parent()
        .filter(|parent| parent.kind() == "decorated_definition")
    else {
        return false;
    };
    let mut cursor = parent.walk();
    parent
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "decorator")
        .filter_map(|decorator| source.get(decorator.byte_range()))
        .any(|decorator| {
            decorator
                .trim()
                .strip_prefix('@')
                .and_then(|name| name.split('(').next())
                .and_then(|name| name.rsplit('.').next())
                == Some("overload")
        })
}

fn descriptor_kind(node: Node<'_>, source: &str) -> Option<&'static str> {
    let parent = node
        .parent()
        .filter(|parent| parent.kind() == "decorated_definition")?;
    let mut cursor = parent.walk();
    parent
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "decorator")
        .filter_map(|decorator| source.get(decorator.byte_range()))
        .find_map(|decorator| {
            let name = decorator
                .trim()
                .strip_prefix('@')?
                .split('(')
                .next()?
                .rsplit('.')
                .next()?;
            match name {
                "property" => Some("property"),
                "getter" => Some("getter"),
                "setter" => Some("setter"),
                "deleter" => Some("deleter"),
                _ => None,
            }
        })
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
    let cc = complexity(node, true, source);
    let sloc = physical_sloc(node, source);
    let token_strings = tokens
        .iter()
        .map(|item| item.text.as_str())
        .collect::<Vec<_>>();
    let ast_strings = ast
        .iter()
        .map(|item| item.text.as_str())
        .collect::<Vec<_>>();
    let token_sequence = token_shingle_hashes(&token_strings);
    let ast_sequence = token_shingle_hashes(&ast_strings);
    let mut token_unique = token_sequence.clone();
    token_unique.sort_unstable();
    token_unique.dedup();
    let mut ast_unique = ast_sequence.clone();
    ast_unique.sort_unstable();
    ast_unique.dedup();
    let descriptor_kind = descriptor_kind(node, source);
    let role = if name.starts_with("test_")
        || path.split('/').any(|part| {
            part == "tests"
                || part == "test"
                || part.starts_with("test_")
                || part.ends_with("_test.py")
        }) {
        FunctionRole::Test
    } else if descriptor_kind.is_some() && cc == 1 {
        FunctionRole::Accessor
    } else {
        FunctionRole::General
    };
    let mut qualified = scopes.to_vec();
    if let Some(last) = qualified.last_mut()
        && let Some(kind) = descriptor_kind
    {
        *last = format!("{name}@{kind}");
    }
    Ok(FunctionRecord {
        identity: FunctionIdentity {
            language: LANGUAGE.to_string(),
            path: path.to_string(),
            qualified_name: qualified.join("."),
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
        && (node.kind() == "function_definition"
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

fn complexity(node: Node<'_>, root: bool, source: &str) -> u32 {
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
            | "for_in_clause"
            | "if_clause"
            | "and"
            | "or"
    ));
    if node.kind() == "case_clause" && !is_wildcard_case(node, source) {
        value += 1;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        value += complexity(child, false, source);
    }
    if root { value + 1 } else { value }
}

fn is_wildcard_case(node: Node<'_>, source: &str) -> bool {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| child.kind() == "case_pattern")
        .and_then(|pattern| source.get(pattern.byte_range()))
        .is_some_and(|pattern| pattern.trim() == "_")
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

#[cfg(test)]
mod tests {
    use super::analyze_python_file;

    #[test]
    fn extracts_fixture_scopes_async_functions_and_metrics() {
        let source = include_str!("../../tests/fixtures/python_analysis.py");
        let file = analyze_python_file("tests/fixtures/python_analysis.py", source).unwrap();
        assert_eq!(file.language, "python");
        assert_eq!(
            file.functions
                .iter()
                .map(|function| function.identity.qualified_name.as_str())
                .collect::<Vec<_>>(),
            [
                "Worker.run",
                "Worker.run.nested",
                "test_plain",
                "decorated",
                "match_value",
                "comprehension",
                "parse_value",
                "NumberBox.value@property",
                "NumberBox.value@setter"
            ]
        );
        let method = &file.functions[0];
        assert_eq!(method.name, "run");
        assert_eq!(method.start_line, 2);
        assert_eq!(method.cc, 4);
        assert_eq!(method.sloc, 8);
        assert_eq!(method.mass, 4.0 * 8.0_f64.sqrt());
        assert!(method.ast_node_count > 0);
        assert!(method.token_count > 0);
        assert!(!method.shingle_hashes.is_empty());
        assert_eq!(file.functions[1].sloc, 2);
        assert_eq!(file.functions[2].role.name(), "test");
        assert_eq!(file.functions[3].start_line, 21);
        assert_eq!(file.functions[4].cc, 2);
        assert_eq!(file.functions[5].cc, 3);
        assert_eq!(
            file.functions
                .iter()
                .filter(|function| function.name == "parse_value")
                .count(),
            1
        );
    }

    #[test]
    fn property_getter_and_setter_have_distinct_identities() {
        let source = "class NumberBox:\n    @property\n    def value(self):\n        return self._value\n\n    @value.setter\n    def value(self, value):\n        self._value = value\n";
        let file = analyze_python_file("src/model.py", source).unwrap();

        assert_eq!(
            file.functions
                .iter()
                .map(|function| function.identity.qualified_name.as_str())
                .collect::<Vec<_>>(),
            ["NumberBox.value@property", "NumberBox.value@setter"]
        );
        assert!(
            file.functions
                .iter()
                .all(|function| function.role.name() == "accessor")
        );
    }

    #[test]
    fn ignores_comments_and_literal_values_in_fingerprints() {
        let first =
            analyze_python_file("src/a.py", "def f(value):\n    return value + 1\n").unwrap();
        let second = analyze_python_file(
            "src/a.py",
            "def f(other): # a trailing comment\n    return other + 999\n",
        )
        .unwrap();
        assert_eq!(
            first.functions[0].normalized_hash,
            second.functions[0].normalized_hash
        );
        assert_eq!(first.functions[0].ast_hash, second.functions[0].ast_hash);
    }

    #[test]
    fn lambda_conditional_contributes_to_parent_metrics() {
        let plain =
            analyze_python_file("src/a.py", "def map_value(value):\n    return value\n").unwrap();
        let with_lambda = analyze_python_file(
            "src/a.py",
            "def map_value(value):\n    mapper = lambda item: 1 if item else 0\n    return mapper(value)\n",
        )
        .unwrap();
        let plain = &plain.functions[0];
        let with_lambda = &with_lambda.functions[0];
        assert_eq!(with_lambda.cc, plain.cc + 1);
        assert_eq!(with_lambda.sloc, plain.sloc + 1);
        assert_ne!(with_lambda.ast_hash, plain.ast_hash);
        assert_ne!(with_lambda.normalized_hash, plain.normalized_hash);
        assert!(with_lambda.token_count > plain.token_count);
    }

    #[test]
    fn rejects_syntax_errors_and_invalid_paths() {
        assert!(analyze_python_file("broken.py", "def broken(:\n").is_err());
        assert!(analyze_python_file("/tmp/broken.py", "def valid(): pass\n").is_err());
    }

    #[test]
    fn repeated_analysis_is_deterministic() {
        let source = include_str!("../../tests/fixtures/python_analysis.py");
        let first = analyze_python_file("pkg/module.py", source).unwrap();
        let second = analyze_python_file("pkg/module.py", source).unwrap();
        assert_eq!(first, second);
    }
}
