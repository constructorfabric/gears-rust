---
status: accepted
date: 2026-05-20
---

# SDK Error Surface: Canonical at the Boundary, Opt-in Typed Projection

**ID**: `cpt-cf-errors-adr-sdk-canonical-projection`

## Context and Problem Statement

Gears ship per-gear SDK crates (`oagw-sdk`, `credstore-sdk`, `tenant-resolver-sdk`, …) that ClientHub consumers use to call other gears in-process. Each SDK historically exposed a hand-rolled error enum (`ServiceGatewayError`, `CredStoreError`, …) that the impl crate populated through a parallel `From<DomainError> for SdkError` ladder, distinct from the `From<DomainError> for CanonicalError` ladder used on the REST boundary.

This caused three concrete problems:

1. **Contract is hostage to internal taxonomy.** Adding a new domain variant means an SDK contract break.
2. **Two parallel boundary mappings.** Each impl crate maintains `From<DomainError> for SdkError` for ClientHub consumers *and* `From<DomainError> for CanonicalError` for REST. They classify the same variants, can disagree, and must update in lockstep.
3. **Consumers learn impl vocabulary.** Pattern matching on `ServiceGatewayError::PayloadTooLarge` couples the consumer to the impl crate's word for that concept.

A pure-alias approach (`pub use CanonicalError as XxxError`) fixes all three but pushes context-walking onto consumers who want sub-category dispatch. Putting a typed projection in the trait signature gives flat dispatch but makes the projection variant set part of the SDK contract — variant changes become breaking, and consumers using multiple SDKs see N different SDK-specific names for the same canonical category.

What's the right shape for the SDK error surface?

## Decision Drivers

* Single authoritative AIP-193 categorization — domain → canonical mapping must live in exactly one place per gear
* Information fidelity at the SDK boundary — projection loss should be a consumer-side opt-in, not a default
* Stable SDK contract — variant changes in the projection must not be SDK semver-major events
* Cross-SDK uniformity — consumers using multiple SDKs should see a uniform error type at trait boundaries
* Ergonomic typed dispatch when consumers want it — flat match arms, typed sub-enums for context discriminators
* Conformance with `cpt-cf-errors-adr-canonical-error-categories` — finite vocabulary, no gear-specific error categories
* `cpt-cf-errors-principle-single-error-gateway` — REST path goes through `CanonicalError` unchanged

## Decision Outcome

SDKs expose errors in **two layers**:

- **At the trait boundary**: `Result<_, CanonicalError>`. Every SDK trait returns the platform's canonical error type — no SDK-specific error type in the trait contract.
- **As an opt-in consumer convenience**: SDKs MAY ship a typed `SdkError` projection enum (with `From<CanonicalError>`) for consumers that want flat typed dispatch. Consumers project at the call site via `.map_err(SdkError::from)?`, or chain transparently through a `From<CanonicalError> for OwnError` impl.

The trait signature is uniform across all SDKs. The projection lives in the SDK as a typed view over canonical — not as the wire contract or the trait contract — and individual SDKs add it only when their consumers benefit from it. Future projection variant changes (adding, splitting, refining) are non-breaking on the trait contract; only consumers that opted into typed dispatch are affected, and only at sites that pattern-matched on the changed variants.

### Consequences

* The SDK trait returns `Result<_, CanonicalError>` for every fallible method. No SDK-specific error type appears in the trait signature.
* The impl crate keeps **only** `From<DomainError> for CanonicalError` in its REST error gear. Any parallel `From<DomainError> for SdkError` is deleted; the facade calls `.map_err(CanonicalError::from)` once per method.
* SDKs MAY ship a typed `SdkError` projection enum (with `From<CanonicalError>`) as a documented consumer convenience. The projection is **not** part of the trait contract:
   - Adding a variant is non-breaking
   - Refining a variant's payload is breaking only for consumers that destructure that variant
   - Removing a variant is breaking only for consumers that pattern-match on it
   - The trait signature is unaffected by any of these changes
* When an SDK ships a projection, it MUST also ship the typed sub-enum vocabulary (`field::TargetHostCode`, `reason::auth::FailureReason`, `gts::Resource`, …) co-located with the wire-string constants they project from, each with `pub fn from_wire` and `pub fn as_wire` helpers. Round-trip `Problem` tests pin every wire-string constant to its expected JSON path.
* When an SDK ships a projection, it MUST include a catch-all `Other { canonical: CanonicalError }` variant so the conversion is infallible and forward-compatible (new canonical categories surface with full fidelity).
* The impl crate references the SDK constants at every construction site that previously hardcoded a wire literal. The `#[resource_error("...")]` proc-macro literals are the one exception (proc macros cannot resolve constants); the round-trip Problem tests pin them.
* SDK error types must not carry transport fields (`instance`, `trace_id`) — those belong to the `Problem` envelope.
* Trait method rustdoc MUST cross-link to the SDK's projection type (when one exists) so the typed surface is discoverable.

### Confirmation

The OAGW SDK demonstrates the pattern end-to-end:

(a) `ServiceGatewayClientV1` trait methods return `Result<_, CanonicalError>`.

(b) `oagw-sdk` ships `ServiceGatewayError` — a 14-variant projection with `From<CanonicalError>` — as a consumer convenience. Variants: `RateLimited`, `Timeout`, `Unavailable`, `AuthFailed`, `PermissionDenied`, `PayloadTooLarge`, `InvalidTargetHost`, `Validation`, `NotFound`, `AlreadyExists`, `FailedPrecondition`, `Aborted`, `Internal`, `Other { canonical: CanonicalError }`.

(c) Typed sub-enums are co-located with their wire-string constants:
- `field::TargetHostCode` next to `MISSING_TARGET_HOST` / `INVALID_TARGET_HOST` / `UNKNOWN_TARGET_HOST`
- `reason::auth::FailureReason` next to `PLUGIN_NOT_FOUND` / `PLUGIN_FAILED` / `PLUGIN_INTERNAL`
- `reason::permission::DenialReason` next to `AUTHZ_DENIED` / `TENANT_*` / `CORS_*`
- `gts::Resource` next to the six `*_SCHEMA` constants

(d) The impl crate's parallel `From<DomainError> for ServiceGatewayError` ladder is deleted (~110 lines + 4 tests). The single AIP-193 mapping is `From<DomainError> for CanonicalError` in `oagw/src/api/rest/error.rs`.

(e) Round-trip `Problem` tests in `oagw-sdk/src/error.rs::tests` pin all 26 `field::*` constants, 8 `reason::*` constants, 1 `quota::*` constant, and 6 `gts::*` constants to their wire JSON paths.

(f) Mini-chat (OAGW's only current consumer) defines `From<CanonicalError> for LlmProviderError` that chains through `ServiceGatewayError::from(err).into()` internally, so every call site stays as plain `?` propagation without `.map_err` boilerplate.

(g) Out-of-process consumers can use `TryFrom<Problem> for CanonicalError` (in `toolkit-canonical-errors`) to reconstruct typed errors from wire bytes. The full chain is `Problem JSON → Problem → CanonicalError → (optional) SdkError`.

(h) Workspace `cargo test` passes (~1,800 tests across `oagw-sdk`, `oagw`, `mini-chat`, `toolkit-canonical-errors`).

(i) **`simple-user-settings-sdk` is the Pattern 1 (no-projection) reference**: trait returns `Result<_, CanonicalError>`, no `SettingsError` enum, no `From<DomainError> for SettingsError` ladder. Consumers either propagate via `?` or match on `CanonicalError` categories directly. Demonstrates the lower bound of the pattern — when the projection layer would add cost without dispatch value.

## The Pattern

### Trait signature — canonical at the boundary

```rust
#[async_trait]
pub trait XxxClientV1: Send + Sync {
    async fn do_thing(&self, ctx: SecurityContext, req: Request)
        -> Result<Response, CanonicalError>;
    // every fallible method returns CanonicalError
}
```

### Impl-side facade — single `.map_err`

```rust
async fn do_thing(&self, ctx: SecurityContext, req: Request)
    -> Result<Response, CanonicalError>
{
    self.inner.do_thing(&ctx, req)
        .await
        .map_err(CanonicalError::from)
}
```

The single AIP-193 ladder lives in `{impl}/src/api/rest/error.rs`:

```rust
impl From<DomainError> for CanonicalError {
    fn from(err: DomainError) -> Self {
        match err {
            DomainError::UserNotFound { id } => UserResourceError::not_found(/* … */)
                .with_resource(id.to_string())
                .create(),
            // … one arm per domain variant, AIP-193 mapping
        }
    }
}
```

### Optional projection — typed view over canonical

When an SDK's consumers benefit from flat typed dispatch, the SDK ships a `From<CanonicalError>`-driven enum **alongside** the trait — not in the trait signature:

```rust
// {sdk}/src/error.rs

use toolkit_canonical_errors::CanonicalError;
use crate::field::TargetHostCode;
use crate::gts::Resource;
use crate::reason::auth::FailureReason as AuthFailureReason;
use crate::reason::permission::DenialReason as PermissionDenialReason;

#[derive(Debug, Clone, Error)]
pub enum ServiceGatewayError {
    #[error("rate limit exceeded")]
    RateLimited { retry_after_secs: Option<u64> },

    #[error("request timed out")]
    Timeout,

    #[error("service unavailable")]
    Unavailable { retry_after_secs: Option<u64> },

    #[error("authentication failed [{reason}]: {detail}")]
    AuthFailed { reason: AuthFailureReason, detail: String },

    // … one variant per dispatch case, driven by consumer needs …

    /// Catch-all for canonical categories the SDK does not model.
    /// Preserves full canonical context for inspection / forward-compat.
    #[error("[{}] {}", canonical.gts_type(), canonical.detail())]
    Other { canonical: CanonicalError },
}

impl From<CanonicalError> for ServiceGatewayError {
    fn from(err: CanonicalError) -> Self {
        match &err {
            CanonicalError::ResourceExhausted { .. } => Self::RateLimited {
                retry_after_secs: err.retry_after_seconds(),
            },
            CanonicalError::DeadlineExceeded { .. } => Self::Timeout,
            CanonicalError::ServiceUnavailable { .. } => Self::Unavailable {
                retry_after_secs: err.retry_after_seconds(),
            },
            // … one arm per canonical variant the SDK models, plus
            // `_ => Self::Other { canonical: err.clone() }` catch-all
        }
    }
}
```

### Co-located vocabulary

Wire-string constants and their typed sub-enums live together. Example for `Unauthenticated.ctx.reason`:

```rust
// {sdk}/src/reason.rs
pub mod auth {
    pub const PLUGIN_NOT_FOUND: &str = "AUTH_PLUGIN_NOT_FOUND";
    pub const PLUGIN_FAILED:    &str = "AUTH_PLUGIN_FAILED";
    pub const PLUGIN_INTERNAL:  &str = "AUTH_PLUGIN_INTERNAL";

    /// Typed view of the wire `reason` strings above.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum FailureReason {
        PluginNotFound,
        PluginFailed,
        PluginInternal,
        Unknown(String),
    }

    impl FailureReason {
        pub fn from_wire(s: Option<&str>) -> Self { /* … */ }
        pub fn as_wire(&self) -> &str { /* … */ }
    }
}
```

Same pattern in `field.rs` (`TargetHostCode` etc.), `gts.rs` (`Resource`), `reason::permission` (`DenialReason`).

### Round-trip tests pin the vocabulary

```rust
// {sdk}/src/error.rs::tests
#[test]
fn auth_reason_constants_round_trip() {
    for r in [reason::auth::PLUGIN_NOT_FOUND, /* … */] {
        let err = CanonicalError::unauthenticated().with_reason(r).create();
        let json = serde_json::to_value(&Problem::from(err)).unwrap();
        assert_eq!(json["context"]["reason"], r);
    }
}
```

The impl crate references the SDK constants at every construction site that previously hardcoded a wire literal. The `#[resource_error("…")]` proc-macro literals are the exception (macros cannot resolve constants); the round-trip tests pin them.

## Consumer Integration

Three patterns, all valid:

### Pattern 1 — pure propagation (no projection)

```rust
let settings = settings_client.get_settings(&ctx).await?;  // ? propagates CanonicalError
```

Caller's own error type implements `From<CanonicalError>`. The projection isn't used — either because the SDK ships none (e.g. `simple-user-settings-sdk`) or because the consumer chose to bypass it. Right choice when the consumer only propagates or dispatches on broad canonical category.

### Pattern 2 — explicit projection at the call site

```rust
let upstream = gateway.get_upstream(&ctx, id).await
    .map_err(ServiceGatewayError::from)?;

match err {
    ServiceGatewayError::RateLimited { retry_after_secs } => /* backoff */,
    ServiceGatewayError::Timeout                          => /* retry */,
    ServiceGatewayError::AuthFailed {
        reason: AuthFailureReason::PluginInternal, ..
    } => /* gateway-side auth broken — transient */,
    _ => /* fallback */,
}
```

Explicit about projecting. Caller's error type implements `From<ServiceGatewayError>`. Right choice for one-off typed-dispatch sites scattered across a codebase that otherwise propagates canonical.

### Pattern 3 — transparent chaining via `From<CanonicalError> for OwnError`

```rust
// Define once in the consumer crate:
impl From<CanonicalError> for OwnConsumerError {
    fn from(err: CanonicalError) -> Self {
        // Route through the SDK's typed projection internally.
        ServiceGatewayError::from(err).into()
    }
}

// Then every call site stays plain `?`:
let upstream = gateway.get_upstream(&ctx, id).await?;
```

Eliminates per-call-site boilerplate while keeping the typed dispatch surface. **Recommended** for consumers that always want typed dispatch. Mini-chat uses this pattern.

### Reverse direction (consumer wraps SDK failure in its own canonical)

Often a consumer wants to catch an SDK failure and re-emit it as a canonical error in its own resource scope (e.g. `account-management` catching an OAGW call and re-emitting it as a tenant-management failure). The canonical-at-boundary contract makes this trivial — the consumer already has `CanonicalError` from the `?`:

```rust
let upstream = gateway.get_upstream(&ctx, id).await
    .map_err(|canonical| {
        AccountMgmtResourceError::failed_precondition()
            .with_precondition_violation(/* … */)
            .create()
    })?;
```

No reverse `From<SdkError> for CanonicalError` impl is needed — canonical is already at the boundary.

### Cross-process round-trip

`TryFrom<Problem> for CanonicalError` (in `toolkit-canonical-errors`) closes the out-of-process loop. An HTTP/REST consumer's full chain:

```
network bytes
  → serde_json::from_slice → Problem
  → CanonicalError::try_from(problem) → CanonicalError
  → (optional) ServiceGatewayError::from(canonical) → ServiceGatewayError
```

The first two hops are mandatory and lossless (modulo wire-strip fields like `Internal.description` which are `#[serde(skip)]` by design — production wire never carries the diagnostic). The third hop is opt-in.

## When to Ship a Projection

The trait contract is fixed (`CanonicalError`), but a projection can add value in two distinct ways:

* **Sub-category dispatch.** When consumers dispatch on dispositions the canonical envelope encodes as wire strings rather than enum tags — auth `FailureReason`, target-host `code`, field-violation `reason`. The projection fans these strings into typed sub-enums so the call site is a flat `match` rather than nested string compares. `oagw-sdk` ships 14 variants for this reason: each is a distinct disposition consumers actually `match` on.

* **Variant narrowing.** When the SDK only emits a handful of the 16 canonical categories, a 2- or 3-variant projection enum (plus the mandatory `Other { canonical }` catch-all) documents the emission set at the type level. Consumers matching on `CanonicalError` directly cannot tell which variants the SDK will produce; they either write a wildcard `_` arm for unreachable cases or over-specify the match. A narrowed projection lets consumers handle exactly the cases they care about, without defensive coverage for the 13 categories that can never arrive.

Skip the projection when consumers do not benefit from either case — typically when they propagate `?` without inspection, or when canonical category tags are already the granularity they dispatch on. `simple-user-settings-sdk` is the Pattern 1 reference: its three CRUD methods emit `NotFound` / `InvalidArgument` / `Internal`, consumers propagate the canonical error rather than `match`ing on it, and neither sub-category fanning nor variant narrowing would change a call site. The trait still returns `CanonicalError` — only the optional projection layer is omitted.

## Non-Canonical Methods

The canonical contract applies to **trait methods whose failures cross or could cross a wire boundary** — i.e., the methods registered in `ClientHub` that another gear might call in-process today and dial over HTTP tomorrow.

SDK methods that exist purely for in-process client-side ergonomics (decoder helpers, stream parsers, builder fluents) are not bound by the canonical contract. They MAY return gear-specific error types — typically a narrow `thiserror` enum — because those failures will never be projected, deserialized, or transported.

The OAGW SDK ships `StreamingError` (in `oagw-sdk/src/error.rs`) for SSE and WebSocket decode failures that surface inside the SDK's own client helpers but never traverse an OAGW response. `StreamingError` does not implement `From<CanonicalError>` and does not appear in the `ServiceGatewayClientV1` trait signature.

## Design Rules

1. **Trait signature returns `CanonicalError`** for every fallible method. Document this in the trait's top-level rustdoc and cross-link the optional projection.
2. **Impl-side facade calls `.map_err(CanonicalError::from)`** once per method. No parallel `From<DomainError> for SdkError` ladder.
3. **SDKs MAY ship a typed `SdkError` projection** as a consumer convenience. When shipped:
   - MUST be infallible (`From<CanonicalError>`, not `TryFrom`)
   - MUST include a catch-all `Other { canonical: CanonicalError }` variant for forward-compat
   - Variant set is driven by consumer dispatch — audit workspace `match` arms before designing
   - Variant names encode consumer intent, not impl vocabulary
4. **When a projection is shipped, co-locate typed sub-enums with wire-string constants**:
   - `gts.rs` — `*_SCHEMA` constants + typed `Resource` enum for `NotFound` / `AlreadyExists.ctx.resource_type`
   - `reason.rs` — submodules per canonical category, each holding both `pub const` reason values and a typed enum
   - `field.rs` — `pub const` constants for `InvalidArgument.ctx.field_violations[].reason`, plus typed sub-enums for families consumers dispatch on
   - `quota.rs` — `pub const` `subject` values for `ResourceExhausted.ctx.violations[].subject`
5. **Each typed sub-enum exposes `pub fn from_wire` and `pub fn as_wire`** (`from_wire` returns `Self` with an `Unknown(String)` catch-all when every wire value is meaningful, or `Option<Self>` when only a few wire strings are valid).
6. **Impl crate references the SDK constants** at every site that previously hardcoded a wire literal. The `#[resource_error("…")]` proc-macro literals are the exception; the round-trip tests pin them.
7. **Round-trip `Problem` tests** in `{sdk}/src/error.rs::tests` pin every wire-string constant to its expected JSON path.
8. **No transport fields in the projection**: `instance` and `trace_id` belong to the `Problem` envelope.
   A retry delay is not a transport field: a variant that carries one reads it from `CanonicalError::retry_after_seconds()`, the same value the HTTP `Retry-After` header carries (for `ResourceExhausted`, the largest per-violation hint), never from a context directly, so the SDK and REST callers back off for the same time.
9. **Document the projection** in the SDK's `error.rs` module rustdoc: state it is an opt-in convenience, show the three consumer integration patterns, include a consumer-dispatch reference table, and link to this ADR.

## Per-SDK Migration

Every SDK uses the same trait-contract migration. Whether to **also** ship a projection is a separate per-SDK decision based on consumer dispatch needs.

| SDK | `From<DomainError> for CanonicalError`? | Ship projection? | Effort |
|---|---|---|---|
| `simple-user-settings-sdk` | yes | no | **done** (Pattern 1 reference) |
| `nodes-registry-sdk` | yes | no | ~30 min |
| `mini-chat-sdk` | yes | no | ~1 hr (plugin-SPI enums separate, do not migrate) |
| `resource-group-sdk` | yes | re-evaluate if consumers want typed cycle/limit dispatch | ~1–2 hr |
| `file-parser` | yes | no | ~1 hr |
| `types-registry-sdk` | yes | likely yes (typed `ValidationError` payload) | ~half day |
| `oagw-sdk` | yes | **yes** | **done** (reference implementation) |
| `credstore-sdk` | **no** | TBD after consumer audit | ~1 day (author canonical boundary first) |
| `authn-resolver-sdk` | **no** | no | ~1 day |
| `tenant-resolver-sdk` | **no** | no | ~1 day |
| `authz-resolver-sdk` | **no** | likely yes (`EnforcerError::AccessDenied(AccessDenial::Pdp { reason })` — typed payload) | ~1–2 days |

For the four resolver SDKs at the bottom, the canonical boundary mapping is the real cost. The trait-contract switch is trivial once that lands.

## Known Gaps

1. **`Internal.description` / `Unknown.description` stripped on the wire** by `#[serde(skip)]` — production wire never carries the diagnostic. Documented in `problem.rs::TryFrom` rustdoc. This is intentional, not a defect; consumers needing the diagnostic must read it in-process via `CanonicalError::diagnostic()` before the wire hop.

## Conformance with Existing Docs

| Doc | Statement | Conformance |
|---|---|---|
| [`ADR 0001`](./0001-cpt-cf-adr-canonical-error-categories.md) §Consequences | *"Every gear must migrate its existing ad-hoc error types to one of the 16 canonical categories — no gear-specific error categories are allowed."* | **Verbatim.** Trait boundary is `CanonicalError`; the opt-in projection is consumer-side, not a wire or trait contract. |
| [`PRD §6`](../PRD.md) Acceptance Criteria | *"No error reaches API consumers outside the canonical vocabulary."* | **Verbatim.** Consumers receive `CanonicalError` at the trait boundary. |
| [`DESIGN §3.2`](../DESIGN.md) `principle-single-error-gateway` | *"Every REST error response is produced from a `CanonicalError` via `From<CanonicalError> for Problem`."* | **Verbatim.** REST path is unchanged. |
| [`DESIGN §3.3`](../DESIGN.md) `cpt-cf-errors-interface-problem-roundtrip` | *"SDK clients deserialize Problem responses back into `CanonicalError`."* | **Implemented.** `TryFrom<Problem> for CanonicalError` landed in `toolkit-canonical-errors`. |

No amendment to ADR 0001 is required — the chosen design preserves the literal text of every constraint above.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)
- **Related**: `cpt-cf-errors-adr-canonical-error-categories`, `cpt-cf-errors-adr-typed-enum-impl`, `cpt-cf-errors-interface-problem-roundtrip`

This decision directly addresses the following requirements:

* `cpt-cf-errors-fr-finite-vocabulary` — trait boundary is `CanonicalError`; the opt-in projection is a typed view (one-to-one when modelling the full canonical surface, narrowed when the SDK emits only a subset) with `Other { canonical }` preserving new canonical categories without an SDK break
* `cpt-cf-errors-fr-compile-time-safety` — `CanonicalError` is exhaustively matchable at the boundary; the projection adds flat typed dispatch for consumers that opt in
* `cpt-cf-errors-fr-single-line-construction` — impl crate emits `CanonicalError` directly via `From<DomainError>`; consumers propagate or project in a single expression
* `cpt-cf-errors-principle-single-error-gateway` — REST path is unchanged; the projection (when present) is a consumer-side translation at the dispatch site, not a wire-path alternative
* `cpt-cf-errors-interface-problem-roundtrip` — `TryFrom<Problem> for CanonicalError` enables the out-of-process consumer story
