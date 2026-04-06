# DE1101 UI Test Guide

## Structure

Each UI test = 3 files:
1. `ui/<name>.rs` — Rust source (the test input)
2. `ui/<name>.stderr` — expected compiler output (empty if no errors)
3. `Cargo.toml` — must have `[[example]]` entry for the test

## Writing a test file (`ui/<name>.rs`)

```rust
// simulated_dir=/workspace/modules/system/resource-group/resource-group/src/api/rest/
#[cfg(test)]
// Should trigger DE1101 - tests must be in separate files
mod tests;

fn main() {}
```

Rules:
- First line: `// simulated_dir=...` sets the simulated module path
- Every triggering line MUST have a comment **on the line above**: `// Should trigger DE1101 - tests must be in separate files`
- Every non-triggering line MUST have: `// Should not trigger DE1101 - tests must be in separate files`
- Always end with `fn main() {}`

## What the lint checks

| Violation | Example | Error? |
|---|---|---|
| Inline test code | `#[cfg(test)] mod tests { ... }` | Yes |
| Wrong module name | `mod tests;` in `handler.rs` (should be `mod handler_tests;`) | Yes |
| Wrong `#[path]` value | `#[path = "foo.rs"]` in `handler.rs` (should be `handler_tests.rs`) | Yes |
| Correct module name | `mod handler_tests;` in `handler.rs` | No |
| Correct `#[path]` value | `#[path = "handler_tests.rs"]` in `handler.rs` | No |

**Key rule**: for `<name>.rs`, the test file must be `<name>_tests.rs` — either via `mod <name>_tests;` or `#[path = "<name>_tests.rs"]`.

## Creating the `.stderr` file

1. Create `.rs` file and empty `.stderr`
2. Add `[[example]]` to `Cargo.toml`
3. Run `cargo test tests::ui_examples`
4. Copy "normalized stderr" from the failure output into `.stderr`
5. For no-error cases, `.stderr` must be a **zero-byte** file (not even a newline)

## Cargo.toml entry

```toml
[[example]]
name = "<name>"
path = "ui/<name>.rs"
```

## Extracting inline tests automatically

Use `extract_tests.py` to batch-extract inline `#[cfg(test)] mod tests { ... }` blocks into separate `*_tests.rs` files:

```sh
python3 extract_tests.py <directory>
```

The script recursively processes all `.rs` files in `<directory>`:
- Extracts inline test module body into `<stem>_tests.rs`
- Replaces the inline block with `#[cfg(test)] #[path = "<stem>_tests.rs"] mod <stem>_tests;`
- Preserves `#[cfg_attr(coverage_nightly, coverage(off))]` if present
- Reports `#[cfg(test)]` on individual items (structs, impls) that need manual attention

## Running tests

```sh
cargo test                      # all tests
cargo test tests::ui_examples   # only UI tests
```
