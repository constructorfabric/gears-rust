---
status: proposed
date: 2026-08-06
---

# Evolve Contracts Additively Within a Major Version; Use Parallel Traits for Breaking Changes

**ID**: `cpt-cf-binding-adr-contract-versioning`

## Table of Contents

1. [Context and Problem Statement](#context-and-problem-statement)
2. [Decision Drivers](#decision-drivers)
3. [Considered Options](#considered-options)
4. [Decision Outcome](#decision-outcome)
5. [Pros and Cons of the Options](#pros-and-cons-of-the-options)
6. [More Information](#more-information)

## Context and Problem Statement

DESIGN §7 (*Versioning and v1/v2 Coexistence*) covers non-breaking evolution
(`#[non_exhaustive]` types, default trait methods, new enum variants) but
explicitly defers the Rust-side major-version story: it lists three candidate
strategies (parallel traits, trait inheritance, separate SDK crates per major)
and states *"The Rust-side strategy is deferred to an ADR."* This ADR makes that
decision.

**What exists today.**

* `#[toolkit::contract(gear = "…", version = "v1")]` records the version as
  **metadata only** — it lands in `ContractDescriptor.version` and
  `ContractIr.version` (`libs/toolkit-contract-macros/src/codegen.rs`) and does
  **not** influence routing or URL construction.
* The URL version segment is written by hand in the projection's
  `base_path` (e.g. `#[toolkit::rest_contract(base_path = "/billing/v1")]`).
  The two are therefore **decoupled** and can drift.
* Additive evolution is supported: DTOs are `#[non_exhaustive]`, and a
  projection/base method with a default body is recorded as `optional` in the IR
  (`MethodIr.optional` / `HttpMethodBindingIr.optional`).
* Breaking changes are **detected** in CI: `.github/workflows/api_contracts.yml`
  runs `oasdiff breaking` against the base-branch spec and blocks merge unless
  the PR carries the `breaking-api-acknowledged` label. Detection is not a
  coexistence mechanism.
* Route generation is **additive and composable**: each
  `register_<trait_snake>_routes(router, openapi, svc)` takes and returns an
  `axum::Router` with no global state, so several of them (plus hand-written
  `OperationBuilder` chains) compose onto one router.
* `ClientHub` keys registrations by `TypeKey(std::any::type_name::<T>())`
  (`libs/toolkit/src/client_hub.rs`) — a **fully-qualified** name, so two traits
  that share a short name but live in different modules are distinct keys.

**Constraints DESIGN §7 places on any chosen strategy.** It must support
(a) simultaneous presence of V1 and V2 traits *in the same SDK crate*, (b) a
migration window in which plugins implement both, and (c) clear deprecation of
V1 on a documented timeline. It also states the remote-side rule: a service MUST
preserve backwards compatibility within a major version.

## Decision Drivers

* **The common case must not need a version at all** — the overwhelming majority
  of contract changes are additive and should ship inside the current major.
* **Both versions must be servable simultaneously** during a migration window
  (DESIGN §7 (a)/(b)).
* **Version selection must be a compile-time, type-level property** — a consumer
  must not be able to mix versions by accident.
* **Work with the shipped codegen**, not against it: additive router
  composition, `ClientHub` type keying, and the enforced trait-name suffix rule.
* **Low ceremony for the in-repo case** — versioning must not force a crate
  split for internal gears.
* **Mechanical breaking-change detection** must remain the CI gate, independent
  of the coexistence mechanism.
* **Industry alignment** — prefer the model Rust/gRPC/Kubernetes developers
  already recognise over a bespoke scheme.

## Considered Options

* **Option A**: Additive-by-default within a major + **parallel traits** for
  breaking changes (version as a namespace/infix, one SDK crate).
* **Option B**: **Trait inheritance** — `trait PaymentV2Api: PaymentV1Api`,
  additive methods only.
* **Option C**: **Separate SDK crate per major** — `billing-sdk-v1`,
  `billing-sdk-v2`.
* **Option D**: **Date/header-based versioning** (Stripe-style) — a single trait
  surface, version pinned per caller via a request header.

## Decision Outcome

Chosen option: **Option A — additive-by-default within a major version, with
parallel traits for breaking changes.**

This is the model gRPC (version in the package name: `billing.v1`,
`billing.v2`) and Kubernetes (parallel API-group versions) use, and it satisfies
every DESIGN §7 constraint without a crate split.

### 1. Additive-by-default — the normal path

Within a major version, **only additive changes are permitted**, and they do
**not** get a new version:

* new optional fields on `#[non_exhaustive]` request/response structs;
* new variants on `#[non_exhaustive]` enums (including `ContractError` enums —
  unknown `(error_domain, error_code)` pairs already round-trip back as the
  generic `Problem` envelope, so an older client tolerates a newer server);
* new contract methods **with a default body** (recorded as `optional` in the
  IR, so peers may omit the endpoint);
* new endpoints on the provider (the generated spec is a *minimum* conformance
  contract per ADR-0002 — a provider may offer strictly more).

The following are **breaking** and require a new major version: removing or
renaming a method or field, changing a field/parameter/return type, making an
optional field required, tightening validation, or changing the success status
or error semantics of an existing operation.

### 2. Breaking change → a parallel trait pair

A new major version introduces a **new base trait and a new projection**, served
alongside the old one. The version is spelled as a **trailing `V<N>` marker on
the trait name** — see [Version spelling](#version-spelling-a-trailing-vn-on-the-trait-name)
below for why, and why module-per-version was rejected. Files may still be split
per version (`contract.rs` / `contract_v2.rs`); that is a source-layout
convention, not part of the type name:

```rust
// sdk/src/contract.rs — v1, untouched by the v2 work
#[toolkit::contract(gear = "billing", version = "v1")]
pub trait PaymentApi: Send + Sync { /* … */ }

// sdk/src/rest.rs
#[toolkit::rest_contract(base_path = "/billing/v1")]
pub trait PaymentApiRest: PaymentApi { /* … */ }

// sdk/src/contract_v2.rs — the breaking change lives here
#[toolkit::contract(gear = "billing", version = "v2")]
pub trait PaymentApiV2: Send + Sync { /* … */ }

// sdk/src/rest_v2.rs
#[toolkit::rest_contract(base_path = "/billing/v2", require_full_coverage)]
pub trait PaymentApiV2Rest: PaymentApiV2 { /* … */ }
```

Both versions live in **one SDK crate** (DESIGN §7 (a)), and the mechanics
already work with the shipped codegen:

* **Server**: each projection generates its own registration function
  (`register_payment_api_rest_routes()` and
  `register_payment_api_v2_rest_routes()`); the gear composes both onto the same
  router, and both contribute to the same `OpenApiRegistry` document. Because the
  trait names differ, their `operationId`s differ too.
* **`ClientHub`**: `register::<dyn PaymentApi>` and
  `register::<dyn PaymentApiV2>` are distinct keys (`TypeKey` is the
  fully-qualified `type_name`). A consumer picks its version by the type it
  resolves — a compile-time choice.
* **`#[toolkit::provides]`**: each version gets its own `wire_payment_api` /
  `wire_payment_api_v2` method and its own `client_wiring` config key, so one
  gear can provide both.
* **Plugins** may implement both traits during the migration window (DESIGN §7
  (b)); each is an ordinary Rust trait impl.

### Version spelling: a trailing `V<N>` on the trait name

**Decision: spell the version as a trailing marker on the trait name —
`NotificationBackendV2` + `NotificationBackendV2Rest`.**

The trait-name suffix rule (DESIGN D6) is enforced at macro-expansion time, but
it classifies the *contract type*, and a major version is orthogonal metadata.
The macro therefore strips a trailing `V<digits>` marker before matching the
type suffix (`support::strip_version_suffix`, used by
`ContractKind::from_suffix` and by the REST/gRPC remote-capability gates), so
`NotificationBackendV2` classifies as a `Backend` exactly like
`NotificationBackend`. The rule stays strict: `PaymentServiceV2` reduces to
`PaymentService`, still has no contract-type suffix, and is still rejected. A
local-only name (`FooEmbedded`, `FooExtension`) has no trailing digits, so
stripping can never turn it into a remote-capable one — `FooEmbeddedV2` is still
refused a REST/gRPC projection.

The projection name needs no special rule: `{Base}Rest` yields
`NotificationBackendV2Rest`.

**Module-per-version (`v2::NotificationBackend`) is NOT recommended** with the
current codegen, because every generated identifier derives from the trait name
alone, so two same-named traits in different modules collide:

* **Duplicate `operationId`s, silently.** `operation_id` is
  `{trait_snake}_{method}` (`rest_contract.rs`), with no version or `base_path`
  component, so both versions emit `notification_backend_rest_deliver` into the
  shared OpenAPI document. The registry keys operations by `METHOD:path`, so both
  are kept and nothing warns — the document violates OpenAPI's operationId
  uniqueness requirement. (`docs/api/api.json` currently has zero duplicates.)
* **`#[toolkit::provides]` fails to compile.** The wire-method name and the
  wiring config key both come from the contract path's *last segment*
  (`provides.rs`), so a gear providing both versions emits `wire_notification_backend`
  twice → `E0592`. `#[toolkit::consumes]` collides the same way.
* **Indistinguishable telemetry.** The client span name and `rpc.service` are
  the trait name, and `http.route` does not include `base_path`, so v1 and v2
  calls are indistinguishable in traces.

A distinct trait name avoids all three at no cost. Module-per-version remains
viable only where the two versions never share a gear or an OpenAPI document.

### 3. `version` and `base_path` must agree

The contract's `version = "vN"` MUST match the version segment of the
projection's `base_path` (`/…/vN`). Today these are independent inputs; the
agreement is a convention. Projections that opt into
`#[toolkit::rest_contract(base_path = "…", require_full_coverage)]` additionally
get this consistency asserted by the generated coverage test, alongside the
base↔projection method-set check.

### 4. Deprecation and sunset

* When vN+1 ships, the vN base and projection traits are marked
  `#[deprecated(note = "…; sunset <date>")]` and the SDK README/CHANGELOG
  records the sunset date.
* Both versions are served for the announced migration window; vN routes are
  removed only after it closes (DESIGN §7 (c)).
* Within a major version the provider MUST stay backwards compatible; the
  `oasdiff breaking` CI gate is the mechanical check, and a breaking diff
  without a new major version is a red build.

### Consequences

* The common path is unchanged: additive evolution needs no new trait, no new
  route, no consumer change.
* A major bump costs a new module (or trait-name infix) plus one extra
  `register_…_routes()` call in the provider gear — no crate split, no macro
  changes.
* Consumers migrate by changing an import/type, which the compiler verifies.
* The version appears in three places (contract `version`, `base_path`, module
  path). §3 constrains the first two; keeping the module path aligned is
  convention. This is the accepted cost of not deriving `base_path` from
  `version` (which would be a breaking change to every existing projection).
* DESIGN §7 stops being an open question; its `NotificationBackendV2` spelling
  is corrected to the forms in §2 above.
* Per-major **crates** (Option C) remain available as an escape hatch for
  externally published SDKs; they are not the default for in-repo gears.

### Confirmation

* Composability is already exercised: the `api-contracts` example composes a
  generated `register_payment_api_rest_routes()` with a hand-written manual
  route chain on one router (`examples/toolkit/api-contracts/api-contracts/src/api/rest/routes.rs`),
  verified by `tests/integration.rs`. Two generated registrations compose
  identically.
* `ClientHub` key distinctness is guaranteed by `TypeKey` being
  `type_name::<T>()` (`libs/toolkit/src/client_hub.rs`), covered by its
  registration/overwrite unit tests.
* The `version`↔`base_path` assertion is emitted by
  `generate_full_coverage_check` under `require_full_coverage`
  (`libs/toolkit-contract-macros/src/rest_contract.rs`), and the trait-name
  marker is checked against the declared `version` in `parse::parse_trait`, so
  all three spellings of the version agree mechanically.
* The version-spelling relaxation is covered by unit tests on
  `support::strip_version_suffix` / `ContractKind::from_suffix` plus trybuild
  fixtures: `pass/valid_versioned_contract.rs` (a versioned `Api` and `Backend`
  with their projections compile) and `fail/bad_suffix_versioned.rs`
  (`PaymentServiceV2` is still rejected).
* v1+v2 coexistence is exercised live in the `api-contracts` example: the SDK
  ships `PaymentApi`/`PaymentApiRest` (v1) alongside `PaymentApiV2`/
  `PaymentApiV2Rest` (v2), the gear provides both, and the server serves
  `/api-contracts/v1/**` and `/api-contracts/v2/**` from one router —
  asserted by `tests/v1_v2_coexistence.rs`, which also pins that every
  `operationId` in the shared OpenAPI document is unique.

## Pros and Cons of the Options

### Option A: Additive-by-default + Parallel Traits (chosen)

* Good, because the 99% case (additive change) needs no versioning ceremony at
  all.
* Good, because it satisfies all three DESIGN §7 constraints — both versions in
  one SDK crate, plugins may implement both, explicit deprecation window.
* Good, because the runtime needs **no** change and route generation is already
  additive: `ClientHub` already keys by fully-qualified type, and each projection
  already emits its own registration function. The only codegen change is the
  one-function `strip_version_suffix` relaxation so a trailing `V<N>` marker does
  not defeat the contract-type suffix rule.
* Good, because version choice is type-level and compiler-checked; a consumer
  cannot silently mix v1 and v2.
* Good, because it mirrors gRPC package versioning and Kubernetes API-group
  versioning — the model most developers already know.
* Neutral, because breaking changes duplicate the trait surface for the
  migration window. That duplication is the point: the old contract keeps
  working untouched.
* Bad, because the version is spelled in three places — the trait name, the
  `version` attribute, and `base_path`. §3 and the trait-name check make all
  three agree mechanically; deriving `base_path` from `version` outright would
  break every existing projection.

### Option B: Trait Inheritance (`V2: V1`)

* Good, because additive methods are declared once and V1 consumers keep
  working.
* Bad, because it is a **category error** for versioning: inheritance can only
  express *additive* changes, but a new major version exists precisely to make a
  *breaking* one (remove a method, change a type). The case it handles is the
  case that needs no new version.
* Bad, because `V2: V1` forces every V2 implementor to also implement the V1
  surface — including methods the breaking change intended to remove.
* Bad, because it entangles the two versions' lifecycles: V1 cannot be sunset
  while V2 inherits from it.

### Option C: Separate SDK Crate per Major

* Good, because isolation is maximal — v1 and v2 can diverge freely, including
  their dependency trees.
* Good, because it is mandatory in some ecosystems (Go's `module/v2` import
  paths) and natural for externally published SDKs.
* Bad, because it contradicts DESIGN §7's requirement that V1 and V2 coexist in
  the *same* SDK crate.
* Bad, because it multiplies release/versioning overhead per gear (new crate,
  new `Cargo.toml`, new publish target) for what is usually a small breaking
  change.
* Bad, because shared types must be duplicated or factored into a third crate,
  adding a dependency layer.
* Neutral, because it remains available as an escape hatch when a consumer must
  link both versions simultaneously or the SDK is published externally.

### Option D: Date/Header-Based Versioning (Stripe-style)

* Good, because callers pin a version without any URL change, and the provider
  can ship many small versioned behaviours.
* Bad, because it requires a per-request version-negotiation and
  request/response transformation layer that does not exist in this platform.
* Bad, because the version stops being a compile-time property of the Rust
  type — the exact safety the contract-binding design is built on (a consumer
  holding `Arc<dyn PaymentApi>` would no longer know which behaviour it gets).
* Bad, because it is a poor fit for internal service-to-service contracts; its
  value is in long-lived public APIs with many external integrators.

## More Information

* PRD: [`../PRD.md`](../PRD.md) — §5.11 `cpt-cf-binding-fr-non-exhaustive`,
  `cpt-cf-binding-fr-default-methods` (the additive-evolution primitives).
* DESIGN: [`../DESIGN.md`](../DESIGN.md) — §7 *Versioning and v1/v2
  Coexistence* (the open question this ADR closes); D6 naming convention (the
  enforced suffix rule constraining version spelling).
* ADR-0001 — contract source of truth:
  [`./0001-cpt-cf-binding-adr-contract-source-of-truth.md`](./0001-cpt-cf-binding-adr-contract-source-of-truth.md)
  — the trait is the versioned artefact.
* ADR-0002 — OpenAPI spec limits:
  [`./0002-cpt-cf-binding-adr-openapi-spec-limits.md`](./0002-cpt-cf-binding-adr-openapi-spec-limits.md)
  — "minimum conformance contract" is what makes provider-side additions
  non-breaking.
* ADR-0003 — projection server generation:
  [`./0003-cpt-cf-binding-adr-projection-server-gen.md`](./0003-cpt-cf-binding-adr-projection-server-gen.md)
  — `register_<name>_routes()` additivity is what lets two versions share a
  router; `require_full_coverage` carries the `version`↔`base_path` assertion.
* Breaking-change gate: `.github/workflows/api_contracts.yml` (`oasdiff
  breaking`, `breaking-api-acknowledged` override label).
* Industry precedent: gRPC/protobuf package versioning (`pkg.v1` → `pkg.v2`,
  field numbers never reused); Kubernetes parallel API-group versions with
  conversion; Google AIP-180/185 (backwards-compatibility rules and versioning).
