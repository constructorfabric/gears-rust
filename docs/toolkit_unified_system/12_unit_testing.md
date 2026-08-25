<!-- Created: 2026-04-07 by Constructor Tech -->
<!-- Updated: 2026-04-28 by Constructor Tech -->

# Unit & Integration Testing Guide

This document defines the philosophy, infrastructure, and patterns for unit and integration tests across all ToolKit gears. Gear-specific test plans (which test cases to add, gap analysis, domain-specific assertions) live in each gear's `docs/features/` folder.

---

## Philosophy

### What Is a Unit Test in This Project

A unit test calls a Rust function directly — a service method, a value object constructor, an error conversion — and verifies the result. No network I/O, no running server.

This is the **primary line of defense**. Every domain invariant, every validation rule, every error path is covered here. E2E tests (see `13_e2e_testing.md`) exist only to verify integration seams that unit tests cannot see.

### Database Usage in Unit Tests

**Ideally, unit tests do not touch a real database.** The preference is to mock dependencies at the appropriate level:

- Pure logic (validation, value objects, error mapping, DTO conversions) — `#[test]`, no DB, no mocks needed
- Service-level tests — mock the repository trait to return canned data; the service logic runs against the mock
- Handler-level tests — mock the service; test routing, deserialization, and response shape in isolation

**At the lowest level** — between the repository implementation and the actual DB — using a real **SQLite `:memory:`** database is acceptable and often preferable. Running migrations against an in-memory SQLite (~1ms per DB) gives confidence that SQL is correct without the complexity of maintaining mock data structures.

**Using a real DB in unit tests is undesirable but not forbidden.** The final decision is left to the developer's judgment based on what produces cleaner, more readable, and more maintainable tests. A well-written service test against SQLite `:memory:` is better than a poorly-written test against a brittle mock. When in doubt: mock higher-level collaborators, use real SQLite only where the SQL itself needs to be verified.

### Three Questions Before Adding a Test

Every test must pass all three:

1. **"Does this test verify deterministic domain logic?"**
   If yes — it belongs here. Type validation, hierarchy invariants, field constraints, error mapping — all deterministic, all testable without HTTP.

2. **"Is this test atomic and fast?"**
   One `#[test]` = one scenario. No `sleep()`, no `timeout()`, no retry loops. Each test creates its own SQLite DB and service instances. Tests run in parallel (`cargo test -j N`). Target: entire suite < 5 seconds.

3. **"Does removing this test reduce confidence in domain correctness?"**
   If not — the test is redundant. Every test must guard a specific behavior that, if broken, would allow data corruption, invariant violation, or silent failure.

### What Belongs in Unit Tests

| Layer | What | How |
|-------|------|-----|
| **Domain services** | Entity CRUD, lifecycle operations, business invariants | `#[tokio::test]` with SQLite `:memory:` + mocked AuthZ |
| **Domain validation** | Format validation, invariant enforcement, field length limits | `#[test]` pure logic, no DB |
| **Value objects** | Parsing, normalization, serde round-trip | `#[cfg(test)]` in-source |
| **Error chains** | `DomainError` → gear error → RFC 9457 `Problem`, external error → `DomainError` | `#[test]` pure logic |
| **DTO conversions** | `From` impls, serde attributes (`rename`, `skip_serializing_if`, `default`, `camelCase`) | `#[cfg(test)]` in-source |
| **OData fields** | `FilterField` name/kind mapping, OData mapper field→column | `#[cfg(test)]` in-source |
| **Seeding** | Idempotent bootstrap logic: create/skip/update semantics | `#[tokio::test]` with DB assertion |
| **Surrogate ID non-exposure** | API responses contain string identifiers, never internal numeric IDs | REST-level `Router::oneshot` |

### What Does NOT Belong Here

- HTTP routing, middleware wiring, header serialization → E2E tests
- PostgreSQL-specific behavior (FK RESTRICT, SERIALIZABLE isolation, domain types) → E2E tests
- Real AuthN/AuthZ pipeline with tokens → E2E tests
- MTLS certificate verification → E2E tests _(p2 — deferred, not implemented yet)_
- Cursor codec encode/decode over HTTP → E2E tests
- Performance, load, concurrency under contention → out of scope

All of the above requires real PostgreSQL; the entries that name the transport or the AuthN/AuthZ pipeline additionally require a running server. Unit tests use SQLite and mock AuthZ — they cannot catch these bugs. Throughout this guide "E2E" means HTTP over PostgreSQL: a case that needs the real database but not the transport belongs in a PostgreSQL-backed Rust integration suite, which runs in-process and boots no server.

Routing a case to E2E only counts as coverage if the venue actually runs on PostgreSQL. Some pytest harnesses seed SQLite directly; sending a PostgreSQL-specific case there moves it out of the unit suite without testing it anywhere.

Where the divergence is in the SQL or the schema rather than in the transport — a dialect rejection, a column width, a `CHECK` or FK — the better venue is usually a feature-gated Rust suite against real PostgreSQL via `testcontainers`, calling repository or service code directly. It is in use today (`--features integration`, a `test-<gear>-pg` target, a step in CI's `integration` job) and diagnoses the failure far more precisely than an HTTP status. See [Check which venue you actually have](13_e2e_testing.md#coverage-goal-one-call-per-api-method) in the E2E guide; record a gap only when neither venue exists.

### Relationship to E2E Tests

Unit and E2E tests form a **complementary pair with zero overlap**:

| Concern | Unit tests | E2E tests |
|---------|------------|-----------|
| Domain invariants | **Yes** (primary) | No |
| Field validation | **Yes** (service-level) | No |
| Table row correctness | **Yes** (SQLite) | Yes (PostgreSQL dialect) |
| Error response format | DomainError→Problem mapping | HTTP headers + Content-Type |
| Tenant isolation | AccessScope construction + scoped queries | Real tokens + real WHERE |
| JSON wire format | DTO serde attrs in-source | Full HTTP roundtrip |
| OData $filter | FilterField name/kind | Full parse→SQL→result chain |

If a bug is catchable by calling a Rust function directly, it lives in unit tests. If it requires HTTP + PostgreSQL, it lives in E2E tests.

---

## Reliability Principles

**Atomic** — one `#[test]` = one behavior. No compound "test everything" functions.

**Fast** — no `sleep`, no `timeout`, no `tokio::time::*`, no polling. SQLite `:memory:` is ~1ms. Target: full suite < 5s.

**Independent** — no shared state. Each test creates its own DB. `cargo test -j N` runs in parallel.

**Synchronous where possible** — pure logic uses `#[test]`, not `#[tokio::test]`. Async only when DB is involved.

**Direct DB assertions** — do not rely solely on service-layer reads (they go through AccessScope). Use the concrete entity type (e.g., `UserEntity::find()` with `use sea_orm::EntityTrait;` in scope) directly to verify table state.

**No new crate dependencies for testing** — follow project conventions: `assert!(matches!(err, Variant { .. }), "msg: {err:?}")` not `assert_matches!`. Manual `vec![]` + loop for table-driven tests, not `rstest`. Plain `async fn` helpers, not fixtures.

**No retry testing** — SERIALIZABLE retry loops are an implementation detail. Tests do not simulate contention.

---

## Test Infrastructure

### Core Principles

1. **Atomic**: each test verifies exactly one behavior. No "also check this while we're here".
2. **Fast**: no `sleep`, no `timeout`, no `tokio::time::*`, no polling, no retries. Target: entire suite < 5s.
3. **Independent**: no shared state. Each test creates its own `SQLite :memory:` DB and fresh service instances. Tests run in any order and in parallel.
4. **Synchronous where possible**: pure logic tests are `#[test]`, not `#[tokio::test]`. Async only when DB or service layer is involved.
5. **Direct DB queries for state verification**: use the concrete entity's `find()` method (e.g., `MyEntity::find()` with `use sea_orm::EntityTrait;` in scope) directly to inspect table state. Do NOT rely solely on service-layer reads to verify writes (service reads go through AccessScope which may filter).

### Anti-Patterns (DO NOT)

```rust
// BAD: timer in test
tokio::time::sleep(Duration::from_millis(100)).await;

// BAD: retry/poll loop
for _ in 0..10 { if check() { break; } sleep(50ms); }

// BAD: compound test
#[tokio::test]
async fn test_everything() {
    // creates entity, moves it, deletes it, checks seeding...
}

// BAD: verifying write via scoped read only
let item = svc.get(&ctx, id).await?; // goes through AccessScope!
// This does NOT prove the DB state — a scope bug could hide the item

// GOOD: direct DB assertion
use sea_orm::EntityTrait;
let model = my_entity::Entity::find_by_id(id).one(&conn).await?.unwrap();
assert_eq!(model.parent_id, Some(new_parent));
```

### Assertion & Parameterization Patterns

**Error variant checks** — use `assert!(matches!(...))` with descriptive message:

```rust
let err = result.unwrap_err();
assert!(
    matches!(err, DomainError::NotFound { .. }),
    "Expected NotFound, got: {err:?}"
);
assert!(err.to_string().contains("not found"));
```

**Table-driven tests** — use manual `vec![]` + loop (NOT `rstest`):

```rust
#[test]
fn domain_errors_map_to_correct_status_codes() {
    let cases: Vec<(DomainError, StatusCode)> = vec![
        (DomainError::not_found("x"), StatusCode::NOT_FOUND),
        (DomainError::validation("x"), StatusCode::BAD_REQUEST),
        (DomainError::conflict("x"), StatusCode::CONFLICT),
    ];
    for (err, expected_status) in cases {
        let problem: Problem = err.into();
        assert_eq!(problem.status, expected_status, "for error: {problem:?}");
    }
}
```

**Value object validation** — table-driven with loop:

```rust
#[test]
fn value_object_valid_cases() {
    let valid = vec!["valid-input-1", "valid-input-2"];
    for input in valid {
        assert!(MyValueObject::new(input).is_ok(), "should be valid: {input}");
    }
}

#[test]
fn value_object_invalid_cases() {
    let invalid = vec![
        ("", "empty input"),
        ("bad format", "wrong format"),
    ];
    for (input, reason) in invalid {
        assert!(MyValueObject::new(input).is_err(), "should reject ({reason}): {input}");
    }
}
```

**Setup helpers** — plain `async fn` in `tests/common/mod.rs` (NOT fixtures, NOT `rstest`).

### Shared Test Helpers (`tests/common/mod.rs`)

Extract duplicated setup code into a shared gear:

```rust
// tests/common/mod.rs

/// SQLite in-memory DB with migrations. ~1ms per call.
pub async fn test_db() -> Arc<DBProvider<DbError>> { ... }

/// SecurityContext for given tenant.
pub fn make_ctx(tenant_id: Uuid) -> SecurityContext { ... }

/// AllowAll PolicyEnforcer (returns tenant-scoped AccessScope).
pub fn make_enforcer() -> PolicyEnforcer { ... }
```

Gear-specific helpers (e.g., `create_root_group`, `assert_closure_rows`) live alongside the shared helpers in `tests/common/`.

### Naming Convention

Tests follow the pattern `{area}_{scenario}` in snake_case:

```
entity_create_with_valid_parent_succeeds
entity_move_under_descendant_returns_cycle_detected
membership_add_duplicate_returns_conflict
seeding_idempotent_on_second_run
value_object_rejects_empty_string
```

### Test File Organization

```
# In-source unit tests (pure logic, #[test] only — instant)
<gear>-sdk/src/models.rs              # Value object validation, serde round-trip, SDK model shape
<gear>-sdk/src/odata/<entity>.rs      # FilterField name/kind correctness
<gear>/src/api/rest/dto.rs            # DTO From impls, serde rename, camelCase, skip_serializing_if
<gear>/src/infra/storage/odata_mapper.rs  # OData mapper field→column mapping

# Integration tests (SQLite :memory: DB, #[tokio::test])
tests/
  common/mod.rs               # Shared helpers + assertion helpers
  domain_unit_test.rs         # DomainError construction, error chains, pure domain logic
  api_rest_test.rs            # REST layer: Router::oneshot, status codes, JSON shapes, PATCH
  authz_integration_test.rs   # PolicyEnforcer tenant scoping (mocked PDP)
  tenant_filtering_db_test.rs # AccessScope + scoped queries on SQLite
  tenant_scoping_test.rs      # AccessScope construction logic
  <entity>_service_test.rs    # Domain service: CRUD, invariants, error paths
  seeding_test.rs             # Idempotent seed: create/skip/update semantics
```

---

## What to Assert in Every Test

The goal of a unit test is to verify **as many observable facts as is reasonable** in a single scenario — not just "it didn't crash". A test that only checks `result.is_ok()` provides almost no value.

### The Four Assertion Dimensions

For every test, cover all dimensions that apply:

**1. Primary outcome** — did the operation succeed or fail with the expected result/error?
```rust
let result = svc.create_entity(&ctx, req).await;
assert!(result.is_ok(), "expected Ok, got: {result:?}");
// or for error paths:
assert!(matches!(result.unwrap_err(), DomainError::NotFound { .. }));
```

**2. Return value fields** — not just the status, but the actual data returned. All fields that matter must be explicitly asserted:
```rust
let entity = result.unwrap();
assert_eq!(entity.name, "Expected Name");
assert_eq!(entity.tenant_id, ctx.tenant_id());
assert_eq!(entity.status, Status::Active);
assert!(entity.created_at <= Utc::now());
// Secondary fields matter too — don't skip them
assert!(entity.metadata.is_none(), "fresh entity should have no metadata");
```

**3. Mock call verification** — when using mocks, assert both *how many times* and *with what arguments* each mock was called:
```rust
// Verify the mock was called exactly once
mock_repo.assert_called_once();

// Verify it received the correct input — not just "it was called"
mock_repo.assert_called_with(CreateEntityRequest {
    name: "Expected Name".to_string(),
    tenant_id: tenant_id,
    ..
});

// Verify a mock was NOT called (for negative paths)
mock_notifier.assert_not_called();
```

**4. Secondary artifacts** — side effects beyond the return value. For DB-backed tests, verify the actual state in storage:
```rust
// Don't rely only on the service return value
let stored = MyEntity::find_by_id(entity.id).one(&conn).await?.unwrap();
assert_eq!(stored.parent_id, Some(parent_id));  // FK set correctly
assert_eq!(stored.status, "active");             // correct initial state
// Rows that should NOT exist
let orphans = RelatedEntity::find()
    .filter(Column::ParentId.eq(deleted_id))
    .count(&conn).await?;
assert_eq!(orphans, 0, "cascade delete must remove related rows");
```

### What "Secondary Artifacts" Means in Practice

When an operation succeeds, many things happen beyond the return value. Each is a potential bug surface:

| Operation | What else to assert |
|-----------|---------------------|
| **Create entity** | All fields in DB match request (not just the returned struct). FK columns resolved to correct IDs. Timestamps set. |
| **Update fields** | Changed fields updated in DB. **Unchanged fields untouched** — a partial update must not zero-out unrelated fields. |
| **Delete entity** | Entity row gone. Related junction rows gone. Parent entity **unaffected**. |
| **Service call with side effects** | Every observable side effect has an assertion. If a notification is sent, assert it. If a counter increments, assert it. |
| **Error path** | Not only the error variant, but the error message contains expected context (entity name, field name, violating value). |

### The "Lazy Assert" Trap

```rust
// BAD — only checks it didn't error
let result = svc.update(&ctx, id, patch).await;
assert!(result.is_ok());

// BAD — checks the return but not DB state (scope bug could hide corruption)
let updated = result.unwrap();
assert_eq!(updated.name, "new name");
// Does NOT verify that unrelated fields were preserved in storage

// GOOD — checks return value AND DB state AND untouched fields
let updated = result.unwrap();
assert_eq!(updated.name, "new name");
let stored = Entity::find_by_id(id).one(&conn).await?.unwrap();
assert_eq!(stored.name, "new name");       // changed field
assert_eq!(stored.status, original_status); // untouched field preserved
assert_eq!(stored.tenant_id, original_tenant_id); // tenant not leaked
```

---

## What to Verify Beyond Ok/Err

Do not rely only on the return value. Mandatory complementary assertions:

### Table State (direct DB queries)

For every write operation, use the concrete entity's `find()` method (e.g., `MyEntity::find()` with `use sea_orm::EntityTrait;` in scope) to confirm:

| Operation | Required DB Assertions |
|-----------|----------------------|
| **Create entity** | Row exists with all fields matching request. FK columns resolved correctly. |
| **Update fields** | Changed fields updated. Unchanged fields **untouched**. |
| **Delete entity** | Row gone. Related rows (FK children) handled per cascade policy. |
| **Seeding (create)** | Entity physically exists in DB. Junction rows present. |
| **Seeding (unchanged)** | `updated_at` **not modified** on re-run. |

### Junction Tables

| Operation | Required DB Assertions |
|-----------|----------------------|
| **Create with references** | Junction rows COUNT = `len(references)`. Each FK correctly resolved from string to surrogate ID. |
| **Update (replace list)** | Old junction rows **deleted**. New rows match new list. COUNT = `len(new_list)`. |
| **Delete (CASCADE)** | Junction rows for deleted entity → 0. |

### Surrogate ID Non-Exposure (REST tests)

Every REST test response **MUST** verify:
- No numeric surrogate ID fields in JSON (e.g., no `type_id`, `parent_type_id`)
- Identifier fields are **strings** (UUIDs or GTS paths), not integers
- Internal-only fields absent from response

### Error Response Shape (REST tests)

For error-path REST tests via `Router::oneshot`:
- HTTP status code matches expected
- `Content-Type` header contains `application/problem+json`
- Response body has `status`, `title`, `detail` fields (RFC 9457)
- No `stack`, `trace`, `backtrace` fields (no internal leaks)

---

## Priority Matrix

Use P1/P2/P3 to prioritize test implementation in gear-specific plans:

### P1 — Critical (business invariants)

Tests that prevent data corruption or violate core business rules:
- All domain invariants from gear acceptance criteria
- Every error path that prevents illegal state (e.g., cascade, duplicate, constraint violation)
- Value object validation (empty, invalid format, boundary)
- Serde round-trip for SDK models (incorrect wire format breaks API clients silently)
- OData FilterField name/kind (incorrect mapping breaks $filter silently)
- Seeding create/skip/update (deployment-critical bootstrap)

### P2 — Important (error paths, REST layer, edges)

Tests that cover secondary paths and REST-level verification:
- Error conversion chains (`ExternalError` → `DomainError` → `Problem`)
- REST endpoint coverage (PUT, PATCH, DELETE, hierarchy, force operations)
- DTO conversion correctness (`From` impls, `#[serde(default)]`)
- OData mapper field→column mapping
- Boundary conditions (max length, empty list, boundary values)

### P3 — Nice to Have (boundary, cosmetic)

Tests that add confidence without blocking delivery:
- `Display` + `Into<String>` for value objects
- Empty-list edge cases for seeding
- Cosmetic behavior (field ordering, default values)

---

## Acceptance Criteria (gear test suite)

- All unit tests pass (`cargo test -p <gear> -p <gear>-sdk`) — 0 failed
- Full suite completes in < 5 seconds
- Zero `sleep`, `timeout`, or `tokio::time` usage in tests
- Every domain invariant from gear acceptance criteria is covered by at least one test
- `make all` (or `make fmt && make lint && make test`) passes with zero errors
