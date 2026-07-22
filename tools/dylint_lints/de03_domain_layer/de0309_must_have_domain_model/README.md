# DE0309: Must Have Domain Model Attribute

## What it does

Checks that struct and enum types in domain modules that are visible beyond their own
module (`pub`, `pub(crate)`, `pub(super)`, `pub(in ...)`) have the `#[domain_model]`
attribute. Strictly module-private types (no `pub` keyword) are exempt.

## Why is this important?

The `#[domain_model]` macro provides **compile-time validation** of Domain-Driven Design (DDD) boundaries. It ensures that domain types don't contain infrastructure dependencies such as:

- HTTP types (`http::StatusCode`, `axum::*`)
- Database types (`sqlx::PgPool`, `sea_orm::*`)
- File system types (`std::fs::*`, `tokio::fs::*`)
- External service clients (`reqwest::*`, `tonic::*`)

By requiring this attribute on all externally-visible domain types, we guarantee that infrastructure concerns cannot leak into the domain layer. (Strictly module-private types are exempt from this lint — their fields are still guarded by `DE0301`/`DE0308`.)

## Example

### Bad

```rust
// src/domain/user.rs

pub struct User {           // Missing #[domain_model]
    pub id: Uuid,
    pub email: String,
}
```

### Good

```rust
// src/domain/user.rs
use toolkit_macros::domain_model;

#[domain_model]
pub struct User {
    pub id: Uuid,
    pub email: String,
}
```

## Configuration

This lint is configured to **deny** by default.

It checks `struct` and `enum` definitions in files whose path contains `/domain/`,
**except** strictly module-private ones (no `pub` keyword). Private types are pure
implementation details that never cross a layer boundary, and their fields are still
guarded against infrastructure leakage by `DE0301_NO_INFRA_IN_DOMAIN` and
`DE0308_NO_HTTP_IN_DOMAIN`, which check every domain `struct`/`enum` regardless of this
attribute. This keeps small technical helpers (e.g. a `HashMap`-key newtype) from
needing either a spurious `#[domain_model]` or an `#[allow(...)]`.

## TDD Approach

This lint is designed for Test-Driven Development:

1. **Add the lint** - CI will fail for all externally-visible domain types without the attribute
2. **Fix each violation** - Add `#[domain_model]` to all externally-visible domain types
3. **CI passes** - All externally-visible domain types are now validated at compile time

## See Also

- [`#[domain_model]` macro documentation](../../../../libs/toolkit-macros/src/domain_model.rs)
- [Domain Layer Architecture](../../../../docs/toolkit_unified_system/02_gear_layout_and_sdk_pattern.md)
