# Feature: Domain Foundation & Error Model


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
  - [1.5 Out of Scope](#15-out-of-scope)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Constructor: `InvocationRecord::new`](#constructor-invocationrecordnew)
  - [Mapping: `ServerlessSdkError` → `RuntimeErrorCategory`](#mapping-serverlesssdkerror--runtimeerrorcategory)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Crate skeleton](#crate-skeleton)
  - [Dependency lockdown](#dependency-lockdown)
  - [Lint and doc policy](#lint-and-doc-policy)
  - [Shared value types](#shared-value-types)
  - [Error enum](#error-enum)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Applicable Concerns](#7-non-applicable-concerns)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-featstatus-domain-foundation-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-serverless-runtime-plugin-sdk-feature-domain-foundation`
## 1. Feature Context

### 1.1 Overview

Stand up the `serverless-runtime-sdk` crate skeleton, lock its dependencies, set the
crate-level lint and doc policies, and ship the shared value types together with the
`ServerlessSdkError` enum that every later feature consumes.

### 1.2 Purpose

This is the foundation feature: every other SDK feature depends on it for value types,
the error enum, and the crate-wide stability discipline. The work is declarative — types
and lint configuration — with no business logic of its own. It is the first thing built
because it has no upstream dependencies and unblocks all six remaining features.

**Requirements**: `cpt-cf-serverless-runtime-plugin-sdk-fr-error-model`,
`cpt-cf-serverless-runtime-plugin-sdk-nfr-no-engine-deps`,
`cpt-cf-serverless-runtime-plugin-sdk-nfr-no-unsafe`,
`cpt-cf-serverless-runtime-plugin-sdk-nfr-api-docs`

**Principles**: `cpt-cf-serverless-runtime-plugin-sdk-principle-impl-agnostic`,
`cpt-cf-serverless-runtime-plugin-sdk-principle-minimal-surface`

**Constraints**: `cpt-cf-serverless-runtime-plugin-sdk-constraint-no-engine-deps`,
`cpt-cf-serverless-runtime-plugin-sdk-constraint-stable-rust`,
`cpt-cf-serverless-runtime-plugin-sdk-constraint-trust-boundary`

### 1.3 Actors

| Actor | Role in Feature |
|---|---|
| `cpt-cf-serverless-runtime-plugin-sdk-actor-adapter-dev` | Consumes the value types and the error enum from inside their adapter crate. |
| `cpt-cf-serverless-runtime-plugin-sdk-actor-runtime-host` | Consumes the value types when receiving `InvocationIndexEvent` and when issuing invocations to plugins. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **ADRs**: `cpt-cf-serverless-runtime-plugin-sdk-adr-non-exhaustive-surface`
- **Dependencies**: None — foundation feature.

### 1.5 Out of Scope

- Handler / workflow trait declarations (Feature 2.2).
- Context and Environment surface (Feature 2.3).
- Trace instrumentation (Feature 2.4).
- `RuntimeAdapter` and `ServerlessRuntimeClient` traits (Features 2.5 and 2.6).
- Conformance test suite (Feature 2.7).

## 2. Actor Flows (CDSL)

Not applicable — the SDK is a library consumed at compile time. There is no runtime
actor flow originating in this feature. Adapter and host code that consumes the
foundation is covered by the actor flows of Features 2.2–2.7.

## 3. Processes / Business Logic (CDSL)

### Constructor: `InvocationRecord::new`

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-algo-domain-foundation-invocation-record-new`

**Input**: required fields (`invocation_id`, `function_id`, `function_version`,
`tenant_id`, `attempt_number`, `correlation_id`, `started_at`, `deadline`, `params`).

**Output**: `InvocationRecord` instance with all required fields set and optional fields
defaulted to `None`.

**Steps**:
1. [ ] - `p1` - Accept required fields as positional arguments - `inst-collect-required`
2. [ ] - `p1` - Construct the struct literal **inside** the crate (the type is `#[non_exhaustive]` outside) - `inst-construct`
3. [ ] - `p1` - **RETURN** the new `InvocationRecord` - `inst-return`

### Mapping: `ServerlessSdkError` → `RuntimeErrorCategory`

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-algo-domain-foundation-error-category-mapping`

**Input**: a `ServerlessSdkError` value (one of `InvalidInput`, `UserError`,
`Transient`, `Permanent`, `Internal`, `Unsupported { operation }`).

**Output**: the `RuntimeErrorCategory` variant the host uses for retry and
dead-letter routing decisions.

**Steps**:
1. [ ] - `p1` - **MATCH** the input variant exhaustively (with a `_` arm for forward compatibility) - `inst-match`
2. [ ] - `p1` - Map `InvalidInput` and `UserError` → `RuntimeErrorCategory::User` (non-retryable) - `inst-map-user`
3. [ ] - `p1` - Map `Transient` → `RuntimeErrorCategory::Transient` (retryable per `RetryPolicy`) - `inst-map-transient`
4. [ ] - `p1` - Map `Permanent` and `Unsupported { .. }` → `RuntimeErrorCategory::Permanent` (non-retryable) - `inst-map-permanent`
5. [ ] - `p1` - Map `Internal` → `RuntimeErrorCategory::Internal` (retryable per `RetryPolicy`, surfaces in audit) - `inst-map-internal`
6. [ ] - `p1` - **RETURN** the mapped category - `inst-return`

## 4. States (CDSL)

Not applicable — the foundation feature defines value types and an error enum. None of
the types has lifecycle state. Lifecycle state machines (where they exist) belong to
the features that own the relevant component, e.g., invocation status transitions live
in the host crate and are tested through the conformance suite in Feature 2.7.

## 5. Definitions of Done

### Crate skeleton

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-dod-domain-foundation-crate-skeleton`

The system **MUST** ship a `serverless-runtime-sdk` Cargo crate registered in the
workspace, with `lib.rs`, module split matching the eight components (`error`, `handler`,
`workflow`, `context`, `environment`, `trace`, `adapter`, `client`), and a `prelude`
re-exporting the stable surface.

**Constraints**: `cpt-cf-serverless-runtime-plugin-sdk-constraint-stable-rust`

**Touches**:
- Files: `Cargo.toml`, `src/lib.rs`, `src/error.rs`, `src/domain.rs`.

### Dependency lockdown

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-dod-domain-foundation-dep-lockdown`

The system **MUST** restrict the crate's dependency graph to `serde`, `serde_json`,
`thiserror`, `async-trait`, `tracing`, and `cf-credstore-sdk`. Any other crate is
forbidden and the workspace `deny.toml` enforces this.

**Constraints**: `cpt-cf-serverless-runtime-plugin-sdk-constraint-no-engine-deps`

**Touches**:
- Files: `Cargo.toml`, workspace `deny.toml`.

### Lint and doc policy

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-dod-domain-foundation-lint-doc-policy`

The system **MUST** declare `#![deny(missing_docs)]` at the crate root and inherit
`unsafe_code = "forbid"` from the workspace lint set. `cargo doc --no-deps` MUST emit
zero warnings.

**Constraints**: `cpt-cf-serverless-runtime-plugin-sdk-constraint-stable-rust`

**Touches**:
- Files: `src/lib.rs` (crate-level attributes), workspace `Cargo.toml` lint table.

### Shared value types

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-dod-domain-foundation-value-types`

The system **MUST** declare `InvocationRecord`, `CompensationContext`,
`RuntimeErrorCategory`, `RuntimeErrorPayload`, `RetryPolicy`, and `TimelineEventType`
as `#[non_exhaustive]` types with public `::new(...)` constructors (or builders) and
the field set documented in DESIGN §3.1. No engine-specific field is permitted.

**Constraints**: `cpt-cf-serverless-runtime-plugin-sdk-constraint-trust-boundary`

**Implements**:
- `cpt-cf-serverless-runtime-plugin-sdk-algo-domain-foundation-invocation-record-new`

**Touches**:
- Files: `src/domain.rs`.
- Entities: `InvocationRecord`, `CompensationContext`, `RuntimeErrorCategory`,
  `RuntimeErrorPayload`, `RetryPolicy`, `TimelineEventType`.

### Error enum

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-dod-domain-foundation-error-enum`

The system **MUST** declare `ServerlessSdkError` as a `#[non_exhaustive]` enum derived
from `thiserror`, covering at least the variants `InvalidInput`, `UserError`,
`Transient`, `Permanent`, `Internal`, and `Unsupported { operation: &'static str }`,
and **MUST** publish the canonical mapping to `RuntimeErrorCategory`.

**Implements**:
- `cpt-cf-serverless-runtime-plugin-sdk-algo-domain-foundation-error-category-mapping`

**Touches**:
- Files: `src/error.rs`.
- Entities: `ServerlessSdkError`.

## 6. Acceptance Criteria

- [ ] `cargo build -p serverless-runtime-sdk` succeeds with no warnings.
- [ ] `cargo doc -p serverless-runtime-sdk --no-deps` succeeds with no missing-doc warnings.
- [ ] `cargo deny check` rejects any dependency outside the documented allow-list.
- [ ] Workspace lint table fails any `unsafe` block in `serverless-runtime-sdk` source.
- [ ] Each shared value type and `ServerlessSdkError` carries `#[non_exhaustive]` —
  verified by a grep gate that fails on any `pub struct` / `pub enum` in `src/domain.rs`
  or `src/error.rs` missing the attribute.
- [ ] A small unit test confirms `ServerlessSdkError` variants map to the documented
  `RuntimeErrorCategory` values per the algorithm above.

## 7. Non-Applicable Concerns

- **API surface**: Not applicable — this feature exports value types and an error enum;
  no REST endpoint, gRPC service, or CLI command.
- **Database**: Not applicable — the SDK is a library; persistence is the host's concern.
- **State machines**: Not applicable — see §4.
- **Performance hot paths**: Not applicable at this stage — types are passive; the
  performance-sensitive surface lives in Feature 2.4 (trace) and Feature 2.5 (adapter).
- **Observability events**: Not applicable — emission belongs to Feature 2.4. This
  feature only defines `TimelineEventType` as a value type.
