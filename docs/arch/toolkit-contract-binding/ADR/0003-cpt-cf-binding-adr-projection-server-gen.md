---
status: proposed
date: 2026-06-02
---

# Extend Projection Traits to Generate Server Routes and Enforce Base-Projection Parity

**ID**: `cpt-cf-binding-adr-projection-server-gen`

## Table of Contents

1. [Context and Problem Statement](#context-and-problem-statement)
2. [Decision Drivers](#decision-drivers)
3. [Considered Options](#considered-options)
4. [Decision Outcome](#decision-outcome)
5. [Pros and Cons of the Options](#pros-and-cons-of-the-options)
6. [More Information](#more-information)

## Context and Problem Statement

ADR-0001 introduced a two-layer architecture: a clean **base trait** (zero transport annotations)
and a **projection trait** (`*Rest`, `*Grpc`) that carries transport annotations and is processed
by `#[modkit_rest_contract]`. The macro currently generates a REST client struct from the
projection. Two gaps remain unfixed.

**Gap 1 — no parity enforcement.** There is no compile-time guarantee that the projection trait
covers the same methods as the base with matching signatures. A rename or new parameter on the
base that is not propagated to the projection silently creates a stale client — the old method
signature is still compiled, the mismatch is invisible until a runtime call fails.

**Gap 2 — two independent OpenAPI sources.** Server-side handler code (`routes.rs`,
`handlers.rs`) is written by hand against `OperationBuilder`, duplicating every path, HTTP verb,
and schema already declared in the projection. The macro generates an IR used for the client;
`OperationBuilder` is called separately for the server. The two are not structurally connected and
will drift.

This ADR decides how to close both gaps while keeping the two-layer design intact.

## Decision Drivers

* **Keep base trait clean** — the base trait is the domain interface; it must carry zero transport
  annotations. Authors and IDE users reading the base trait see only the domain contract.
* **Scale across multiple transports** — a contract may have both a REST and a gRPC projection;
  if annotations for all transports were placed on the base, every method would accumulate a stack
  of per-transport attributes. Each projection must remain a self-contained, transport-specific file.
* **Single OpenAPI source of truth** — the same IR that drives REST client generation must also
  drive server route generation; the two must share one definition and cannot diverge.
* **Parity enforced at compile time** — a method missing from the projection, or with a mismatched
  signature, must be caught at `cargo check`, not at runtime.
* **Zero migration cost** — existing projection traits must continue to compile unchanged; new
  features are additive.
* **Preserve the escape hatch** — ADR-0001 and ADR-0002 promise manual implementation as an
  opt-out; that promise is kept.
* **Preserve ADR-0002 scope** — the macro covers the same narrow REST subset; the annotation
  vocabulary grows only by the additions already anticipated in ADR-0002 § Phase 2.

## Considered Options

* **Option A**: Extend the `#[modkit::rest_contract]` projection macro to (a) enforce method-set
  parity against the base at compile time and (b) additionally generate a server-side
  `register_<name>_routes()` function alongside the existing client struct.
* **Option B**: Collapse HTTP annotations onto the base trait; remove projection traits; a single
  `#[modkit::contract]` macro generates client, server routes, and OpenAPI in one pass.
* **Option C**: Retain projection traits unchanged; commit an `openapi.json` per SDK crate; add a
  CI diff check that regenerates and compares it on every PR.

## Decision Outcome

Chosen option: **Option A — Extended projection macro.**

The `#[modkit::rest_contract]` macro gains two new responsibilities while leaving the two-layer
design and the base trait untouched.

### 1. Compile-time base-projection parity check

The macro receives the projection trait token stream. It has access to the declared supertrait
bound (e.g., `: BillingApi`) and to the list of methods redeclared in the projection. It emits
a `const _` validation block that, for each method declared in the projection, verifies:

* The method name exists on the base trait (Rust enforces signature compatibility through the
  `: Base` supertrait; the macro's job is to catch the *coverage* direction).
* No extra methods exist in the projection that are absent from the base (those would generate
  unreachable client dispatch code).

A `#[rest_contract(require_full_coverage)]` opt-in causes the macro to also error when a method
exists on the base but is absent from the projection (i.e., no REST binding declared for it).
Without this flag the missing method is silently skipped (useful during incremental adoption).

### 2. Server-side `register_<name>_routes()` generation

From the same IR pass that builds `HttpBindingIr` for the client, the macro additionally emits:

```rust
/// Register all `BillingApi` REST routes on the given router.
pub fn register_billing_api_routes(
    router: axum::Router,
    openapi: &dyn modkit::openapi::OpenApiRegistry,
    service: std::sync::Arc<dyn BillingApi>,
) -> axum::Router { /* generated OperationBuilder calls */ }
```

This function is the drop-in replacement for the hand-written `routes.rs`. The same IR that
produces the client's URL template and method verb drives the `OperationBuilder` call sequence.

### Before / After

**Before (current state):**

```rust
// base trait — clean; zero annotations
#[modkit::contract(module = "billing", version = "v1")]
pub trait BillingApi: Send + Sync {
    /// Charge a payment method.
    #[idempotency(NonIdempotentWrite)]
    async fn charge(
        &self, req: ChargeRequest, #[secctx] ctx: SecurityContext,
    ) -> Result<ChargeResponse, BillingError>;
}

// projection — generates client only; no parity check; no server routes
#[modkit::rest_contract(base_path = "/api/billing/v1")]
pub trait BillingApiRest: BillingApi {
    #[post("/payments/charge")]
    async fn charge(
        &self, req: ChargeRequest, #[secctx] ctx: SecurityContext,
    ) -> Result<ChargeResponse, BillingError>;
}

// routes.rs — hand-written; independently duplicates path and schema
pub fn routes(service: Arc<dyn BillingApi>) -> Router {
    OperationBuilder::new()
        .post("/api/billing/v1/payments/charge")
        .summary("Charge a payment method")
        // ... 25 more lines, maintained manually
        .build(handler, service)
}
```

**After (extended projection):**

```rust
// base trait — UNCHANGED; still zero transport annotations
#[modkit::contract(module = "billing", version = "v1")]
pub trait BillingApi: Send + Sync {
    /// Charge a payment method.
    #[idempotency(NonIdempotentWrite)]
    async fn charge(
        &self, req: ChargeRequest, #[secctx] ctx: SecurityContext,
    ) -> Result<ChargeResponse, BillingError>;
}

// projection — generates client AND register_billing_api_routes()
// compile error if method absent from base or signature mismatches
#[modkit::rest_contract(base_path = "/api/billing/v1", require_full_coverage)]
pub trait BillingApiRest: BillingApi {
    /// Charge a payment method.
    /// Creates a new payment in `pending` status and returns its identifier.
    #[post("/payments/charge")]
    #[rest(status = 201, tag = "payments")]
    async fn charge(
        &self, req: ChargeRequest, #[secctx] ctx: SecurityContext,
    ) -> Result<ChargeResponse, BillingError>;
}

// routes.rs — DELETED; register_billing_api_routes() is now generated
```

### Metadata resolution for generated `OperationBuilder` calls

| `OperationBuilder` field | Source (in priority order) |
|--------------------------|----------------------------|
| `summary` | First line of the projection method's `///` doc-comment |
| `description` | Remaining lines of the projection method's `///` doc-comment |
| `operation_id` | `"<module>.<method_name>"` |
| `tag` | `#[rest(tag = "…")]`, or default `"<Module>"` |
| `.authenticated()` | Always emitted when a `#[secctx]` parameter is present |
| `.path_param(…)` | One call per `#[path]`-annotated parameter |
| `.query_param(…)` | One call per `#[query]`-annotated parameter (scalar / flat-struct) |
| Request body | `.json_request::<ReqType>()` when a Body binding exists |
| Success response | `.json_response_with_schema::<OkType>()`, overrideable with `#[rest(status = N)]` |
| Error responses | `.standard_errors()` always — mirrors `ContractError → Problem Details` |
| SSE response | `.sse_json::<ItemType>()` when `#[streaming]` is present |
| License | `.no_license_required()` default; `#[rest(license = "…")]` override |

Doc-comments on projection methods are optional. When absent the macro synthesizes a summary from
the method name (e.g., `charge` → `"Charge"`). Authors who want richer OpenAPI descriptions add
`///` lines to the projection method; these supplement, not replace, the base trait's doc-comments
which remain the canonical domain documentation.

### gRPC projection — unchanged, still additive

Adding gRPC means adding a separate `BillingApiGrpc` projection processed by
`#[modkit::grpc_contract]`. Each projection is a self-contained file with only its own transport's
annotations. No method accumulates annotations from multiple transports in one place.

```rust
// grpc/mod.rs — independent from rest/mod.rs
#[modkit::grpc_contract(package = "billing.v1")]
pub trait BillingApiGrpc: BillingApi {
    #[grpc(name = "ChargePayment")]   // optional; defaults to PascalCase of method name
    async fn charge(
        &self, req: ChargeRequest, #[secctx] ctx: SecurityContext,
    ) -> Result<ChargeResponse, BillingError>;
}
```

### Consequences

* The two-layer design from ADR-0001 is preserved and extended, not replaced. No migration
  required; existing projection traits compile unchanged.
* `register_<name>_routes()` is a new generated symbol; existing hand-written `routes.rs` files
  must be deleted or delegated to the generated function. The transition is per-SDK-crate and can
  be done incrementally.
* `require_full_coverage` is opt-in to allow incremental adoption; new SDK crates are expected to
  use it by default.
* The method-level annotation vocabulary grows by `#[path]`, `#[query]` (scalar / flat-struct),
  and `#[rest(status, tag, license, server_manual)]`. These were already anticipated in ADR-0002
  § Phase 2. The base trait annotation vocabulary is unchanged.
* `#[rest(server_manual)]` on a projection method excludes that method from
  `register_<name>_routes()` while keeping it in the client. The author writes a manual
  `OperationBuilder` call and composes it with the generated function.
* ADR-0002 scope is preserved: union bodies, multipart, response headers, and per-status schemas
  still require a manual `impl Base for MyClient`; the macro does not gain new coverage.

### Confirmation

* Unit tests: each projection annotation → `OperationBuilder` emission mapping covered by
  `cargo expand`-based macro expansion tests in `libs/modkit-contract-macros/tests/`.
* Parity test: projection with a method absent from the base emits a compile error naming the
  offending method.
* Coverage test: `require_full_coverage` on a projection that omits a base method emits a compile
  error listing the uncovered method name.
* Integration test: `register_billing_api_routes` for the `api-contracts` example produces an
  OpenAPI JSON fragment equal to the hand-written `routes.rs` fragment it replaces.
* Regression: all existing projection traits (`BillingApiRest` without `require_full_coverage`)
  continue to compile and produce an identical client struct to today's output.

## Pros and Cons of the Options

### Option A: Extended Projection Macro (chosen)

Extend `#[modkit::rest_contract]` to enforce parity and emit server routes from the same IR.

* Good, because base trait stays clean — zero transport annotations, unchanged from ADR-0001.
* Good, because each transport's projection is a self-contained file; adding gRPC does not add
  any annotations to the REST projection or to the base.
* Good, because zero migration cost — no existing code changes required; new capabilities are
  strictly additive.
* Good, because parity enforcement closes the silent-stale-client failure mode.
* Good, because server routes and OpenAPI spec derive from the same IR as the client — single
  source of truth.
* Neutral, because projection methods must still be redeclared (signatures duplicated from base).
  Mitigated: parity check turns signature drift from a silent bug into a loud compile error.

### Option B: Collapse Annotations onto Base Trait

HTTP annotations move onto the base trait. Projection traits are removed. One macro, one trait.

* Good, because each method is defined exactly once.
* Bad, because the base trait accumulates annotations from all transports in use. A REST + gRPC
  contract results in HTTP path attributes, gRPC name overrides, `#[path]`/`#[query]` inline on
  parameters, and `#[rest(…)]` overrides all on the same method — the trait becomes a transport
  configuration file, not a domain interface.
* Bad, because the annotation stack grows with each transport added; REST-only contracts start
  clean but are one gRPC addition away from becoming noisy.
* Bad, because migration requires rewriting every existing projection trait in the repository.
* Neutral, because the base trait emitted to consumers is clean (annotations stripped at expansion
  time), but authors reading the source see the full annotation stack.

### Option C: Retain Projection Traits + CI OpenAPI Diff

Commit `openapi.json` per SDK crate; CI regenerates and diffs on every PR.

* Good, because no code changes required.
* Good, because drift is caught in CI.
* Bad, because the root causes (no parity check, two independent OpenAPI generators) are not fixed.
* Bad, because committed `openapi.json` files become noisy regeneration artifacts on every method
  change.
* Bad, because two separate OpenAPI generators persist; CI enforces agreement but does not
  eliminate the duplication.

## More Information

* ADR-0001 — contract source of truth:
  [`0001-cpt-cf-binding-adr-contract-source-of-truth.md`](./0001-cpt-cf-binding-adr-contract-source-of-truth.md)
  — two-layer design is preserved; this ADR extends the projection macro's responsibilities.
* ADR-0002 — OpenAPI spec limits:
  [`0002-cpt-cf-binding-adr-openapi-spec-limits.md`](./0002-cpt-cf-binding-adr-openapi-spec-limits.md)
  — Phase 1 scope is unchanged; `#[path]`, `#[query]`, `#[rest(…)]` are the Phase 2 additions
  already anticipated there.
* Vision ADR-0003 (PR #1957) `cpt-cf-adr-rest-first-oop` — OoP modules each serve their own HTTP
  server with `OperationBuilder` routes; `register_<name>_routes()` is the generated function that
  satisfies that requirement.
* Vision ADR-0004 (PR #1957) `cpt-cf-adr-rest-client-generation` — Phase 2 (proc-macro from
  annotated trait) is implemented here via the extended projection macro.
* Current PoC: `libs/modkit-contract-macros/src/rest_contract.rs` (client generation);
  `examples/modkit/api-contracts/src/api/rest/routes.rs` (hand-written routes to be replaced).
