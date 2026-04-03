# DE1101: Resource-group Tests Must Be In Separate Files

## What it does

This lint forbids inline test code inside production Rust files of `cf-resource-group`.

It reports:

- inline `#[test]` / `#[tokio::test]`
- inline `#[cfg(test)] mod tests { ... }`
- other inline test-only items kept directly in production source files

It allows:

- integration tests under `tests/`
- dedicated unit-test files such as `*_test.rs` and `*_tests.rs`
- out-of-line test modules declared from production files

## Why

Keeping tests in separate files makes it easier to:

- filter test files out when counting lines of code
- navigate the codebase for both humans and LLMs because files stay smaller
- keep production logic and test code separated by file type

Test files should never be the place where production logic lives.

## Scope

This lint applies only to:

- `modules/system/resource-group/resource-group/`

It does not enforce this rule for other modules or crates.

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
pub fn normalize_name(name: &str) -> String {
    name.trim().to_lowercase()
}

#[cfg(test)]
#[path = "normalize_name_tests.rs"]
mod tests;
```

```rust
// normalize_name_tests.rs
use super::*;

#[test]
fn trims_and_lowercases() {
    assert_eq!(normalize_name(" Admin "), "admin");
}
```

## Configuration

This lint is configured to `deny` by default.

Allowed test locations are:

- files under `tests/`
- files named `*_test.rs`
- files named `*_tests.rs`

## Intent

This rule is intentionally strict for `resource-group`: production files should contain production code, and test files should contain tests only.
