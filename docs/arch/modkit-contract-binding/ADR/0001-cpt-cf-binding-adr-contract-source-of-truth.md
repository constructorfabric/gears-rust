---
status: accepted
date: 2026-04-10
---

# Use Rust Trait as Contract Source of Truth, with Macro Generation and Manual Implementation Both Supported

**ID**: `cpt-cf-binding-adr-contract-source-of-truth`

## Context and Problem Statement

The contract-binding system needs a single source of truth for module contracts. The source of truth determines how contracts are authored, how clients are produced, how OpenAPI specs are generated, and how the system evolves. The choice affects every module author, every plugin implementer, and every third-party integrator.

Four realistic approaches are available, ranging from "trait-first with code generation" to "IDL-first with generated Rust types." Each has tradeoffs for compile-time safety, boilerplate, multi-language support, developer ergonomics, and evolution cost.

## Decision Drivers

* **Compile-time safety** — contract violations should be caught at `cargo check`, not at runtime or in production
* **Zero-cost in-process path** — compile-time plugins must not pay any overhead compared to a direct trait method call
* **Rust-native developer experience** — the primary audience is Rust developers; the happy path must feel idiomatic
* **Third-party language support** — external teams must be able to implement contracts in Go, Java, Python without compiling Rust
* **Evolution cost** — adding fields, methods, or transport projections must be non-breaking for existing plugins and consumers
* **Escape hatch** — when the common-case generator does not cover a specific need, authors must be able to write the client by hand
* **Tooling maturity** — prefer approaches with mature Rust ecosystem support over those requiring bespoke tooling
* **Readability** — a developer reading the SDK crate should understand the contract without chasing macro expansions or external files

## Considered Options

* **Option A**: Contract trait + macro generation + manual implementation allowed (current design)
* **Option B**: Contract trait + manual implementations only
* **Option C**: OpenAPI-first — OpenAPI YAML as source, generate Rust traits and clients
* **Option D**: IDL-first — proto / Smithy / TypeSpec as source, generate Rust types and clients

## Decision Outcome

Chosen option: **Option A — Contract trait as source of truth, with `#[modkit_rest_contract]` macro generating clients for the common case and manual implementation explicitly supported as an escape hatch.**

The Rust trait is the authoritative definition of the domain contract. Consumers depend on the base trait (`Arc<dyn NotificationBackend>`). The macro reads the optional protocol projection trait (`NotificationBackendRest`) and generates a REST client that implements the base trait with HTTP dispatch. For cases the macro does not cover (complex path templates, query parameter composition, custom auth, bespoke retry), authors write the client by hand — the consumer interface is identical either way.

This option was chosen because it is the only one that satisfies all decision drivers simultaneously. Option B solves nothing — it just codifies the status quo of every SDK reinventing HTTP clients. Option C forces the team to maintain OpenAPI YAML alongside Rust code, creating drift. Option D fails because no IDL captures REST and gRPC faithfully at the same time (see evidence report `docs/research/rest-grpc-unification-evidence.html`), and the generated Rust types would be second-class citizens.

### Consequences

* All module SDK crates define contracts as plain Rust traits with zero transport annotations
* Protocol projections are separate traits annotated with `#[modkit_rest_contract]` that extend the base via `: Base`
* The `cf-modkit-contract-macros` crate owns the proc macros and must be maintained for the platform's lifetime
* The `cf-modkit-contract-runtime` crate provides `ProblemDetails`, `ClientConfig`, SSE parser, retry helper — all dependencies of generated clients
* OpenAPI specs are a **derived artifact** generated at runtime from the trait via `schemars`, not a hand-written source
* Third-party implementors consume the generated OpenAPI spec at `/.well-known/openapi.json` to build clients in other languages
* Manual client implementations must implement the same base trait; the consumer cannot tell the difference
* The macro intentionally covers the common case only (POST + JSON, SSE via `#[streaming]`, retry via `#[retryable]`); complex REST semantics are handled by hand-written clients
* Adding a gRPC projection is purely additive — new trait `*Grpc`, new macro `#[modkit_grpc_contract]`, new generated client; the base trait and REST projection are untouched
* Signature drift between base and projection is a compile error (`: Base` supertrait bound + redeclaration)
* The evolution story relies on `#[non_exhaustive]` types and default trait methods — authors must discipline themselves to use both

### Confirmation

The PoC at `striped-zebra-dev/modkit-binding-poc` validates the approach end-to-end: `NotificationBackend` base trait + `NotificationBackendRest` projection with `#[modkit_rest_contract]`, generated `NotificationBackendRestClient`, working OpenAPI spec, SSE streaming, round-trip error mapping. All 8 tests pass. `make demo` shows identical behavior via compile-time plugin and REST proxy.

## Pros and Cons of the Options

### Option A: Contract Trait + Macro Generation + Manual Implementation Allowed

The Rust trait is the source of truth. A proc macro (`#[modkit_rest_contract]`) reads a protocol projection trait and generates a REST client. Authors can bypass the macro and write clients by hand when needed.

**Advantages:**

* The Rust trait is what consumers already depend on — zero indirection for the common case.
* The Rust compiler enforces contract conformance through supertrait bounds and method signature matching.
* Compile-time plugins implement the base trait directly with zero overhead.
* The macro covers the 80% case and hand-written clients cover the rest — no lock-in.
* Adding a new transport (gRPC, other) is purely additive — a new projection trait, a new macro, no changes to the base or the REST projection.
* The OpenAPI specification is generated from the same trait consumed by the Rust client; the specification cannot drift from the implementation.

**Tradeoffs:**

* Procedural macros are opaque; debugging requires `cargo expand` to inspect generated code.
* The macro must cover many edge cases over time (generics, lifetimes, complex return types) or defer them to hand-written clients.

**Disadvantages:**

* The approach is Rust-specific; third parties depend on the generated OpenAPI specification to work in other languages.
* The macro adds a compile-time dependency on `syn`, `quote`, and `schemars` in SDK crates.

### Option B: Contract Trait + Manual Implementations Only

Rust trait defines the contract; every REST client is hand-written. No procedural macro, no code generation. Authors write `impl NotificationBackend for NotificationBackendRestClient` directly with `reqwest` calls, error mapping, retry logic.

**Advantages:**

* No macro indirection — every line of code is visible and debuggable with standard Rust tools.
* Authors retain full control over HTTP details (paths, methods, headers, authentication).
* SDK crates have no dependency on `cf-modkit-contract-macros`.

**Disadvantages:**

* Every SDK reinvents the same boilerplate (HTTP client setup, error reconstruction, retry with backoff, SSE parsing), producing substantial duplication.
* No automatic OpenAPI specification generation — authors must hand-write YAML and keep it synchronized with the trait, which will inevitably drift.
* Third-party integrators have no language-neutral contract to target.
* Error mapping across module boundaries is inconsistent — each SDK implements `ContractError` differently.
* This is effectively the current state of the platform, which is the problem this change exists to resolve.

### Option C: OpenAPI-First — OpenAPI YAML as Source, Generate Rust Types

The contract is authored as an OpenAPI YAML file. A build step generates Rust traits, request/response types, and clients. Manual implementations work against the generated traits.

**Advantages:**

* OpenAPI is a mature, widely-understood standard with substantial tooling support.
* Third-party integrators receive the source of truth directly, without a derivation step.
* REST concerns (paths, methods, query parameters, headers) are first-class in the IDL.

**Disadvantages:**

* Rust developers edit YAML to modify a contract, losing Rust-native ergonomics.
* Generated Rust types are second-class citizens — `Debug` derivations, custom implementations, and newtype wrappers are awkward to introduce.
* Compile-time plugins must work against generated traits rather than hand-written ones; plugin authors are forced to depend on codegen artifacts.
* OpenAPI has no canonical representation of streaming; SSE is expressed via the `text/event-stream` media type, but event schemas are lossy.
* OpenAPI does not cover gRPC; adding a gRPC projection requires a separate source of truth, reintroducing the problem of Option D.
* The build step slows the edit loop (YAML change → codegen → compile) compared to a direct trait edit.
* Maintaining the YAML source in alignment with actual Rust business logic requires discipline; in practice, drift occurs.

### Option D: IDL-First — Protobuf, Smithy, or TypeSpec as Source

The contract is authored in an external IDL (protobuf, Smithy, TypeSpec, or similar). A code generator produces Rust types, traits, and clients. The IDL is the portable source; Rust is one of many targets.

**Advantages:**

* The IDL is language-neutral by design; every target language receives first-class support.
* gRPC, REST, and (with Smithy/TypeSpec) other protocols can be derived from the same source.
* Mature ecosystems (protobuf, buf, Smithy) provide stable tooling, schema evolution rules, and backwards-compatibility checks.

**Tradeoffs:**

* IDL-native versioning (protobuf field numbers, Smithy member indexes) is rigorous but imposes discipline.

**Disadvantages:**

* No IDL captures REST and gRPC faithfully simultaneously; forcing both into one model produces a lowest-common-denominator surface that is worse at being REST than a purpose-built REST API and worse at being gRPC than native gRPC (evidence report, 11 sources).
* Rust developers author contracts in a non-Rust language, losing the language's type system, trait system, and IDE integration.
* Generated Rust code is verbose, difficult to read, and impractical to hand-edit.
* Compile-time plugins must work against generated traits, creating a layer of indirection the current platform does not have.
* IDL build steps are slow (protoc, smithy-build) and complicate CI.
* Rust ecosystem idioms (newtype wrappers, `#[non_exhaustive]`, custom serde attributes) must be re-applied on top of generated code, often awkwardly.
* The team has Rust expertise rather than IDL expertise; adopting an IDL imposes a learning curve across the whole organization.

## More Information

* PRD: [`../PRD.md`](../PRD.md)
* DESIGN: [`../DESIGN.md`](../DESIGN.md)
* ADR-0002 — OpenAPI spec generation limits: [`./0002-cpt-cf-binding-adr-openapi-spec-limits.md`](./0002-cpt-cf-binding-adr-openapi-spec-limits.md)
* Evidence report on IDL unification: [rest-grpc-unification-evidence.html](https://github.com/striped-zebra-dev/modkit-binding-poc/blob/main/docs/research/rest-grpc-unification-evidence.html)
* PoC repository: [striped-zebra-dev/modkit-binding-poc](https://github.com/striped-zebra-dev/modkit-binding-poc)
* Module/plugin declaration and resolution: [PR #1380](https://github.com/cyberfabric/cyberfabric-core/pull/1380) (complementary, not alternative)
