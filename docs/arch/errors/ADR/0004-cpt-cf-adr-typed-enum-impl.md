---
status: accepted
date: 2026-02-28
---

# Use Typed Enum with Category-Typed Constructors for CanonicalError

**ID**: `cpt-cf-errors-adr-typed-enum-impl`

## Context and Problem Statement

The canonical error model requires a single Rust type that all gears use to express failures. This type must carry a category, structured context, and optional metadata. Multiple implementation approaches exist in Rust's type system. Which approach best satisfies compile-time safety, ergonomics, and transport agnosticism?

## Decision Drivers

* Compile-time exhaustiveness — forgetting to handle a category must be a compile error
* Type-safe context — each category must accept only its designated context type
* Ergonomics — single-expression construction, `?` operator compatibility
* Transport agnosticism — the error type must not embed HTTP or gRPC details
* Blanket `From` impls — common library errors must propagate via `?` without per-call-site mapping
* LLM-friendliness — finite, discoverable API surface

## Considered Options

* **Option A**: Typed enum (`CanonicalError`) with one variant per category, each carrying its context type
* **Option B**: Per-error structs implementing an `ErrorSchema` trait
* **Option C**: Trait object (`Box<dyn CanonicalError>`) with a `category()` method

## Decision Outcome

Chosen option: **Option A — Typed enum with category-typed constructors**, because it is the only option that provides both compile-time exhaustiveness and type-safe context in a single mechanism, with native `?` operator support and no heap allocation.

### Consequences

* The error library must define a single `CanonicalError` enum with 16 variants, each carrying its specific context type — the enum is the central error type for the entire platform
* Each category has its own context type (`NotFoundV1`, `InvalidArgumentV1`, `ResourceExhaustedV1`, etc., with unversioned aliases) with its own fields — these types are part of the public API surface
* Blanket `From` implementations for common library errors (`std::io`, `serde_json`, `sea_orm` behind the `sea-orm` feature) must be provided in the error library to enable `?` propagation
* The `Problem` wire format conversion must be a single exhaustive `match` in one location — all 16 categories are handled centrally, not per-gear
* Adding a new category in the future adds a new enum variant, which is a breaking change — `#[non_exhaustive]` or major version bump must be decided
* Gears that need error customisation beyond the 16 categories must do so through context payload specialisation, not through new enum variants
* The `#[resource_error]` attribute macro must generate typed constructor functions that delegate to the enum constructors — the macro is a convenience layer, not a separate error type

### Confirmation

`libs/toolkit-canonical-errors` implements all 16 variants (`src/error.rs`), typed builders (`src/builder.rs`), the exhaustive `match` in `Problem` conversion (`src/problem.rs`) and the blanket `From` impls. `tests/showcase.rs` covers every category.

## Pros and Cons of the Options

### Option A: Typed Enum

A single `enum CanonicalError` with 16 variants. Each variant carries `ctx: ContextType`, `detail: String`, `resource_type: Option<String>`, `resource_name: Option<String>` and `overrides: TransportOverrides` (`Internal` has no resource fields).

* Good, because exhaustive `match` — compiler enforces all categories are handled
* Good, because each variant has a specific context type — wrong context is a compile error
* Good, because one `From<CanonicalError> for Problem` impl covers all gears
* Good, because blanket `From` impls for library errors (io, serde_json, sea_orm) enable `?`
* Good, because stack-allocated — no heap allocation for the error itself
* Neutral, because enum size equals largest variant (acceptable for error types)
* Bad, because new variants require `#[non_exhaustive]` or a major version bump

### Option B: Per-Error Structs with `ErrorSchema` Trait

Each error is a separate struct implementing a trait with `STATUS`, `TITLE`, `SCHEMA_ID` constants.

* Good, because each error is self-contained
* Good, because adding a new error requires no changes to existing code
* Bad, because 15-20 lines of boilerplate per error type (struct + trait impl)
* Bad, because no exhaustive match — impossible to verify all errors are handled
* Bad, because no blanket `From` impls for library errors — each gear manually maps each library error
* Bad, because `STATUS: u16` embeds HTTP details in domain code

### Option C: Trait Object (`Box<dyn CanonicalError>`)

A trait with `fn category(&self) -> Category` and `fn context(&self) -> ContextValue`. Errors are `Box<dyn CanonicalError>`.

* Good, because gears can define their own error structs conforming to the category contract
* Good, because extensible without modifying the trait
* Bad, because `Box<dyn>` heap-allocates every error
* Bad, because no compile-time exhaustiveness — `category()` returns a runtime value
* Bad, because `?` operator requires `From` impls for `Box<dyn CanonicalError>` which are awkward
* Bad, because the trait name collides with `std::error::Error` in scope

## More Information

Library error blanket `From` impls shipped with `cf-gears-toolkit-canonical-errors` (`src/error.rs`):

| Library Type | Maps To | Rationale |
|---|---|---|
| `std::io::Error` | `Internal` | IO failures are infrastructure errors |
| `serde_json::Error` | `InvalidArgument` (format) | Serialization failures indicate malformed input at API boundary |
| `sea_orm::DbErr` (feature `sea-orm`) | `Internal` | Database errors are infrastructure failures |

There are no impls for `anyhow::Error` or `sqlx::Error`. Gears needing finer-grained mapping (e.g., a missing row → `NotFound`) map the library error into their own domain error first and convert that to `CanonicalError`; the orphan rule prevents a gear from adding `From<LibraryError> for CanonicalError` itself.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements:

* `cpt-cf-errors-fr-compile-time-safety` — Typed enum provides exhaustive matching and type-safe context
* `cpt-cf-errors-fr-single-line-construction` — Ergonomic constructors (one per category) enable single-expression error creation
* `cpt-cf-errors-fr-library-error-propagation` — Blanket `From` impls enable `?` propagation
* `cpt-cf-errors-fr-transport-agnostic` — Enum carries no transport details; mapping is in `From<CanonicalError> for Problem`
* `cpt-cf-errors-fr-resource-scoped-construction` — `#[resource_error]` macro generates typed constructors that auto-tag `resource_type`
