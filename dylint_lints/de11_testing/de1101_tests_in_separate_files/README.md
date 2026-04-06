# DE1101: Tests Must Be In Separate Files

## What it does

This lint forbids inline test code inside production Rust files across the repository.

It reports:

- inline `#[test]` / `#[tokio::test]`
- inline `#[cfg(test)] mod tests { ... }`
- other inline test-only items kept directly in production source files
- test module names that don't match `{source_stem}_tests` (e.g. `mod tests;` in `handler.rs` instead of `mod handler_tests;`)
- `#[path = "..."]` values that don't match `{source_stem}_tests.rs`

It allows:

- integration tests under `tests/`
- dedicated unit-test files named `{source_stem}_tests.rs`
- out-of-line test modules with correct naming (`mod {source_stem}_tests;`)
- `#[path = "{source_stem}_tests.rs"]` when the path value matches the source file

## Why

Keeping tests in separate files makes it easier to:

- filter test files out when counting lines of code
- navigate the codebase for both humans and LLMs because files stay smaller
- keep production logic and test code separated by file type

Test files should never be the place where production logic lives.

## Relation to Rust Book Guidance

The Rust Book recommends the conventional unit-test layout of keeping `#[cfg(test)] mod tests { ... }`
inline in the same `src` file as the production code being tested:

- https://doc.rust-lang.org/book/ch11-03-test-organization.html

That recommendation is valid and idiomatic Rust, especially for small libraries and for directly
testing private functions.

This lint intentionally adopts a stricter repository-level policy:

- inline unit tests in production files are forbidden
- test code must live in `tests/` or `{source_stem}_tests.rs`
- out-of-line test modules must be named `{source_stem}_tests` to match the source file
- if `#[path = "..."]` is used, it must reference `{source_stem}_tests.rs`

This is a conscious trade-off, not a claim that the Rust Book is wrong. The cost is that the codebase
deviates from the most common Rust unit-test convention and adds a small amount of file indirection.
The benefit is stronger separation between production code and test code, easier LOC filtering, smaller
production files, and simpler navigation for both humans and LLMs.

More concretely, keeping tests and production logic in different files improves several engineering
workflows:

- Coverage analysis is easier to interpret because production code and test code are not mixed in the
  same file. This makes it simpler to reason about what percentage of the real implementation is
  exercised, instead of visually or mechanically filtering test-only sections from production files.
- Linters and static analysis tools work more cleanly when they analyze production files that contain
  production logic only. Mixed files increase noise, make intent less obvious, and can complicate
  rules that are meant to reason about architecture, layering, complexity, ownership, or API shape.
- Code review is clearer because reviewers can inspect production changes separately from test changes.
  When both live in one file, the review diff mixes behavioral changes, assertions, helpers, mocks,
  and fixtures into one stream, which makes the review more mentally expensive.
- Automated review systems, repository analyzers, and LLM-based tooling generally perform better when
  a file has a single dominant purpose. Separating tests from implementation reduces ambiguity,
  improves summarization quality, and makes it easier for tools to classify code correctly.
- LLM context usage is lower when production files do not also embed large test modules. Loading a
  production file into context brings in less non-essential material, which improves focus, reduces
  token usage, and makes subsequent analysis or code generation more precise.
- Metrics and repository reporting become more trustworthy. File size, churn, ownership, complexity,
  and architectural scans are easier to interpret when test scaffolding does not inflate production
  modules.
- Navigation is faster because engineers can open a production file and read only the implementation,
  while test scenarios, fixtures, and assertions live in dedicated test files with clear intent.

For this repository, those benefits outweigh the downsides, so DE1101 enforces the stricter rule by
design.

## Scope

This lint applies to production Rust files across the repository.

## Examples

### Bad

```rust
pub fn normalize_name(name: &str) -> String {
    name.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_and_lowercases() {
        assert_eq!(normalize_name(" Admin "), "admin");
    }
}
```

### Good

```rust
// normalize_name.rs
pub fn normalize_name(name: &str) -> String {
    name.trim().to_lowercase()
}

#[cfg(test)]
mod normalize_name_tests;
```

```rust
// normalize_name_tests.rs
use super::*;

#[test]
fn trims_and_lowercases() {
    assert_eq!(normalize_name(" Admin "), "admin");
}
```

### Also bad — `#[path]` value does not match source file

```rust
// handler.rs
#[cfg(test)]
#[path = "other_tests.rs"]  // should be "handler_tests.rs"
mod tests;
```

### Also bad — module name does not match source file

```rust
// handler.rs
#[cfg(test)]
mod tests; // should be `mod handler_tests;`
```

## Configuration

This lint is configured to `warn` by default.

Allowed test locations are:

- files under `tests/`
- files named `{source_stem}_tests.rs` (matching the production file name)

Additional constraints:

- `#[path = "..."]` is forbidden on `#[cfg(test)]` modules
- test module name must exactly match `{source_stem}_tests`

## Intent

This rule is intentionally strict for the repository: production files should contain production code, and test files should contain tests only.
