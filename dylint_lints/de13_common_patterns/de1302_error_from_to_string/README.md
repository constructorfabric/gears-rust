Created: 2026-03-18 by Constructor Tech
Updated: 2026-03-18 by Constructor Tech

# DE1302: No `.to_string()` in Error From Impls

## What it does

Detects `.to_string()` calls inside `fn from()` bodies within `impl From<X> for Y` blocks where X or Y implements `std::error::Error`. Such conversions silently destroy the error chain.

## Why is this bad?

When you call `e.to_string()` inside a `From` impl, you convert the original error to a string and discard it. The resulting error:

- Has no `.source()` (error chain is broken)
- Cannot be matched or downcast by callers
- Loses structured metadata (error codes, fields, etc.)

Tools like `anyhow`, `thiserror`'s `#[from]`, or storing the error directly preserve the chain without extra effort.

## Example

### Bad

```rust
impl From<DatabaseError> for AppError {
    fn from(e: DatabaseError) -> Self {
        AppError::Internal(e.to_string())  // chain lost!
    }
}
```

### Good

```rust
#[derive(thiserror::Error, Debug)]
enum AppError {
    #[error(transparent)]
    Database(#[from] DatabaseError),
}
```

```rust
impl From<DatabaseError> for AppError {
    fn from(e: DatabaseError) -> Self {
        AppError::Database(e)  // store source; chain preserved
    }
}
```

## Configuration

This lint is configured to **deny** by default.

It only flags when the source or target type implements `std::error::Error`, avoiding false positives from name-based heuristics.

## See Also

- [thiserror](https://crates.io/crates/thiserror)
- [anyhow](https://crates.io/crates/anyhow)
- [Error handling in Rust](https://doc.rust-lang.org/book/ch09-00-error-handling.html)
