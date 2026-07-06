# Technical Design — Canonical Error System

## 1. Architecture Overview

### 1.1 Architectural Vision

The canonical error system provides a single, universal error type (`CanonicalError`) that all Gears use to express failures. It replaces the ad-hoc `Problem::new()` / `ErrDef` / `declare_errors!` / `ErrorCode` patterns with a typed, transport-agnostic model.

**Canonical errors** are a closed set of 16 error categories (based on Google's canonical error codes). Each category has:
- A typed context struct carrying machine-readable error details
- A GTS compound type identifier for global uniqueness
- A fixed HTTP status mapping for REST (gRPC/SSE mappings are future work)

The `CanonicalError` enum has one variant per category. Every variant carries four fields: `ctx` (category-specific context type), `detail`, `resource_type`, and `resource_name` (the `Internal` variant is an exception — it carries only `ctx` and `detail`). Constructing an error uses the builder pattern — e.g., `CanonicalError::internal("db failure").create()` or `UserResourceError::not_found("User not found").with_resource("user-123").create()`. Using the wrong context type for a category is a compile error.

**Builder-only enforcement**: The enum and all variants are marked `#[non_exhaustive]`, preventing direct variant construction from outside the crate. Internal constructors and mutation methods (`with_detail`, `with_resource_type`, `with_resource`) are `pub(crate)`, ensuring the only external construction path is through the builder API (`ResourceErrorBuilder` or the three non-resource builders: `internal`, `service_unavailable`, `unauthenticated`).

Canonical errors construction examples:

```rust
// 1. Direct canonical error
let auth_err = CanonicalError::unauthenticated()
    .with_reason("TOKEN_EXPIRED")
    .create();

// 2. Library error propagation via ?
async fn process_data() -> Result<Data, CanonicalError> {
    let file = tokio::fs::read("data.json").await?;  // io::Error → Internal
    let data: Data = serde_json::from_slice(&file)?; // serde_json::Error → InvalidArgument
    Ok(data)
}

// 3. Resource-scoped error construction
use toolkit_canonical_errors::resource_error;

#[resource_error("gts.cf.core.users.user.v1~")]
struct UserResourceError;

async fn get_user(id: &str) -> Result<User, CanonicalError> {
    db.find_user(id)
        .await?  // DbErr → Internal
        .ok_or_else(|| UserResourceError::not_found("User not found")
            .with_resource(id)
            .create())
}

// 4. Validation error with multiple field violations
let validation_err = UserResourceError::invalid_argument()
    .with_field_violation("email", "Invalid email format", "INVALID_FORMAT")
    .with_field_violation("age", "Must be between 0 and 120", "OUT_OF_RANGE")
    .create();
```

**Resource-scoped errors** are a convenience layer for gear-owned resources. The `#[resource_error]` attribute macro declares a resource type and generates constructors that auto-tag every error with the resource's GTS identity:

```rust
use toolkit_canonical_errors::resource_error;

#[resource_error("gts.cf.core.users.user.v1~")]
struct UserResourceError;

// Generated constructors return builders — call .create() to produce CanonicalError:
UserResourceError::not_found("User not found")     // → builder (resource required)
    .with_resource("user-123").create();             // → CanonicalError::NotFound with resource_type set
UserResourceError::permission_denied().create();     // → CanonicalError::PermissionDenied with resource_type set
```

Non-resource errors (e.g., `service_unavailable`, `unauthenticated`) use `CanonicalError::` constructors directly.

The `Problem` struct (RFC 9457) is the REST wire format. A single `From<CanonicalError> for Problem` implementation handles all 16 categories.

### 1.2 Canonical Error Categories

GTS ID pattern: `gts.cf.core.errors.err.v1~cf.core.err.{category}.v1~`

| # | Category | HTTP | Use When |
|---|----------|------|----------|
| 1 | `cancelled` | 499 | Client cancelled the request before completion |
| 2 | `unknown` | 500 | Error doesn't match any other category |
| 3 | `invalid_argument` | 400 | Invalid request — malformed fields, bad format, constraint violations |
| 4 | `deadline_exceeded` | 504 | Server did not complete within the allowed time |
| 5 | `not_found` | 404 | Resource does not exist or filtered by access controls |
| 6 | `already_exists` | 409 | Resource the client tried to create already exists |
| 7 | `permission_denied` | 403 | Authenticated but insufficient permissions |
| 8 | `resource_exhausted` | 429 | Quota or rate limit exceeded |
| 9 | `failed_precondition` | 400 | Valid request but system state prevents execution |
| 10 | `aborted` | 409 | Concurrency conflict (optimistic lock, transaction) |
| 11 | `out_of_range` | 400 | Value syntactically valid but outside acceptable range |
| 12 | `unimplemented` | 501 | Operation recognized but not yet implemented |
| 13 | `internal` | 500 | Known infrastructure failure (DB, IO, serialization) |
| 14 | `service_unavailable` | 503 | Service temporarily unavailable (system-level only) |
| 15 | `data_loss` | 500 | Unrecoverable data loss or corruption |
| 16 | `unauthenticated` | 401 | No valid authentication credentials |

Note: this design keeps Google `unavailable` semantics, but uses the explicit platform name `service_unavailable` for the canonical category identifier.

See [§ 4. Category Reference](#4-category-reference) for full definitions including context schemas, constructors, and JSON wire examples.

### 1.3 Architecture Drivers

#### Functional Drivers

| Requirement | Design Response |
|-------------|-----------------|
| `cpt-cf-errors-fr-transport-agnostic` | `CanonicalError` enum carries no transport details; `From` impls at boundaries |
| `cpt-cf-errors-fr-finite-vocabulary` | 16-variant enum with exhaustive `match` |
| `cpt-cf-errors-fr-structured-context` | Each variant carries a typed context struct |
| `cpt-cf-errors-fr-mandatory-trace-id` | `Problem.trace_id` field populated by middleware |
| `cpt-cf-errors-fr-public-private-isolation` | Internal details never included in production responses; `trace_id` used for correlation |
| `cpt-cf-errors-fr-compile-time-safety` | Typed enum + `#[resource_error]` macro |
| `cpt-cf-errors-fr-gts-identification` | `GtsSchema` trait with `SCHEMA_ID` const per context type |
| `cpt-cf-errors-fr-single-line-construction` | One builder per category; `CanonicalError::category(detail).create()` or `ResourceError::category(detail).with_resource(id).create()` |
| `cpt-cf-errors-fr-resource-scoped-construction` | `#[resource_error]` macro generates constructors with auto-tagged `resource_type` |
| `cpt-cf-errors-fr-builder-only-construction` | `#[non_exhaustive]` on enum + variants; `pub(crate)` internal constructors and mutation methods |
| `cpt-cf-errors-fr-library-error-propagation` | Blanket `From` impls for common library errors (`io::Error`, `serde_json::Error`, `DbErr`) |
| `cpt-cf-errors-fr-schema-drift-prevention` | Showcase snapshot tests + `cargo-semver-checks` + schema file diffing in CI |
| `cpt-cf-errors-fr-standard-adoption` | RFC 9457 Problem Details as REST wire format |

#### Key ADRs

| ADR ID | Decision Summary |
|--------|-----------------|
| `cpt-cf-errors-adr-canonical-error-categories` | 16 canonical categories based on Google's error codes |
| `cpt-cf-errors-adr-gts-error-identification` | GTS compound type identifiers for error categories |
| `cpt-cf-errors-adr-rfc9457-wire-format` | RFC 9457 Problem Details as REST wire format |
| `cpt-cf-errors-adr-typed-enum-impl` | Typed enum with category-typed constructors |

### 1.4 Architecture Layers

```text
      Gear handler code
               │
               │  CanonicalError::category(detail).create()
               │  or ResourceError::category(detail).with_resource(id).create()
               v
      ┌─────────────────┐
      │  CanonicalError │ ← domain layer (transport-agnostic)
      │  (16 variants)  │
      └────────┬────────┘
               │
    ┌──────────┼──────────┐
    v          v          v
 Problem    Status      Event
 (REST)     (gRPC)      (SSE)
 RFC9457    (future)    (future)
```

## 2. Principles & Constraints

### 2.1 Design Principles

#### Transport Agnosticism

- [ ] `p1` - **ID**: `cpt-cf-errors-principle-transport-agnosticism`

`CanonicalError` is the only error type accepted by API layers. It carries no HTTP status codes, gRPC codes, or transport headers. Transport mapping happens in exactly one `From` impl per transport at the boundary.

**ADRs**: `cpt-cf-errors-adr-typed-enum-impl`

#### Single Error Gateway

- [ ] `p1` - **ID**: `cpt-cf-errors-principle-single-error-gateway`

There is no alternative path for returning errors. Every REST error response is produced from a `CanonicalError` via `From<CanonicalError> for Problem`. This eliminates inconsistent error formats across gears.

**ADRs**: `cpt-cf-errors-adr-typed-enum-impl`

#### Fixed Context Structures

- [ ] `p1` - **ID**: `cpt-cf-errors-principle-fixed-context-structures`

Each canonical category has exactly one associated context type with a fixed set of fields. This prevents ad-hoc metadata keys, ensures consumers can parse error details without guessing, and makes the error surface auditable at compile time.

**ADRs**: `cpt-cf-errors-adr-canonical-error-categories`

#### Catalog-First

- [ ] `p2` - **ID**: `cpt-cf-errors-principle-catalog-first`

Every canonical category has a GTS identifier assigned before any code is written. The catalog is the source of truth for error codes.

**ADRs**: `cpt-cf-errors-adr-gts-error-identification`

#### Fail-Safe Fallback

- [ ] `p2` - **ID**: `cpt-cf-errors-principle-fail-safe-fallback`

Any error that does not match a canonical category is mapped to `internal` with a trace ID. No error escapes the system without a canonical category.

> **Note**: Full enforcement of this principle (catching panics, unhandled rejections, and unknown error types in middleware) depends on the error middleware catch-all capability, which is out of scope for the current phase (see PRD §4.2). In the current phase, the principle is upheld by compile-time enforcement (typed enum, Architecture lint rules) and the single `From<CanonicalError> for Problem` conversion path.

### 2.2 Constraints

#### RFC 9457 Compliance

- [ ] `p1` - **ID**: `cpt-cf-errors-constraint-rfc9457`

All REST error responses use `Content-Type: application/problem+json` and include the RFC 9457 fields: `type`, `title`, `status`, `detail`, and `instance`. The `type` field carries the GTS URI for the error category.

**ADRs**: `cpt-cf-errors-adr-rfc9457-wire-format`

#### GTS Code Format

- [ ] `p1` - **ID**: `cpt-cf-errors-constraint-gts-code-format`

Error category GTS identifiers use the compound GTS type format:

```text
gts.cf.core.errors.err.v1~cf.core.err.{category}.v1~
```

where `{category}` is the lowercase canonical category name (e.g., `not_found`, `invalid_argument`).

**ADRs**: `cpt-cf-errors-adr-gts-error-identification`

#### No Internal Details in Production

- [ ] `p1` - **ID**: `cpt-cf-errors-constraint-no-internal-details`

In production, `internal` and `unknown` errors return an opaque message with a `trace_id` for correlation. The `detail` field for `internal` errors contains a generic message, never exception text or stack traces. Internal diagnostic information (stack traces, connection strings, file paths) is logged server-side with the `trace_id` for correlation and never included in the response.

#### Error Contract Stability

- [ ] `p1` - **ID**: `cpt-cf-errors-constraint-error-contract-stability`

Every error response consists of **contract parts** (fixed per category) and **variable parts** (per-occurrence).

**Contract parts** (part of public API surface — breaking change policy applies):
- Canonical category
- Context type schema (field names and types)
- GTS identifier
- HTTP status code
- Title

**Variable parts** (not part of the contract — may change freely):
- `detail` message
- `instance` path
- `trace_id`
- Context field values

**Breaking changes** (require major version bump of `cf-gears-toolkit-errors`):
- Removing or renaming a canonical category
- Changing the context type associated with a category
- Removing or renaming a field in a context type schema
- Changing the type of a field in a context type schema
- Changing the GTS identifier of a category
- Changing the HTTP status code mapped to a category

**Non-breaking changes** (minor version):
- Adding a new optional field to a context type
- Adding a new canonical category

#### Macro-Based GTS Construction

- [ ] `p1` - **ID**: `cpt-cf-errors-constraint-macro-gts-construction`

Resource types are declared via the `#[resource_error]` attribute macro that associates a GTS identifier with a named type. The macro generates error constructors for 13 canonical categories (all except `internal`, `service_unavailable`, and `unauthenticated`, which are not resource-scoped), and tags every generated constructor with `resource_type` automatically.

```rust
use toolkit_canonical_errors::resource_error;

#[resource_error("gts.cf.core.users.user.v1~")]
struct UserResourceError;

// Usage in a handler:
async fn get_user(Path(id): Path<String>) -> Result<Json<User>, CanonicalError> {
    let user = db.find_user(&id)
        .await?  // DbErr → CanonicalError::Internal via blanket From
        .ok_or_else(|| UserResourceError::not_found("User not found")
            .with_resource(&id)
            .create())?;
    Ok(Json(user))
}
```

## 3. Technical Architecture

### 3.1 Domain Model

**Technology**: Rust enums, GTS

**Location**: [`libs/toolkit-canonical-errors/src/`](../../../libs/toolkit-canonical-errors/src/)

**Core Entities**:

| Entity | Description |
|--------|-------------|
| `CanonicalError` | 16-variant enum — the universal error type |
| `Problem` | RFC 9457 wire format struct for REST responses |
| Context types | `Validation`, `ResourceInfo`, `ErrorInfo`, `QuotaFailure`, `PreconditionFailure`, `RetryInfo`, `RequestInfo` |

### 3.2 Component Model

```text
┌─────────────────────────────────────────────────┐
│  libs/toolkit-errors                             │
│  ┌───────────────┐  ┌─────────────────────────┐ │
│  │ CanonicalError│  │ Context Types           │ │
│  │ (16 variants) │──│ Validation, ResourceInfo│ │
│  └───────┬───────┘  │ ErrorInfo, QuotaFailure │ │
│          │          │ PreconditionFailure,    │ │
│          │          │ RetryInfo, RequestInfo  │ │
│          │          └─────────────────────────┘ │
│          v                                      │
│  ┌─────────────────┐                            │
│  │ REST Mapping    │  From<CanonicalError>      │
│  │ → Problem       │  for Problem               │
│  └─────────────────┘                            │
├─────────────────────────────────────────────────┤
│  libs/toolkit-canonical-errors-macro              │
│  ┌──────────────────────┐                       │
│  │ #[resource_error]    │ macro                 │
│  └──────────────────────┘                       │
├─────────────────────────────────────────────────┤
│  architecture lints (via `cargo gears lint`)                                │
│  ┌─────────────────┐                            │
│  │ Architecture Lint Rules     │ compile-time lint           │
│  └─────────────────┘                            │
└─────────────────────────────────────────────────┘
```

#### CanonicalError

- [ ] `p1` - **ID**: `cpt-cf-errors-component-canonical-error`

**Responsibility scope**:

Owns the 16 canonical error categories. Owns the mapping from category to GTS identifier, HTTP status code, and title. Each variant is a struct with four fields: `ctx` (category-specific context type), `detail: String`, `resource_type: Option<String>`, `resource_name: Option<String>` (the `Internal` variant carries only `ctx` and `detail`).

Provides builder-returning constructors (one per category) and public accessors (`detail()`, `resource_type()`, `resource_name()`, `gts_type()`, `status_code()`, `title()`). Internal mutation methods (`with_detail()`, `with_resource_type()`, `with_resource()`) are `pub(crate)` — used only by the builder's `.create()` implementation. The enum and all variants are `#[non_exhaustive]`, preventing direct variant construction from outside the crate.

Provides blanket `From` implementations for common library error types so that `?` propagates library errors into canonical categories without per-call-site mapping.

**Responsibility boundaries**:

Does not know about HTTP, gRPC, or any transport. Does not perform serialization. Does not enrich errors with trace IDs (that is the middleware's job).

##### Related components (by ID)

- `cpt-cf-errors-component-context-types` — provides the context structs carried by each variant
- `cpt-cf-errors-component-rest-mapping` — consumes `CanonicalError` and produces `Problem`

#### Context Types

- [ ] `p1` - **ID**: `cpt-cf-errors-component-context-types`

**Responsibility scope**:

Defines the structured payload types for each error category. All context types use versioned naming (`XxxV1`) with unversioned type aliases (e.g., `pub type ResourceInfo = ResourceInfoV1;`). Each struct has a fixed set of public fields and provides builder/constructor methods. All context types implement the `GtsSchema` trait (via `#[struct_to_gts_schema]` macro) and carry an internal `gts_type: GtsSchemaId` field that is skipped during serialization.

**Responsibility boundaries**:

Context types are pure data. They do not perform validation, logging, or transport mapping.

##### Related components (by ID)

- `cpt-cf-errors-component-canonical-error` — uses context types as variant payloads

#### REST Mapping Layer

- [ ] `p1` - **ID**: `cpt-cf-errors-component-rest-mapping`

**Responsibility scope**:

Implements `From<CanonicalError> for Problem`.

Maps each category to its HTTP status code and serializes the context type into the `context` JSON field. Injects `resource_type` into the context JSON when present.

**Responsibility boundaries**:

Only handles REST (HTTP). Does not handle gRPC or SSE. Does not add `trace_id` or `instance` — those are set by the middleware/framework layer.

##### Related components (by ID)

- `cpt-cf-errors-component-canonical-error` — consumed by this layer
- `cpt-cf-errors-component-error-middleware` — calls this layer to produce Problem responses

#### Error Middleware

- [ ] `p2` - **ID**: `cpt-cf-errors-component-error-middleware`

**Responsibility scope**:

Axum middleware that catches any `CanonicalError` returned from handlers, calls `Problem::from_error()`, sets `trace_id` from the request span, sets `instance` from the request URI, and returns the `application/problem+json` response.

**Responsibility boundaries**:

Does not construct domain errors. Does not decide which category to use — that is the handler's job.

> **Out of scope (current phase)**: Catch-all behavior (intercepting panics, unhandled rejections, and unknown error types and wrapping them as `CanonicalError::internal(...)`) depends on the foundation phase and is deferred per PRD §4.2.

#### Resource Error Macro

- [ ] `p1` - **ID**: `cpt-cf-errors-component-resource-error-macro`

**Responsibility scope**:

The `#[resource_error("gts.cf.core.users.user.v1~")] struct UserResourceError;` attribute macro on a unit struct generates 13 associated functions (all categories except `internal`, `service_unavailable`, and `unauthenticated`). Each generated function returns a `ResourceErrorBuilder` that:
1. Pre-sets `resource_type` to the GTS identifier (e.g., `"gts.cf.core.users.user.v1~"`)
2. Accepts a `detail` string describing the error occurrence
3. For `not_found`, `already_exists`, and `data_loss`: requires `.with_resource(name)` before `.create()` to set the `resource_name`

**Responsibility boundaries**:

The macro is a code generator. It does not add new categories or context types. It does not perform any runtime logic beyond delegation to `CanonicalError` constructors.

#### Architecture Lint Rules

- [ ] `p1` - **ID**: `cpt-cf-errors-component-architecture-lint-rules`

**Responsibility scope**:

A set of architecture lint rules (via `cargo gears lint`) that enforce canonical error construction patterns at compile time. The rules detect and reject code that bypasses the canonical error system — e.g., constructing `Problem` directly, returning raw HTTP error responses, or using legacy error patterns (`Problem::new()`, `ErrDef`, `declare_errors!`, `ErrorCode`).

**Rules**:
1. **No direct `Problem` construction** — all `Problem` instances must originate from `CanonicalError` via the `From` impl
2. **No legacy error patterns** — usage of `Problem::new()`, `ErrDef`, `declare_errors!`, or `ErrorCode` is flagged
3. **No raw status-code error responses** — handlers must return `CanonicalError`, not ad-hoc HTTP error responses

**Responsibility boundaries**:

Architecture lint rules are static analysis only. They do not modify code, do not run at runtime, and do not define new error categories or context types.

##### Related components (by ID)

- `cpt-cf-errors-component-canonical-error` — the rules enforce that all errors flow through this type

### 3.3 API Contracts

#### RFC 9457 Problem Wire Format

- [ ] `p1` - **ID**: `cpt-cf-errors-interface-problem-wire-format`

**Technology**: JSON (`application/problem+json`)

Every REST error response follows this structure:

| Field | Source | Part | Description |
|-------|--------|------|-------------|
| `type` | GTS URI from category | **Contract** | Error type URI (e.g., `gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~`) |
| `title` | Static per category | **Contract** | Human-readable summary (e.g., "Not Found") |
| `status` | HTTP status from mapping | **Contract** | HTTP status code as integer |
| `detail` | `CanonicalError.detail` | Variable | Human-readable explanation of this occurrence |
| `instance` | Request URI path | Variable | URI identifying this specific occurrence |
| `trace_id` | Request context | Variable | W3C trace ID for correlation |
| `context` | Serialized context type | Contract schema / Variable values | Category-specific structured details |

**Base Error Schema**

The base error schema defines the common structure for all error categories.

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "gts://gts.cf.core.errors.err.v1~",
  "type": "object",
  "required": ["type", "title", "status", "detail", "trace_id", "context"],
  "properties": {
    "type": {
      "type": "string",
      "description": "GTS type identifier for the error category"
    },
    "title": {
      "type": "string",
      "description": "Human-readable error category title"
    },
    "status": {
      "type": "integer",
      "description": "HTTP status code"
    },
    "detail": {
      "type": "string",
      "description": "Human-readable explanation of this error occurrence"
    },
    "trace_id": {
      "type": "string",
      "description": "W3C trace ID for request correlation, injected by error middleware"
    },
    "context": {
      "type": "object",
      "description": "Category-specific structured error details"
    }
  }
}
```

**Production response example**:

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
  "title": "Not Found",
  "status": 404,
  "detail": "User not found",
  "instance": "/api/v1/users/user-123",
  "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736",
  "context": {
    "resource_type": "gts.cf.core.users.user.v1~",
    "resource_name": "user-123"
  }
}
```

**Rust definition**:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Problem {
    #[serde(rename = "type")]
    pub problem_type: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    pub context: serde_json::Value,
}
```

**Conversion functions**:

```rust
impl Problem {
    pub fn from_error(err: CanonicalError) -> Self;
}

impl From<CanonicalError> for Problem {
    fn from(err: CanonicalError) -> Self {
        Problem::from_error(err)
    }
}
```

#### Round-Trip Deserialization

- [ ] `p2` - **ID**: `cpt-cf-errors-interface-problem-roundtrip`

**Technology**: `TryFrom<Problem> for CanonicalError`

SDK clients deserialize Problem responses back into `CanonicalError`, enabling transparent error propagation across gear boundaries.

```rust
impl TryFrom<Problem> for CanonicalError {
    type Error = ProblemConversionError;
    fn try_from(problem: Problem) -> Result<Self, Self::Error>;
}
```

The GTS type URI is matched against the known set of 16 GTS identifiers to dispatch to the correct variant.

### 3.4 Construction Paths

Two entry points converge into the same `CanonicalError` type. These are the **only** construction paths — direct variant construction and internal constructors are inaccessible from outside the crate (enforced by `#[non_exhaustive]` on all variants and `pub(crate)` on internal constructors):

```text
Resource-scoped construction              Non-resource construction
─────────────────────────────             ─────────────────────────
#[resource_error("gts...")]               CanonicalError::internal(detail).create()
struct R;
UserResourceError::not_found(detail)      CanonicalError::service_unavailable().create()
  .with_resource(id).create()             CanonicalError::unauthenticated()
UserResourceError::permission_denied(d)     .with_reason(r).create()
  .create()                                           │
        │                                             │
        │  resource_type auto-tagged by macro         │  resource_type = None
        └─────────────────────┬───────────────────────┘
                              │
                              v
                     ┌─────────────────┐
                     │  CanonicalError │
                     │  (16 variants)  │  #[non_exhaustive]
                     └────────┬────────┘
                              │
                ┌─────────────┼─────────────┐
                v             v             v
           Problem(REST)   Status(gRPC)  Event(SSE)
           RFC 9457        (future)      (future)
```

**Direct canonical error instantiation**:

```rust
use toolkit_canonical_errors::resource_error;

#[resource_error("gts.cf.core.users.user.v1~")]
struct UserResourceError;

let err = UserResourceError::invalid_argument()
    .with_field_violation("email", "must be a valid email address", "INVALID_FORMAT")
    .create();
```

**Resource-scoped error instantiation**:

```rust
use toolkit_canonical_errors::resource_error;

#[resource_error("gts.cf.core.users.user.v1~")]
struct UserResourceError;

let err = UserResourceError::not_found("User not found")
    .with_resource("user-123")
    .create();
```

### 3.5 GTS Registration

Each error category and each context type is registered in the GTS Types Registry as a base type. The `GtsSchema` trait generates JSON Schema from the Rust type definitions, ensuring the schema and code are always in sync.

**Error category GTS identifiers** are defined as `const` values returned by `CanonicalError::gts_type()`. Each is a single string ending in `.v1~`:

```rust
pub fn gts_type(&self) -> &'static str {
    match self {
        Self::Cancelled { .. }        => "gts.cf.core.errors.err.v1~cf.core.err.cancelled.v1~",
        Self::NotFound { .. }         => "gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
        Self::PermissionDenied { .. } => "gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
        Self::Internal { .. }         => "gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
        // ... all 16 variants, each a single &'static str
    }
}
```

### 3.6 Internal Details Logging

Internal diagnostic information (stack traces, connection strings, file paths) is never included in the error response. Instead, the middleware logs error details server-side at `WARN` or `ERROR` level with the `trace_id` for correlation. Operators use the `trace_id` from the response to look up the corresponding log entries.

### 3.7 Interactions & Sequences

#### Error Construction → Wire Response

- [ ] `p1` - **ID**: `cpt-cf-errors-seq-error-to-wire`

```text
Handler                CanonicalError          Problem              Client
  │                        │                      │                   │
  │  ::not_found(d)       │                      │                   │
  │  .with_resource(id)   │                      │                   │
  │  .create()            │                      │                   │
  ├───────────────────────>│                      │                   │
  │                        │                      │                   │
  │  Err(canonical_error)  │                      │                   │
  ├────────────────────────┤                      │                   │
  │                        │                      │                   │
  │              Middleware: From<CanonicalError> │                   │
  │                        ├─────────────────────>│                   │
  │                        │  set trace_id        │                   │
  │                        │  set instance        │                   │
  │                        │                      │                   │
  │                        │                      │  application/     │
  │                        │                      │  problem+json     │
  │                        │                      ├──────────────────>│
```

1. Handler constructs `CanonicalError` via builder + `.create()` or `#[resource_error]` macro builder + `.create()`
2. Handler returns `Err(canonical_error)` from the handler function
3. Error middleware catches the error, calls `Problem::from_error()`
4. Middleware sets `trace_id` from span context, `instance` from request URI
5. Middleware logs error details server-side at WARN/ERROR with `trace_id` for correlation
6. Middleware returns `application/problem+json` response to client

#### Trace ID Injection

The `trace_id` and `instance` fields are **not** set by handler code. They are injected automatically by the error middleware layer when converting `CanonicalError` to `Problem`.

**How trace_id is injected**:

1. **Tracing span extraction**: The middleware extracts the trace ID from incoming request headers (`x-trace-id`, `x-request-id`, `traceparent`). If no W3C trace ID is available, the current span ID may be used as a temporary fallback until the W3C extraction workstream is completed.
2. **Problem enrichment**: After calling `Problem::from_error()`, the middleware sets `trace_id` and `instance` before serializing the response
3. **Logging correlation**: The same `trace_id` is used when logging error details at WARN/ERROR level

**Middleware implementation example**:

```rust
use cf_toolkit_errors::{CanonicalError, Problem};
use axum::http::Uri;

// In error middleware layer:
async fn handle_error(err: CanonicalError, uri: &Uri, trace_id: Option<String>) -> Problem {
    let mut problem = Problem::from_error(err);

    // Inject trace_id from span or request headers
    problem.trace_id = trace_id.or_else(|| {
        tracing::Span::current()
            .id()
            // Temporary fallback only; replace with W3C trace-id extraction.
            .map(|id| format!("{:?}", id))
    });

    // Inject instance from request URI
    problem.instance = Some(uri.path().to_string());

    problem
}
```

**Handler code does NOT set trace_id**:

```rust
// ✅ Correct - handler returns CanonicalError without trace_id
async fn get_user(Path(id): Path<String>) -> Result<Json<User>, CanonicalError> {
    let user = db.find_user(&id)
        .await?
        .ok_or_else(|| UserResourceError::not_found("User not found")
            .with_resource(&id)
            .create())?;
    Ok(Json(user))
}
// Middleware automatically adds trace_id and instance when converting to Problem
```

### 3.8 Context Type Extensibility (`extra` field)

> **Out of scope**: Error extensibility rules are out of scope for the current phase (see PRD §4.2). This section documents the reserved extension point for future phases only. The `extra` field MUST NOT be populated by any p1 code.

In future phases, every context type will carry an optional `extra: Option<serde_json::Value>` field. In **p1 (current)** this field does not exist in the struct definitions — it is reserved for future use and will be added when the extensibility rules are implemented.

**Purpose**: `extra` provides an open-ended extension point for error categories. Rather than extending the 16 base categories with new fields, callers can attach category-specific structured data without breaking the base schema.

**p3+ — Derived GTS types**: Future versions may allow a handler to attach a *derived* GTS type identifier to an error, effectively sub-typing the error for a specific domain. The GTS type chain expresses this derivation:

```text
gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~cf.scripting._.invalid_script_format.v1~
```

The innermost segment (`cf.scripting._.invalid_script_format.v1~`) declares its own `extra` schema — e.g., `{ "script_line": 42, "expected_token": ";" }` — while the parent segments remain fully backward-compatible. A client that understands only the base `invalid_argument` type safely ignores `extra`; a client that recognises the innermost type can interpret it fully.

Constraints:
- `extra` is always a JSON **object** or absent — never a scalar or array
- Base context types never populate `extra` directly (that is the derived type's responsibility)
- The derived GTS type string MUST end with `~` and MUST be registered in the Types Registry

### 3.9 Database schemas & tables

Not applicable. Errors are transient in-memory values. No persistent storage.

### 3.10 Contract Enforcement Tiers

| Tier | When | Mechanism | What It Catches |
|------|------|-----------|-----------------|
| 1. Compile-time | `cargo build` | Typed enum variants, exhaustive `match`, `#[resource_error]` macro, `GtsSchema` const, architecture lint rules (via `cargo gears lint`), `#[non_exhaustive]` on enum + variants, `pub(crate)` internal constructors | Wrong context type, missing match arm, GTS typos, direct `Problem` construction, legacy error patterns, direct variant construction, bypassing builder API |
| 2. Test-time | `cargo test` | Showcase tests with `assert_eq!` on full Problem JSON per category; JSON Schema equality assertions per context type | Field renames, default message changes, status code changes, schema drift |
| 3. CI-time | PR merge gate | `cargo-semver-checks` on `cf-gears-toolkit-errors`; schema file diffing; snapshot CI gate | Removed types, changed signatures, schema evolution |
| 4. Design-time | Architecture | Single `Problem` conversion point; dedicated context constructors; `GtsSchema` generates schemas from types | Ad-hoc JSON construction, missing required fields, schema/code divergence |

## 4. Category Reference

Each section below defines one canonical error category: GTS ID, HTTP mapping, context type, constructor, JSON wire example, and similar categories.

All variants share the same structure: `{ ctx: ContextType, detail: String, resource_type: Option<String>, resource_name: Option<String> }` (the `Internal` variant carries only `ctx` and `detail`). Context schemas are documented where first introduced; subsequent categories using the same context type reference back.


### 4.1 `cancelled`

- [ ] `p1` - **ID**: `cpt-cf-errors-design-cancelled`

**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.cancelled.v1~`
**HTTP Status**: 499
**Title**: "Cancelled"
**Context Type**: `Cancelled`
**Use When**: The client cancelled the request before the server finished processing.

→ [Full reference](./categories/01-cancelled.md)

### 4.2 `unknown`

- [ ] `p1` - **ID**: `cpt-cf-errors-design-unknown`

**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.unknown.v1~`
**HTTP Status**: 500
**Title**: "Unknown"
**Context Type**: `Unknown`
**Use When**: An error occurred that does not match any other canonical category.

→ [Full reference](./categories/02-unknown.md)

### 4.3 `invalid_argument`

- [ ] `p1` - **ID**: `cpt-cf-errors-design-invalid-argument`

**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~`
**HTTP Status**: 400
**Title**: "Invalid Argument"
**Context Type**: `InvalidArgument`
**Use When**: The client sent an invalid request — malformed fields, bad format, or constraint violations.

→ [Full reference](./categories/03-invalid-argument.md)

### 4.4 `deadline_exceeded`

- [ ] `p1` - **ID**: `cpt-cf-errors-design-deadline-exceeded`

**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.deadline_exceeded.v1~`
**HTTP Status**: 504
**Title**: "Deadline Exceeded"
**Context Type**: `DeadlineExceeded`
**Use When**: The server did not complete the operation within the allowed time.

→ [Full reference](./categories/04-deadline-exceeded.md)

### 4.5 `not_found`

- [ ] `p1` - **ID**: `cpt-cf-errors-design-not-found`

**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~`
**HTTP Status**: 404
**Title**: "Not Found"
**Context Type**: `NotFound`
**Use When**: The requested resource does not exist or was filtered out by access controls.

→ [Full reference](./categories/05-not-found.md)

### 4.6 `already_exists`

- [ ] `p1` - **ID**: `cpt-cf-errors-design-already-exists`

**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.already_exists.v1~`
**HTTP Status**: 409
**Title**: "Already Exists"
**Context Type**: `AlreadyExists`
**Use When**: The resource the client tried to create already exists.

→ [Full reference](./categories/06-already-exists.md)

### 4.7 `permission_denied`

- [ ] `p1` - **ID**: `cpt-cf-errors-design-permission-denied`

**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~`
**HTTP Status**: 403
**Title**: "Permission Denied"
**Context Type**: `PermissionDenied`
**Use When**: The caller is authenticated but does not have permission for the requested operation.

→ [Full reference](./categories/07-permission-denied.md)

### 4.8 `resource_exhausted`

- [ ] `p1` - **ID**: `cpt-cf-errors-design-resource-exhausted`

**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~`
**HTTP Status**: 429
**Title**: "Resource Exhausted"
**Context Type**: `ResourceExhausted`
**Use When**: A quota or rate limit was exceeded.

→ [Full reference](./categories/08-resource-exhausted.md)

### 4.9 `failed_precondition`

- [ ] `p1` - **ID**: `cpt-cf-errors-design-failed-precondition`

**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~`
**HTTP Status**: 400
**Title**: "Failed Precondition"
**Context Type**: `FailedPrecondition`
**Use When**: The request is valid but the system is not in the required state to perform it.

→ [Full reference](./categories/09-failed-precondition.md)

### 4.10 `aborted`

- [ ] `p1` - **ID**: `cpt-cf-errors-design-aborted`

**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.aborted.v1~`
**HTTP Status**: 409
**Title**: "Aborted"
**Context Type**: `Aborted`
**Use When**: The operation was aborted due to a concurrency conflict. The client can retry.

→ [Full reference](./categories/10-aborted.md)

### 4.11 `out_of_range`

- [ ] `p1` - **ID**: `cpt-cf-errors-design-out-of-range`

**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.out_of_range.v1~`
**HTTP Status**: 400
**Title**: "Out of Range"
**Context Type**: `OutOfRange`
**Use When**: A value is syntactically valid but outside the acceptable range.

→ [Full reference](./categories/11-out-of-range.md)

### 4.12 `unimplemented`

- [ ] `p1` - **ID**: `cpt-cf-errors-design-unimplemented`

**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.unimplemented.v1~`
**HTTP Status**: 501
**Title**: "Unimplemented"
**Context Type**: `Unimplemented`
**Use When**: The requested operation is recognized but not implemented.

→ [Full reference](./categories/12-unimplemented.md)

### 4.13 `internal`

- [ ] `p1` - **ID**: `cpt-cf-errors-design-internal`

**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.internal.v1~`
**HTTP Status**: 500
**Title**: "Internal"
**Context Type**: `Internal`
**Use When**: A known infrastructure failure occurred (database error, serialization bug, etc.).

→ [Full reference](./categories/13-internal.md)

### 4.14 `service_unavailable`

- [ ] `p1` - **ID**: `cpt-cf-errors-design-service-unavailable`

**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~`
**HTTP Status**: 503
**Title**: "Service Unavailable"
**Context Type**: `ServiceUnavailable`
**Use When**: The service is temporarily unavailable.

→ [Full reference](./categories/14-service-unavailable.md)

### 4.15 `data_loss`

- [ ] `p1` - **ID**: `cpt-cf-errors-design-data-loss`

**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.data_loss.v1~`
**HTTP Status**: 500
**Title**: "Data Loss"
**Context Type**: `DataLoss`
**Use When**: Unrecoverable data loss or corruption detected.

→ [Full reference](./categories/15-data-loss.md)

### 4.16 `unauthenticated`

- [ ] `p1` - **ID**: `cpt-cf-errors-design-unauthenticated`

**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~`
**HTTP Status**: 401
**Title**: "Unauthenticated"
**Context Type**: `Unauthenticated`
**Use When**: The request does not have valid authentication credentials.

→ [Full reference](./categories/16-unauthenticated.md)

## 5. Non-Applicable Checklist Areas

- **Performance Architecture (PERF)**: Not applicable. Error construction is O(1) enum + struct allocation. No caching, pooling, or scaling concerns specific to the error system.
- **Data Architecture (DATA)**: Not applicable. Errors are transient; no persistent storage.
- **Operations (OPS)**: Not applicable. Error handling does not introduce deployment topology, infrastructure, or monitoring requirements beyond what the observability stack already provides.
- **Compliance (COMPL)**: Flexible fields (`ResourceInfo.resource_name`, `FieldViolation.field`) may carry user-provided identifiers. Gears MUST apply data minimization when populating these fields. The `cpt-cf-errors-constraint-no-internal-details` constraint prevents stack traces and internal detail leakage in production, but does not address PII in context fields. PII handling in error responses follows the platform's data classification policy.
- **Usability (UX)**: Not applicable. This design covers the API error wire format, not user-facing error display.

## 6. Traceability

- **PRD**: [PRD.md](./PRD.md)
- **ADRs**: [ADR/](./ADR/)
- **Existing implementation**: [`libs/toolkit-canonical-errors/src/problem.rs`](../../../libs/toolkit-canonical-errors/src/problem.rs)
- **Supersedes**: PR #290 (`docs/unified-error-system/`)
