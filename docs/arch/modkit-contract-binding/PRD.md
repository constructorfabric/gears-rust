# PRD -- ModKit Contract Binding System

## 1. Overview

### 1.1 Purpose

A two-layer contract-binding system for CyberFabric ModKit that separates module contracts (plain Rust traits) from their transport projections (transport-annotated traits), enabling modules to expose and consume interfaces across process boundaries without coupling domain logic to transport details.

### 1.2 Background / Problem Statement

ModKit modules define Rust traits as their extension points. Today, binding an implementation to a trait requires compiling it into the same binary -- a Cargo dependency on the SDK crate, a direct `impl Trait`, and registration into ClientHub via `inventory`. This works for in-process plugins but breaks when:

- The implementation lives in a separate process (out-of-process plugin, sidecar, external service).
- The implementation is written in another language.
- The implementer has an independent release cycle and cannot be compiled into the host binary.

There is no mechanism to generate REST clients from a trait definition, no way to validate that a remote service actually implements the expected contract, and no lifecycle phase in which the platform can wire REST proxies for unsatisfied traits. Each SDK crate that needs remote binding reinvents HTTP client code, error mapping, and retry logic independently.

### 1.3 Goals (Business Outcomes)

- Module developers define contracts once as plain Rust traits and get remote bindings (REST clients, OpenAPI specs) generated automatically.
- Consumers of a contract interact with it identically whether the backing implementation is compile-time or remote -- binding-mode-agnostic code.
- Four contract types with distinct operational semantics (Api, Embedded, Backend, Extension) make the caller's obligations explicit from the trait name alone.
- Compile-time enforcement prevents signature drift between base traits and transport projections.
- OpenAPI spec validation at registration time prevents incompatible remote services from entering the cluster.

### 1.4 Glossary

| Term | Definition |
|------|------------|
| Contract | A plain Rust trait with zero transport annotations that defines an interface between a module and its consumers or plugins. Always named with one of the four suffixes: `Api`, `Embedded`, `Backend`, `Extension`. |
| Transport projection | A trait that extends a contract with transport-specific annotations (HTTP paths, methods, streaming). Named `{Base}Rest` or `{Base}Grpc`. Generates a client implementation and OpenAPI spec via proc macro. |
| Api | A contract the module **offers** across a boundary. Trait name ends with `Api`. Caller assumes independent failure domain, timeouts, retries, error mapping. Cannot participate in the caller's ACID transaction. Remote always -- there is no "local Api". |
| Embedded | A contract the module **offers** in-process. Trait name ends with `Embedded`. Shares the caller's failure domain and can participate in the caller's transaction. Has managed lifecycle (start/stop, background workers). Local always -- there is no "remote Embedded". No transport projections. |
| Backend | A contract the module **needs**, satisfied across a boundary. Trait name ends with `Backend`. Same operational semantics as Api: independent failure domain, timeout, retry, circuit breaker. Cannot participate in the module's ACID transaction. Transport projections (`BackendRest`, `BackendGrpc`) provide the remote binding. |
| Extension | A contract the module **needs**, satisfied in-process. Trait name ends with `Extension`. Shares the module's failure domain. Can participate in the module's transaction. Fast, deterministic, no timeout. Local always -- no transport projections. |
| Offers | The module implements the trait and serves it to consumers. Contracts with `Api` or `Embedded` suffix. |
| Needs | The module depends on the trait and expects a plugin to implement it. Contracts with `Backend` or `Extension` suffix. |

## 2. Actors

### 2.1 Human Actors

#### Module Developer

**ID**: `cpt-cf-binding-actor-module-developer`

- **Role**: Defines base contract traits and transport projection traits for modules. Writes handler code that implements or consumes contracts.
- **Needs**: A simple, type-safe mechanism to define contracts once and get remote bindings generated without writing HTTP client code, error mapping, or retry logic by hand.

#### Plugin Developer

**ID**: `cpt-cf-binding-actor-plugin-developer`

- **Role**: Implements Backend or Extension contracts either as compile-time plugins or as standalone remote services.
- **Needs**: Clear contract definitions with compile-time enforcement, generated OpenAPI specs as the conformance target for remote implementations.

#### API Consumer

**ID**: `cpt-cf-binding-actor-api-consumer`

- **Role**: Calls module APIs exposed via transport projections.
- **Needs**: Consistent error responses (Problem Details), discoverable endpoints (OpenAPI), and reliable retry semantics.

### 2.2 System Actors

#### CI Pipeline

**ID**: `cpt-cf-binding-actor-ci-pipeline`

- **Role**: Runs automated checks on every PR to detect signature drift, schema changes, and contract violations.

#### Module Host Runtime

**ID**: `cpt-cf-binding-actor-host-runtime`

- **Role**: Executes the module lifecycle including plugin discovery, compile-time registration, proxy wiring, and post-init phases.

#### Service Directory

**ID**: `cpt-cf-binding-actor-service-directory`

- **Role**: Resolves GTS IDs to client configurations and validates remote service compatibility via OpenAPI spec comparison.

#### LLM Agent

**ID**: `cpt-cf-binding-actor-llm-agent`

- **Role**: Generates module code that defines or consumes contracts.
- **Needs**: Discoverable naming conventions, finite contract vocabulary, compile-time safety.

## 3. Operational Concept & Environment

The contract-binding system operates within the standard CyberFabric ModKit runtime. Contracts are defined at compile time as Rust traits. Transport projections generate REST clients and OpenAPI specs via proc macros during compilation. At runtime, the module host executes a lifecycle with a dedicated proxy wiring phase that instantiates REST proxies for unsatisfied Backend traits using the service directory.

The four contract types encode fundamentally different operational semantics that affect how callers write code:

```
                      Local contracts              Remote-capable contracts
                      (Embedded / Extension)       (Api / Backend)
----------------------------------------------------------------------
Transaction scope     can participate in caller's   cannot participate (ACID)
Failure domain        same as caller               independent
Timeout / retry       not applicable               required
Circuit breaker       not applicable               recommended
Error mapping         Rust errors directly          Problem Details over wire
Serialization         none (zero-copy)             JSON / protobuf
Lifecycle             shared with host             independent process
Settings              shared config                own config (url, timeout, retry)
Dependencies          shared DI context            own connection / client
```

Collapsing to fewer contract types would force callers into unnecessary defensive code (timeout/retry for local calls) or mask critical operational differences (transactional participation for remote calls).

## 4. Scope

### 4.1 In Scope

- Four contract types (Api, Embedded, Backend, Extension) with naming convention enforcement
- Base trait definition as plain Rust traits with zero transport annotations
- REST transport projection via `#[modkit_rest_contract]` proc macro
- REST client code generation implementing both base and transport traits
- OpenAPI 3.1 spec generation from transport projection traits
- SSE streaming support via `#[streaming]` annotation
- Retryable method support via `#[retryable]` annotation with exponential backoff
- Error mapping via `#[derive(ContractError)]` generating RFC 9457 Problem Details with `error_code` and `error_domain`
- Runtime support crate: `ProblemDetails` type, SSE parser, `ClientConfig`, `with_retry()` helper
- Service directory trait definition (interface only, implementation out of scope)
- OpenAPI spec validation at service registration time
- ClientHub fallback resolution: compile-time registration priority, REST proxy fallback
- Proxy wiring lifecycle phase in module host
- Feature-gated REST client dependencies (`rest-client` feature flag)
- `#[non_exhaustive]` on request/response structs for additive non-breaking changes
- Default trait methods for new transport projection methods

### 4.2 Out of Scope

- gRPC transport projection (`#[modkit_grpc_contract]`) -- future work, same pattern
- Service directory implementation -- delivered by cluster service discovery workstream
- Transaction guard (TxGuard) compile-time mechanism -- open design question, separate ADR
- Transaction context propagation for Embedded/Extension contracts -- separate ADR
- Circuit breaker implementation -- future work
- Remote backend unavailability / degraded-mode behavior -- future work
- Compile-only and REST-only enforcement at build/boot time -- future work

## 5. Functional Requirements

### 5.1 Contract Types & Naming Convention

#### Four Contract Types

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-four-contract-types`

The system MUST support exactly four contract type suffixes: `Api`, `Embedded`, `Backend`, and `Extension`. Every contract trait name MUST end with one of these suffixes. The suffix determines the operational semantics of the contract.

- **Rationale**: The four types encode fundamentally different caller obligations (transaction participation, failure domain, timeout/retry). The suffix makes these obligations readable from the trait name alone.
- **Actors**: `cpt-cf-binding-actor-module-developer`, `cpt-cf-binding-actor-llm-agent`

#### Naming Convention Matrix

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-naming-convention`

The system MUST enforce the following naming convention:

```
              Local (in-process,           Remote-capable (boundary,
               tx-aware, shared fate)       independent failure domain)
              ----------------------       ---------------------------
Offers        {Noun}Embedded               {Noun}Api
Needs         {Noun}Extension              {Noun}Backend
```

Transport projections MUST append `Rest` or `Grpc` to the base name (e.g., `NotificationBackendRest`, `NotificationApiRest`).

- **Rationale**: One glance at the trait name tells you the operational semantics -- no need to check other files or follow implicit conventions.
- **Actors**: `cpt-cf-binding-actor-module-developer`, `cpt-cf-binding-actor-plugin-developer`

#### Api Means Remote Always

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-api-remote-always`

A trait with `Api` suffix MUST represent a remote-capable contract. There is no "local Api". If the module offers an in-process interface, it MUST use the `Embedded` suffix instead.

- **Rationale**: Hard rule prevents ambiguity. When a caller sees `Api`, they know to write defensive code (timeout, retry, error mapping).
- **Actors**: `cpt-cf-binding-actor-module-developer`

#### Embedded Means Local Always

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-embedded-local-always`

A trait with `Embedded` suffix MUST represent an in-process contract. It MUST NOT have transport projections. The implementation shares the caller's failure domain and can participate in the caller's ACID transaction.

- **Rationale**: Hard rule prevents accidental remote calls inside transaction scopes.
- **Actors**: `cpt-cf-binding-actor-module-developer`

#### Extension Means Local Always

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-extension-local-always`

A trait with `Extension` suffix MUST represent an in-process contract. It MUST NOT have transport projections. The implementation shares the module's failure domain.

- **Rationale**: Extensions are fast, deterministic, in-process plugins (request transforms, credential resolution, message formatting). No transport overhead.
- **Actors**: `cpt-cf-binding-actor-module-developer`

### 5.2 Base Trait (Layer 1)

#### Base Trait as Plain Rust Trait

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-base-trait-plain`

Base contract traits MUST be plain Rust traits with zero transport annotations, zero macros, and zero binding-mode concerns. They define the domain contract -- what the module needs or provides.

- **Rationale**: Clean separation of domain logic from transport. Compile-time plugins implement the base trait directly. Consumers depend on `Arc<dyn Trait>`.
- **Actors**: `cpt-cf-binding-actor-module-developer`, `cpt-cf-binding-actor-plugin-developer`

#### Base Trait Send + Sync Bound

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-base-trait-send-sync`

All base contract traits MUST have `Send + Sync` supertraits to support sharing across async tasks and thread-safe registration in ClientHub.

- **Rationale**: Required for `Arc<dyn Trait>` usage in async contexts.
- **Actors**: `cpt-cf-binding-actor-module-developer`

### 5.3 Transport Projection (Layer 2)

#### REST Transport Projection Macro

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-rest-macro`

The `#[modkit_rest_contract]` proc macro SHALL generate a REST client struct and OpenAPI spec function from a trait that extends a base contract trait. When a trait annotated with `#[modkit_rest_contract]` extends a base trait (e.g., `trait FooApiRest: FooApi`), the macro SHALL generate a `FooApiRestClient` struct that implements both the base trait (with HTTP dispatch logic) and the transport trait.

- **Rationale**: Eliminates hand-written HTTP client code, ensures generated clients conform to the contract.
- **Actors**: `cpt-cf-binding-actor-module-developer`

#### Compiler-Checked Signature Conformance

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-signature-check`

Redeclared methods in the transport projection trait MUST be compile-time checked against the base trait signatures. If a transport projection redeclares a method with a different parameter type, return type, or error type than the base trait, the Rust compiler MUST reject the code with a type mismatch error.

- **Rationale**: Prevents silent signature drift between the domain contract and its transport binding.
- **Actors**: `cpt-cf-binding-actor-module-developer`, `cpt-cf-binding-actor-ci-pipeline`

#### Missing Method Handling

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-missing-method`

When a base trait has a method that is not redeclared in the transport projection, the macro SHALL either generate a default delegation or emit a compile error.

- **Rationale**: Ensures complete coverage -- no base trait method silently lacks a transport binding.
- **Actors**: `cpt-cf-binding-actor-module-developer`

#### HTTP Method and Path Annotations

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-http-annotations`

Transport projection methods MUST declare their HTTP method and path via annotations: `#[get(...)]`, `#[post(...)]`, `#[put(...)]`, `#[patch(...)]`, `#[delete(...)]`. The generated client SHALL dispatch to the configured base URL plus the declared path.

- **Rationale**: HTTP routing is explicit in the trait, not in configuration or convention.
- **Actors**: `cpt-cf-binding-actor-module-developer`

#### Parameter Annotations

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-param-annotations`

Transport projection methods MUST support parameter annotations: `#[path]`, `#[query]`, `#[header]`. Annotations are on the actual parameters with real types, not separate strings.

- **Rationale**: Type-safe parameter binding prevents runtime mismatches between path templates and parameter types.
- **Actors**: `cpt-cf-binding-actor-module-developer`

#### Annotation Stripping

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-annotation-stripping`

The macro MUST read transport annotations (`#[post(...)]`, `#[get(...)]`, `#[streaming]`, `#[retryable]`) for code generation and strip them from the emitted trait definition.

- **Rationale**: Annotations are metadata for the macro, not part of the trait's Rust interface.
- **Actors**: `cpt-cf-binding-actor-module-developer`

#### REST-Only Methods via Default Implementations

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-rest-only-methods`

The transport projection trait MAY add methods with default implementations that are not in the base trait (e.g., `deliver_batch` that delegates to `deliver` in a loop). These methods SHALL be available on the generated REST client. Compile-time plugins implementing only the base trait SHALL NOT be affected.

- **Rationale**: Enables REST-specific convenience endpoints (batch, pagination) without polluting the domain contract.
- **Actors**: `cpt-cf-binding-actor-module-developer`

### 5.4 OpenAPI Generation

#### OpenAPI 3.1 Spec Generation

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-openapi-generation`

The `#[modkit_rest_contract]` macro SHALL generate a function (e.g., `foo_api_rest_openapi_spec()`) returning a `serde_json::Value` with a valid OpenAPI 3.1 spec. The spec SHALL include endpoint paths, HTTP methods, request/response schemas (via `schemars`), and error schemas.

- **Rationale**: OpenAPI spec is the conformance target for remote implementations and enables spec validation at registration.
- **Actors**: `cpt-cf-binding-actor-module-developer`, `cpt-cf-binding-actor-service-directory`

#### OpenAPI Spec Validation at Registration

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-openapi-validation`

The service directory SHALL validate remote service compatibility at registration time by fetching `/.well-known/openapi.json` from the service and checking that all required endpoints exist with correct HTTP methods and content types. Registration SHALL be rejected if validation fails, with a clear error listing missing endpoints.

- **Rationale**: Prevents incompatible remote services from entering the cluster and causing runtime failures.
- **Actors**: `cpt-cf-binding-actor-service-directory`

#### Validation Report

- [ ] `p2` - **ID**: `cpt-cf-binding-fr-validation-report`

The service directory SHALL produce a detailed validation report listing each check (endpoint presence, HTTP method, content type) with pass/fail status and total checks passed.

- **Rationale**: Enables operators to diagnose registration failures quickly.
- **Actors**: `cpt-cf-binding-actor-service-directory`

#### Optional Endpoint Handling

- [ ] `p2` - **ID**: `cpt-cf-binding-fr-optional-endpoints`

Endpoints marked as optional (description contains "MAY omit") SHALL NOT cause registration failure when absent from the remote service's OpenAPI spec.

- **Rationale**: Allows progressive implementation of optional transport-only methods.
- **Actors**: `cpt-cf-binding-actor-service-directory`

### 5.5 Error Mapping

#### ContractError Derive Macro

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-contract-error-derive`

The `#[derive(ContractError)]` macro SHALL generate RFC 9457 Problem Details conversion from an annotated error enum. The error code SHALL be derived from the variant name in UPPER_SNAKE_CASE. The error domain SHALL be specified via `#[contract_error(domain = "...")]` attribute.

- **Rationale**: Consistent, machine-readable error responses across all module boundaries without hand-written conversion code.
- **Actors**: `cpt-cf-binding-actor-module-developer`

#### Error Round-Trip Serialization

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-error-round-trip`

A `ContractError` converted to Problem Details JSON via `to_problem_details()` and deserialized back via `from_problem_details()` MUST result in the same error variant with all structured context fields preserved.

- **Rationale**: Enables reliable error reconstruction on the client side without lossy manual mapping.
- **Actors**: `cpt-cf-binding-actor-api-consumer`

#### Unknown Error Fallback

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-unknown-error-fallback`

When `from_problem_details()` receives an `error_code` or `error_domain` that does not match any known variant, it SHALL return a fallback error variant (typically `Internal`) with the unknown code or domain in the description.

- **Rationale**: Graceful degradation when the server adds new error variants that the client does not yet know about.
- **Actors**: `cpt-cf-binding-actor-api-consumer`

#### Variant HTTP Status and Problem Type

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-variant-annotations`

Each error enum variant SHALL be annotated with HTTP status code and problem type URI suffix (e.g., `#[error(status = 404, problem_type = "not-found")]`). The macro SHALL generate `status_code()` and `problem_type()` methods from these annotations.

- **Rationale**: HTTP status mapping is explicit in the error definition, not in framework middleware.
- **Actors**: `cpt-cf-binding-actor-module-developer`

#### Structured Context in Problem Details

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-structured-context`

The `context` field in Problem Details SHALL carry the variant's associated data as a JSON object. Named fields SHALL be serialized to context, and `from_problem_details()` SHALL deserialize the context back into the variant's fields.

- **Rationale**: Machine-readable error context enables programmatic error handling beyond status code matching.
- **Actors**: `cpt-cf-binding-actor-api-consumer`

### 5.6 SSE Streaming

#### Streaming Method Generation

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-streaming`

Methods annotated with `#[streaming]` in a transport projection SHALL generate SSE-aware REST client code. The generated client SHALL send `Accept: text/event-stream` header, parse server-sent events into a native Rust `Stream`, and the OpenAPI spec SHALL declare the endpoint with `text/event-stream` content type. SSE comment lines (`:keepalive`) SHALL be silently discarded.

- **Rationale**: Streaming is a first-class transport concern for event-driven modules (chat, notifications, telemetry).
- **Actors**: `cpt-cf-binding-actor-module-developer`

### 5.7 Retryable Methods

#### Retryable Method Generation

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-retryable`

Methods annotated with `#[retryable]` SHALL generate retry logic with exponential backoff. On transient HTTP failures (429, 502, 503, 504), the generated client SHALL retry. Retry policy (max retries, base delay, max delay) SHALL be configured via `ClientConfig`. Methods NOT annotated with `#[retryable]` SHALL return errors immediately without retrying.

- **Rationale**: Retry semantics are a property of the method, not the caller. Declaring retryability at the contract level prevents inconsistent retry behavior across callers.
- **Actors**: `cpt-cf-binding-actor-module-developer`

### 5.8 Runtime Support

#### ProblemDetails Type

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-problem-details-type`

The runtime crate SHALL provide a `ProblemDetails` struct for RFC 9457 wire format with fields: `type`, `title`, `status`, `detail`, `error_code`, `error_domain`, `context` (when non-null), and `trace_id` (when present).

- **Rationale**: Shared type for generated client error deserialization and server error serialization.
- **Actors**: `cpt-cf-binding-actor-module-developer`, `cpt-cf-binding-actor-api-consumer`

#### SSE Stream Parser

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-sse-parser`

The runtime crate SHALL provide an SSE stream parser that converts a `reqwest` byte stream into typed deserialized events. The parser SHALL handle multi-line `data:` fields and silently discard comment lines.

- **Rationale**: Shared parser for all generated streaming clients, avoiding per-client SSE implementation.
- **Actors**: `cpt-cf-binding-actor-module-developer`

#### ClientConfig

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-client-config`

The runtime crate SHALL provide a `ClientConfig` carrying base URL, timeout, and retry policy (`RetryConfig`). Generated REST clients SHALL accept `ClientConfig` for construction (e.g., `FooApiRestClient::from_config(config)`).

- **Rationale**: Uniform configuration for all generated clients. Service directory produces `ClientConfig`, clients consume it.
- **Actors**: `cpt-cf-binding-actor-module-developer`, `cpt-cf-binding-actor-service-directory`

#### Retry Helper

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-retry-helper`

The runtime crate SHALL provide a `with_retry()` function implementing exponential backoff. It SHALL respect `max_retries` and `max_delay` from `RetryConfig`. Non-retryable errors SHALL fail immediately without waiting.

- **Rationale**: Shared retry logic for generated clients and manual usage.
- **Actors**: `cpt-cf-binding-actor-module-developer`

#### REST Client Feature Gate

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-feature-gate`

The generated REST client and its dependencies (`reqwest`, `schemars`) SHALL be behind a `rest-client` Cargo feature flag. Consumers depending on the SDK crate without the `rest-client` feature SHALL NOT compile HTTP dependencies.

- **Rationale**: Compile-time-only consumers should not pay for HTTP dependencies they do not use.
- **Actors**: `cpt-cf-binding-actor-module-developer`, `cpt-cf-binding-actor-plugin-developer`

### 5.9 ClientHub Fallback & Proxy Wiring

#### ClientHub Fallback Resolution

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-clienthub-fallback`

ClientHub SHALL support fallback resolution: compile-time registration takes priority. When no compile-time plugin exists for a Backend trait, the platform SHALL instantiate the generated REST client using the service directory's `ClientConfig` and register it as `Arc<dyn Trait>` in ClientHub.

- **Rationale**: Seamless transition from compile-time to remote binding without consumer code changes.
- **Actors**: `cpt-cf-binding-actor-host-runtime`

#### Missing Required Backend Fails Startup

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-missing-backend-fail`

When no compile-time plugin AND no service directory entry exist for a required Backend trait, the platform SHALL fail startup with a clear error naming the unsatisfied trait.

- **Rationale**: Fail-fast prevents runtime surprises from unsatisfied dependencies.
- **Actors**: `cpt-cf-binding-actor-host-runtime`

#### Proxy Wiring Lifecycle Phase

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-proxy-wiring-phase`

The module host runtime SHALL include a proxy wiring phase in the lifecycle: plugin discovery (inventory) -> compile-time registrations (init) -> proxy wiring -> post-init. Proxy wiring SHALL only instantiate REST proxies for traits with no compile-time registration.

- **Rationale**: Deterministic lifecycle ordering ensures compile-time plugins always take precedence.
- **Actors**: `cpt-cf-binding-actor-host-runtime`

#### Binding-Mode-Agnostic Consumer Code

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-binding-agnostic`

Consumers of a contract trait SHALL interact with it identically via `hub.get::<dyn Trait>()` regardless of whether the underlying implementation is compile-time or REST-based. The consumer SHALL NOT need to handle or be aware of the binding mode.

- **Rationale**: Decouples consumer code from deployment topology decisions.
- **Actors**: `cpt-cf-binding-actor-module-developer`

### 5.10 Service Directory Contract

#### Service Directory Trait

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-directory-trait`

The system SHALL define a trait for service directory resolution. Given a GTS ID (or prefix), the directory SHALL return a `ClientConfig` if a matching service is registered, or `None` otherwise. Implementation is out of scope -- delivered by cluster service discovery.

- **Rationale**: The binding system needs a well-defined interface to the directory without coupling to its implementation.
- **Actors**: `cpt-cf-binding-actor-service-directory`, `cpt-cf-binding-actor-host-runtime`

### 5.11 Versioning & Non-Exhaustive Types

#### Non-Exhaustive Request/Response Types

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-non-exhaustive`

All request and response structs used in contracts MUST be annotated with `#[non_exhaustive]`. Additive field changes are non-breaking. Breaking changes require a new major version.

- **Rationale**: Prevents downstream compile failures when new fields are added to shared types.
- **Actors**: `cpt-cf-binding-actor-module-developer`, `cpt-cf-binding-actor-plugin-developer`

#### Default Methods for New Transport Methods

- [ ] `p1` - **ID**: `cpt-cf-binding-fr-default-methods`

New methods added to transport projection traits MUST have default implementations. Existing plugins that implement only the base trait SHALL compile unchanged.

- **Rationale**: Additive transport methods must not break existing in-process plugins.
- **Actors**: `cpt-cf-binding-actor-module-developer`

## 6. Non-Functional Requirements

### 6.1 Module-Specific NFRs

#### Macro Expansion Transparency

- [ ] `p2` - **ID**: `cpt-cf-binding-nfr-macro-transparency`

The generated code from `#[modkit_rest_contract]` MUST be inspectable via `cargo expand`. Generated struct names, method names, and trait implementations MUST follow predictable naming conventions (e.g., `{Trait}Client` for the client struct).

- **Rationale**: Developers and LLM agents must be able to understand and debug generated code.

#### Feature-Gated Compile Time

- [ ] `p2` - **ID**: `cpt-cf-binding-nfr-compile-time`

SDK crates with the `rest-client` feature disabled MUST NOT incur additional compile time from HTTP-related dependencies (`reqwest`, `schemars`, `openapiv3`).

- **Rationale**: Compile-time-only consumers should have the same build performance as before the binding system is introduced.

## 7. Constraints & Assumptions

### 7.1 Constraints

- The system is built on CyberFabric's ModKit framework and uses its ClientHub, inventory-based plugin discovery, and module lifecycle.
- REST is the first transport. gRPC is a future transport following the same two-layer pattern.
- The service directory implementation is delivered by a separate workstream. This change defines only the interface trait.
- Alignment with ADR-0004 (PR #1380) module/plugin declaration macros is required.

### 7.2 Assumptions

- Module developers adopt the four-suffix naming convention for all new contract traits.
- Remote services expose `/.well-known/openapi.json` for spec validation.
- `schemars` can derive JSON schemas for all request/response types used in contracts.
- The module host runtime supports adding new lifecycle phases (proxy wiring) without breaking existing modules.

### 7.3 Open Design Questions

- **Transaction guard (TxGuard)**: A compile-time mechanism that restricts which contracts can be called inside a transaction scope. Within a `TxGuard<'tx>`, only Embedded/Extension contracts would be callable -- the compiler would reject calls to Api/Backend traits. This would enforce the operational semantics table at the type level, not just by naming convention. Needs its own ADR to design the type-state mechanism and interaction with database transactions (SeaORM/SQLx). Not a requirement for this change.
- **Remote backend unavailability**: Circuit breakers, fallback methods, degraded-mode behavior when remote plugins are temporarily down.
- **gRPC transport projection**: `#[modkit_grpc_contract]` macro design, proto generation approach, interaction with tonic.
- **Transaction context propagation**: How Embedded/Extension contracts receive and participate in the caller's transaction scope.

## 8. Prior Art

| Reference | Relevance |
|-----------|-----------|
| Working PoC: [striped-zebra-dev/modkit-binding-poc](https://github.com/striped-zebra-dev/modkit-binding-poc) | Validated the two-layer approach, transport projection generation, and ClientHub fallback resolution |
| Module/plugin declaration and resolution: [PR #1380](https://github.com/cyberfabric/cyberfabric-core/pull/1380) | Typed module/plugin resolution -- the binding system complements this |
| WCF (Windows Communication Foundation) | Two-layer contract/binding model: service contract (interface) + binding (transport). Similar separation of domain interface from transport projection |
| OSGi Remote Services | Service interfaces published locally, discovered and proxied remotely via generated stubs. Similar compile-time-first with remote fallback pattern |
| Hexagonal Architecture (Ports and Adapters) | Contracts as ports, transport projections as adapters. The base trait is the port; the REST projection is an adapter |

## 9. Traceability

- **PRD** (this document): [`./PRD.md`](./PRD.md)
- **Design**: [`./DESIGN.md`](./DESIGN.md)
- **ADR-0001** — contract source of truth: [`./ADR/0001-cpt-cf-binding-adr-contract-source-of-truth.md`](./ADR/0001-cpt-cf-binding-adr-contract-source-of-truth.md)
- **ADR-0002** — OpenAPI spec limits: [`./ADR/0002-cpt-cf-binding-adr-openapi-spec-limits.md`](./ADR/0002-cpt-cf-binding-adr-openapi-spec-limits.md)
- **PoC**: [striped-zebra-dev/modkit-binding-poc](https://github.com/striped-zebra-dev/modkit-binding-poc)
