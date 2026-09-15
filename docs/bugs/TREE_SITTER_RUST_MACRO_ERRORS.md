# tree-sitter-rust reports errors for valid macro-heavy Rust

## Summary

`tree-sitter-rust` 0.24 reports `ERROR` nodes for valid Rust in macro-heavy
source files. Slop Gate uses `root_node().has_error()` to detect malformed
source, so these recovery nodes incorrectly caused complete repository
revisions to be rejected.

This report contains two independent reproductions observed in maintained
public repositories. The first is a macro matcher using a tilde token. The
second is a file containing a valid `pin_project!` invocation.

## Environment

- `tree-sitter`: 0.27
- `tree-sitter-rust`: 0.24
- macOS on Apple Silicon
- Parser API: `Parser::parse(source, None)` followed by
  `tree.root_node().has_error()`

## Reproduction 1: macro matcher

Parse this complete source file:

```rust
macro_rules! parser {
    (0 (~$($fuel:tt)*) $rest:tt) => { $rest };
}

fn stable() {}
```

Expected: no `ERROR` node. The `~` is a valid literal token in a
`macro_rules!` matcher, and the `$($fuel:tt)*` fragment is a valid repetition.

Observed: an `ERROR` node is reported at the macro matcher. In the real
`anyhow` source, the corresponding location is `src/ensure.rs:134`.

## Reproduction 2: macro invocation

The Hyper repository at commit
`c6dca2078ce223050dc0832be7c9ab07baa6c4bf` contains the valid Rust file
`tests/support/tokiort.rs`. It uses `pin_project_lite::pin_project` and parses
successfully under Rust tooling and `rustfmt`, but tree-sitter-rust reports an
`ERROR` near `1:1` when the file is parsed as part of the repository.

The file is available at:

<https://github.com/hyperium/hyper/blob/c6dca2078ce223050dc0832be7c9ab07baa6c4bf/tests/support/tokiort.rs>

The error location appears to be a cascading recovery location. Parsing the
file independently can succeed after changing its path and surrounding
context, so the minimal reproducer for this case should be reduced from the
linked file during triage.

## Why this matters

`ERROR` nodes are useful when the grammar cannot represent input, but they do
not prove that Rust source is invalid. Macro token trees intentionally contain
syntax that is interpreted by a later macro expansion. Consumers need a way to
distinguish unsupported macro grammar from malformed ordinary Rust.

Please either accept these forms in the grammar or document a reliable node
classification that lets consumers identify errors inside macro definitions
and invocations.

## Temporary consumer workaround

Slop Gate now retains strict errors for ordinary Rust and tolerates parser
errors associated with macro-heavy files. This is a compatibility workaround,
not a claim that every error in such a file is harmless.
