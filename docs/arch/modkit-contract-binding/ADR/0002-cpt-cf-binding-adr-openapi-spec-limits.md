---
status: accepted
date: 2026-04-10
---

# Treat Generated OpenAPI Spec as Minimum Conformance Contract, Not Exhaustive Specification

**ID**: `cpt-cf-binding-adr-openapi-spec-limits`

## Context and Problem Statement

ADR-0001 chose the Rust trait as the contract source of truth, with `#[modkit_rest_contract]` generating clients and OpenAPI specs via `schemars`. This decision inherits a structural limitation: a Rust trait signature plus `schemars`-derived JSON schemas cannot faithfully express the full REST surface. The generator cannot represent union request bodies (oneOf/anyOf with discriminators), content-type negotiation (e.g., `application/vnd.foo+json`), multiple response schemas per status code, non-body content (multipart, form, octet-stream), path parameters with regex/range validation, response headers, custom security schemes, complex query parameter serialization (arrays, deep objects), or responses that vary by input.

Option D (IDL-first) was rejected in ADR-0001 because no IDL captures REST and gRPC faithfully at once. The trait-first approach has a symmetric problem: Rust traits plus `schemars` cannot faithfully express everything REST permits. If the generated spec is treated as the authoritative specification for third-party implementors, two failure modes appear:

1. Third parties implementing richer REST surfaces (multipart uploads, content negotiation, per-status schemas) fail registration even when their service is correct.
2. Services that match path, method, and the narrow generated schema pass validation while serving payloads the Rust client cannot decode.

This ADR defines the role of the generated spec and the policy for everything it cannot express.

## Decision Drivers

* **Honesty over coverage** — the platform must not pretend the generated spec is exhaustive when it is not
* **Compile-time safety for the common case** — POST/GET/DELETE with JSON bodies and SSE streams must be fully machine-verifiable
* **Escape hatch preserved** — authors must retain the ability to implement contracts manually when the generator is too narrow (ADR-0001 Option A promise)
* **Third-party clarity** — external integrators must know which parts of the contract are generator-derived and which parts are hand-authored
* **Directory validation correctness** — service registration must not falsely reject valid richer implementations, and must not falsely accept narrower ones
* **Tooling maturity** — prefer the idiomatic output of mature Rust crates (`schemars`) over a bespoke annotation language
* **Macro surface simplicity** — the attribute vocabulary must remain small enough to learn in an afternoon
* **Failure visibility** — if a trait asks for something the generator cannot express, the build must fail fast, not emit a silently-broken spec

## Considered Options

* **Option A**: Narrow but honest — generator covers the common case, manual implementation fills the gaps, generated OpenAPI is declared the minimum conformance contract (current design)
* **Option B**: Aggressive annotation vocabulary — extend `#[modkit_rest_contract]` with `#[content_type]`, `#[response_header]`, `#[multipart]`, `#[response(status=404, schema=…)]`, `#[path(pattern="…")]`, and similar attributes until the macro covers ~95% of REST
* **Option C**: OpenAPI-first with augmentation — hand-write the spec, generate Rust types from it
* **Option D**: Dual source — authors write both the Rust trait and the OpenAPI YAML, CI verifies they agree

## Decision Outcome

Chosen option: **Option A — Narrow but honest.** The `#[modkit_rest_contract]` macro covers a deliberate subset of REST, the generated OpenAPI spec is documented as the **minimum conformance contract** (services may offer strictly more, never strictly less), and anything outside the subset is handled by manual implementation — authors write `impl Base for MyCustomRestClient` with full HTTP control, and the consumer interface (`Arc<dyn Base>`) is identical to the macro-generated case.

Option B was rejected because the attribute vocabulary required to cover 95% of REST approaches the complexity of a bespoke IDL embedded in Rust attribute syntax, reintroducing the "lowest common denominator" problem ADR-0001 used to reject IDL-first. Option C was rejected for the same reasons as ADR-0001 Option C. Option D was rejected because dual sources drift in practice regardless of CI discipline, and because it forces every author to learn two authoring surfaces.

### Phase 1 scope — what the macro generates

The `#[modkit_rest_contract]` macro supports:

* `POST`, `GET`, `DELETE` with a single JSON request body (where applicable)
* Path parameter extraction via the future `#[path]` annotation on method arguments
* Query parameter serialization via the future `#[query]` annotation (scalar and flat-struct forms only)
* A single success response schema per method, derived from the `Ok` type of the method's `Result<T, E>`
* Error responses in RFC 9457 Problem Details form with `error_code` + `error_domain` extensions (see ADR-0001 in the errors directory)
* Server-sent events via `#[streaming]`, rendered as `text/event-stream` in the OpenAPI spec
* Standard retry, timeout, and base URL configuration via `ClientConfig`

### Phase 1 scope — what is explicitly out of scope

* Union request bodies (oneOf / anyOf with discriminators)
* Multiple response content types per status code
* Response headers beyond the standard platform set (`Content-Type`, `Content-Length`, tracing headers injected by middleware)
* Multipart, URL-encoded-form, and octet-stream request or response bodies
* Regex patterns and integer ranges on path parameters
* Custom security schemes in the OpenAPI spec (authentication is carried by `SecurityContext` as the first method argument — see DESIGN)
* Webhooks (modelled as separate push contracts, not as REST callbacks)
* Hypermedia / HATEOAS link generation

### Escape hatch

For any contract method that needs a feature outside the Phase 1 scope, authors write a manual client:

```rust
impl NotificationBackend for MyCustomRestClient {
    // full control over reqwest, content negotiation, multipart, headers, etc.
}
```

The consumer continues to depend on `Arc<dyn NotificationBackend>`; it cannot tell whether the implementation was macro-generated or hand-written. For these methods, the OpenAPI spec is either hand-authored or the generated spec is augmented with vendor extensions (`x-modkit-*`) that describe the manual surface.

### Consequences

* Macro documentation must enumerate the supported subset exactly, with a dedicated "Not supported — use manual implementation" section; no feature may be tacitly supported.
* CI must fail fast when a trait asks for an unsupported pattern (e.g., a method returning `Result<EnumWithMultipleVariants, _>` without a discriminator strategy). The macro must emit a compile error, not a silently-broken spec.
* The directory's OpenAPI validator treats the registered spec as the **minimum conformance contract**: remote services may legitimately expose additional paths, headers, content types, or schema fields beyond what the spec declares, but they must honor every path, method, required field, and response shape the spec does declare.
* Third-party integrators receive a **starting-point spec**, not a complete specification. Where a service uses manual implementation, the README for that SDK crate must state which methods are hand-written and how the OpenAPI spec was produced (generated, augmented, or hand-written).
* The generator emits a top-level `x-modkit-spec-scope: minimum-conformance` vendor extension so downstream tooling can distinguish a generator-derived spec from a hand-curated one.
* When a trait mixes macro-supported and manual methods, the macro generates the spec for its subset and the author appends the rest; the DESIGN document defines the merge strategy.
* Extending the macro's scope later (e.g., adding `#[content_type]` in Phase 2) is a non-breaking change provided the attribute is optional and the default behavior remains the Phase 1 semantics.
* The test suite must include a "rejected trait" fixture for every unsupported pattern listed above; each fixture must produce a compile error with a message pointing at the manual-implementation escape hatch.

### Confirmation

The PoC at `~/projects/modkit-binding-poc/` validates the narrow-but-honest approach on the `notification-sdk` crate: `NotificationBackend` with POST+JSON and SSE streaming, a generated OpenAPI spec, and a manual-implementation path exercised by a secondary fixture. Anything more complex (multipart upload, header-driven response variants) falls to a hand-written client in the PoC. The evidence report `~/projects/modkit-binding-poc/docs/research/rest-grpc-unification-evidence.html` (referenced from the PoC README) documents the reasoning for keeping the generator narrow rather than attempting to model the full REST surface in Rust attributes.

## Pros and Cons of the Options

### Option A: Narrow but Honest

The macro covers a declared subset of REST. Everything outside the subset is handled by manual `impl Base for …`. The generated OpenAPI is published as the minimum conformance contract.

**Advantages:**

* Keeps the macro small and learnable; the full attribute vocabulary fits on one page.
* The generated spec is always correct for what it claims to describe; it simply claims less than a hand-authored spec.
* The escape hatch promised by ADR-0001 is preserved and exercised, not vestigial.
* Directory validation is well-defined: a remote may offer more, never less.
* Phase-2 scope extensions are additive and non-breaking.

**Tradeoffs:**

* Authors must know which methods fall inside the subset and which require manual implementation; the boundary must be documented and taught.
* Third-party integrators cannot treat the spec as exhaustive and must consult the SDK README for manual methods.

**Disadvantages:**

* SDKs mixing macro-generated and manual methods produce a split OpenAPI spec that must be merged, adding a small amount of build-time tooling.
* Some useful REST features (per-status schemas, response headers) require falling out of the macro entirely rather than adding one attribute.

### Option B: Aggressive Annotation Vocabulary

Extend the macro with attributes for every REST feature: `#[content_type]`, `#[response_header]`, `#[multipart]`, `#[response(status=404, schema=…)]`, `#[path(pattern="…")]`, `#[query(style="deepObject")]`, `#[security(scheme="bearer")]`, and so on. The macro aims to cover ~95% of REST.

**Advantages:**

* A larger fraction of SDKs can be macro-generated end-to-end.
* The generated OpenAPI spec is closer to a complete specification.
* Manual implementation remains an escape hatch but is reached far less often.

**Tradeoffs:**

* The attribute vocabulary grows into a small embedded DSL; authors must learn it in addition to Rust traits, OpenAPI, and HTTP semantics.

**Disadvantages:**

* The attribute surface reintroduces the "lowest common denominator IDL" problem that ADR-0001 used to reject Option D — REST semantics are encoded in attribute syntax that must be parsed, validated, and maintained.
* Procedural-macro complexity balloons; `cf-modkit-contract-macros` becomes a non-trivial artifact with its own changelog and release cadence.
* Each new attribute is a new failure mode at macro-expansion time, with error messages that must point back into the author's trait.
* Attributes interact (`#[multipart]` with `#[response_header]` with `#[streaming]`) producing combinatorial edge cases.
* The incremental return diminishes sharply past ~80% coverage; the last 15% still requires manual implementation, which now must coexist with a much larger macro surface.

### Option C: OpenAPI-First with Augmentation

Hand-write the OpenAPI spec. Generate Rust types and client stubs from it. Authors extend the generated Rust with business logic.

**Advantages:**

* Third-party integrators receive the source of truth directly.
* Full REST expressiveness is available by construction.

**Disadvantages:**

* Rejected in ADR-0001 for the same reasons (Rust developers editing YAML, generated Rust as second-class, no gRPC story, slow edit loop, YAML-to-logic drift). Restating the same rejection.

### Option D: Dual Source

Authors write the Rust trait and the OpenAPI YAML. CI verifies the two agree on paths, methods, request/response schemas.

**Advantages:**

* Both constituencies (Rust developers, third-party integrators) get a first-class source.
* The OpenAPI spec can use the full REST vocabulary without constraining the Rust trait.

**Tradeoffs:**

* CI consistency checks become the contract between the two sources; their strictness determines how much drift is tolerated.

**Disadvantages:**

* Every author learns two authoring surfaces and must keep them in sync for every change.
* Drift between trait and YAML is the default state; CI catches divergence but cannot decide which side is correct when they disagree.
* Review burden doubles — every contract change is reviewed in both representations.
* The platform inherits the authoring cost of IDL-first and the ambiguity of trait-first without gaining the benefits of either.

## More Information

* PRD: [`../PRD.md`](../PRD.md)
* DESIGN: [`../DESIGN.md`](../DESIGN.md)
* ADR-0001 — contract source of truth: [`./0001-cpt-cf-binding-adr-contract-source-of-truth.md`](./0001-cpt-cf-binding-adr-contract-source-of-truth.md)
* Evidence report on REST/gRPC unification limits: [rest-grpc-unification-evidence.html](https://github.com/striped-zebra-dev/modkit-binding-poc/blob/main/docs/research/rest-grpc-unification-evidence.html)
* PoC repository: [striped-zebra-dev/modkit-binding-poc](https://github.com/striped-zebra-dev/modkit-binding-poc) (the README links the evidence report)
* RFC 9457 Problem Details: https://www.rfc-editor.org/rfc/rfc9457
