---
cf: true
type: requirement
name: ToolKit Framework Compliance Review
version: 1.0
purpose: Verify that code changes follow ToolKit architectural rules and invariants
---

# ToolKit Framework Compliance Review

This review verifies that the pull request follows **ToolKit architectural invariants and patterns**.

It runs **in addition to the Rust PR review guidelines**.

Focus only on **framework-specific compliance**, not generic Rust style.

## Authoritative reference

When a change touches gear layout, `@/lib/toolkit`*, plugins, REST wiring, ClientHub, OpenAPI, lifecycle/stateful tasks, SSE, or standardized HTTP errors, consult `docs/toolkit_unified_system/README.md` — the canonical ToolKit architecture document. Findings that contradict it take priority.

---

# Output Contract

Produce an **issues-only report**.

For each issue include:

- Checklist ID
- Severity
- Location
- Issue
- Why it matters
- Fix

Do not repeat generic Rust findings.

---

# Severity

CRITICAL – breaks framework security model or architecture invariants
HIGH – breaks gear architecture or integration contracts
MEDIUM – deviation from recommended ToolKit patterns
LOW – minor style / best practice

---

# Core Framework Invariants

These rules apply to all gears.

---

## TOOLKIT-CORE-001: SDK Pattern Enforcement

Public gear APIs MUST be defined in `<gear>-sdk` crates.

Check:

- Traits used for inter-gear communication are defined in the SDK crate
- Public models live in the SDK crate
- Public error types live in the SDK crate
- Consumers depend only on `<gear>-sdk`

Violation examples:

- Gears importing internal domain types from another gear
- SDK leaking REST DTOs or database entities
- Consumers depending on gear implementation crate

Why it matters:

ToolKit enforces transport-agnostic APIs and gear isolation.

---

## TOOLKIT-CORE-002: Gear Layout Compliance

Gears must follow the canonical structure.

Required structure:

```

gears/<gear>/ <gear>-sdk/ <gear>/
api/rest
domain
infra

```

Check:

- REST DTOs exist only under `api/rest/dto.rs`
- Business logic lives in `domain/`
- Storage adapters live in `infra/storage`
- SDK types are not duplicated in the gear crate

Why it matters:

Ensures separation of API, domain, and infrastructure.

---

## TOOLKIT-CORE-003: Gear Naming Convention

Gear names must be **kebab-case**.

Check:

- Folder names
- `#[toolkit::gear(name = "...")]`

Why it matters:

Naming consistency is enforced by CI and macros.

---

# REST Layer Rules

---

## TOOLKIT-REST-001: OperationBuilder Usage

All REST endpoints must be defined via `OperationBuilder`.

Check:

- No direct Axum router manipulation
- Routes registered via `.register(router, openapi)`
- `.operation_id()` is defined
- `.standard_errors()` is included

Why it matters:

OperationBuilder guarantees compile-time completeness and OpenAPI correctness.

---

## TOOLKIT-REST-002: Authentication Declaration

Every endpoint must declare its auth posture.

Check:

- `.authenticated()` for protected routes
- `.public()` for open routes

Why it matters:

Gateway middleware depends on this metadata.

---

## TOOLKIT-REST-003: SecurityContext Extraction

Handlers must receive `SecurityContext` via Axum extension.

Correct pattern:

```

Extension(ctx): Extension<SecurityContext>

```

Check:

- SecurityContext not manually constructed
- Handlers do not bypass gateway injection

Why it matters:

AuthN is gateway responsibility.

---

# Error Handling

---

## TOOLKIT-ERR-001: RFC 9457 Problem Usage

All REST errors must use `Problem`.

Check:

- Handler return type is `ApiResult<T>`
- `Problem` is returned for errors
- No custom HTTP error structs

Why it matters:

ToolKit standardizes error handling with RFC-9457.

---

## TOOLKIT-ERR-002: Domain Error Separation

Domain errors must not contain transport logic.

Check:

- Domain errors defined in `domain/error.rs`
- SDK errors transport-agnostic
- Conversion chain:

```

DomainError → SDK Error → Problem

```

---

# Security Model

---

## TOOLKIT-SEC-001: SecureConn Enforcement

All database access must go through `SecureConn`.

Check:

- No raw database connections
- No direct `DatabaseConnection`
- Use `db.sea_secure()`

Why it matters:

SecureConn enforces authorization constraints.

---

## TOOLKIT-SEC-002: PolicyEnforcer Usage

Authorization must be handled through `PolicyEnforcer`.

Check:

- No manual `AccessScope` construction
- AccessScope obtained from PolicyEnforcer

Why it matters:

Authorization decisions must come from PDP.

---

# Database Layer

---

## TOOLKIT-DB-001: Repository Pattern

Repository methods must accept `&impl DBRunner`.

Check:

- No direct use of `SecureConn` in repository APIs
- Repository methods work both with transactions and normal queries

---

## TOOLKIT-DB-002: SQL Restrictions

Raw SQL must only exist in migrations.

Check:

- No SQL in handlers/services
- No SQL in repositories unless generated via ORM

Why it matters:

Database safety and migration discipline.

---

# ClientHub and Gears

---

## TOOLKIT-CLIENT-001: ClientHub Resolution

Gears must communicate via ClientHub.

Check:

- No direct gear dependency calls
- Clients resolved via:

```

ctx.client_hub().get::<dyn MyGearApi>()

```

---

## TOOLKIT-CLIENT-002: Plugin Isolation

Regular gears must not depend on plugin gears.

Check:

- Plugins accessed only via main gear API
- Scoped clients used for plugin resolution

---

# OData Integration

---

## TOOLKIT-ODATA-001: ODataFilterable Usage

DTO filtering must use `ODataFilterable`.

Check:

- DTOs derive `ODataFilterable`
- `.with_odata_filter()` used in OperationBuilder

---

# Lifecycle and Background Tasks

---

## TOOLKIT-LIFE-001: CancellationToken Usage

Background tasks must respect cancellation.

Check:

- CancellationToken passed to tasks
- Tasks stop on token cancellation

---

# Out-of-Process Gears

---

## TOOLKIT-OOP-001: SDK Pattern for gRPC

Out-of-process gears must expose API via SDK crate.

Check:

- gRPC client defined in SDK
- server implementation in gear crate

---

# Additional Heuristics

Be suspicious of:

- gears accessing DB directly without SecureConn
- gears calling other gears directly instead of ClientHub
- REST handlers performing domain logic instead of delegating to services
- DTO types leaking into SDK
- entities leaking into REST

---

# Review Philosophy

ToolKit prioritizes:

- gear isolation
- transport-agnostic APIs
- secure data access
- explicit authorization
- compile-time safe REST wiring

Framework rules override convenience.

If a change violates these invariants, it should be flagged even if the Rust code itself is correct.
