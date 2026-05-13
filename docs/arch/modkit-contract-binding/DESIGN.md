# Technical Design — ModKit Contract Binding

## 1. Architecture Overview

### 1.1 Architectural Vision

The contract-binding system introduces a two-layer trait architecture for ModKit modules. The first layer is the **base trait** -- a plain Rust trait with zero annotations that defines the domain contract. The second layer is the **transport projection** -- a trait that extends the base and carries transport-specific annotations (HTTP paths, methods, streaming). A proc macro processes the projection and generates a REST client, OpenAPI spec, and any transport-specific logic.

Four **contract types** encode operational semantics directly in the trait name. The contract type determines the failure domain, transaction scope, timeout requirements, and error handling strategy. There is no configuration file, no annotation, and no runtime flag that overrides what the name declares.

- **Api** -- the module offers this across a boundary. Remote. Caller handles timeouts, retries, circuit breakers.
- **Embedded** -- the module offers this in-process. Local. Shares the caller's failure domain and transaction scope.
- **Backend** -- the module needs a plugin that operates across a boundary. Remote-capable. Transport projections provide the remote binding.
- **Extension** -- the module needs a plugin that operates in-process. Local. Fast, deterministic, no transport.

Consumers always depend on the base trait (`Arc<dyn NotificationBackend>`). Whether the underlying implementation is a compile-time plugin or a generated REST client is invisible to the consumer.

```text
  Module SDK crate
  ┌──────────────────────────────────────────────────────────────────┐
  │                                                                  │
  │  trait NotificationBackend          (base contract, zero annot.) │
  │    deliver()                                                     │
  │    stream_delivery()                                             │
  │                                                                  │
  │  #[modkit_rest_contract]                                         │
  │  trait NotificationBackendRest: NotificationBackend              │
  │    #[post("/v1/deliver")]           (transport projection)       │
  │    deliver()                                                     │
  │    #[streaming] #[post("/v1/delivery/stream")]                   │
  │    stream_delivery()                                             │
  │                                                                  │
  └────────────┬──────────────────────────────┬──────────────────────┘
               │                              │
               │  compile-time plugin         │  macro-generated
               │  impl NotificationBackend    │  NotificationBackendRestClient
               │  directly                    │  impl NotificationBackend (HTTP dispatch)
               v                              v
  ┌─────────────────────┐       ┌──────────────────────────────────┐
  │  In-process plugin  │       │  REST client (feature-gated)     │
  │  (inventory reg.)   │       │  + OpenAPI spec function         │
  └─────────────────────┘       │  + SSE stream support            │
                                │  + Retry with backoff            │
                                └──────────────────────────────────┘
               │                              │
               └──────────────┬───────────────┘
                              │
                              v
                 ┌─────────────────────────┐
                 │  ClientHub              │
                 │  Arc<dyn Backend>       │
                 │  (binding-mode agnostic)│
                 └─────────────────────────┘
                              │
                              v
                 ┌─────────────────────────┐
                 │  Module business logic  │
                 │  (same code regardless  │
                 │   of binding mode)      │
                 └─────────────────────────┘
```

### 1.2 Glossary

| Term | Definition |
|------|------------|
| Contract | A Rust trait that defines an interface between a module and its consumers or plugins. Always a plain trait with zero transport annotations. |
| Transport projection | A trait that extends a contract with transport-specific annotations (HTTP paths, methods, streaming). Generates a client and OpenAPI spec via proc macro. Named `{Base}Rest` or `{Base}Grpc`. |
| Api | A contract the module **offers** across a boundary. Caller assumes independent failure domain, timeouts, retries, error mapping. Cannot participate in the caller's ACID transaction. Trait name ends with `Api`. Example: `NotificationApi`. |
| Embedded | A contract the module **offers** in-process. Shares the caller's failure domain and can participate in the caller's transaction. Has lifecycle (start/stop), state, background workers. If it fails, the caller fails. No timeout/retry needed. Trait name ends with `Embedded`. Example: `EventProducerEmbedded`. |
| Backend | A contract the module **needs**, satisfied across a boundary. Same operational semantics as Api: independent failure domain, timeout, retry, circuit breaker. Cannot participate in the module's ACID transaction. Trait name ends with `Backend`. Example: `NotificationBackend`. |
| Extension | A contract the module **needs**, satisfied in-process. Shares the module's failure domain. Can participate in the module's transaction. Fast, deterministic, no retry. If it fails, the module fails. Trait name ends with `Extension`. Example: `NotificationFormatterExtension`. |
| Offers | The module implements the trait and serves it to consumers. |
| Needs | The module depends on the trait and expects a plugin to implement it. |
| Base trait | The first layer -- a plain Rust trait defining the domain contract with no transport annotations. |
| Projection trait | The second layer -- extends the base with transport annotations. Processed by a proc macro to generate client code. |
| Binding mode | Whether a contract is satisfied by a compile-time plugin or a generated REST/gRPC client. Determined by which traits exist, not by an annotation. |

### 1.3 Operational Semantics

The four contract types are distinguished by their operational semantics, not deployment topology. The following table defines the invariants that each contract type carries:

| Dimension | Local contracts (Embedded / Extension) | Remote-capable contracts (Api / Backend) |
|-----------|----------------------------------------|------------------------------------------|
| Transaction scope | Can participate in caller's ACID transaction | Cannot participate -- independent transaction boundary |
| Failure domain | Same as caller -- if it fails, you fail | Independent -- the callee can fail without crashing the caller |
| Timeout / retry | Not applicable -- in-process call | Required -- network may be slow or unreachable |
| Circuit breaker | Not applicable | Recommended -- protect against cascading failure |
| Error mapping | Rust errors directly | Problem Details over the wire, reconstructed via `ContractError` |
| Serialization | None (zero-copy, shared memory) | JSON / protobuf -- all data crosses a serialization boundary |
| Lifecycle | Shared with host process | Independent process with own lifecycle |
| Settings | Shared config context | Own config (URL, timeout, retry policy) via `ClientConfig` |
| Dependencies | Shared DI context (ClientHub) | Own connection / HTTP client |

### 1.4 Naming Convention Matrix

Every trait name ends with its contract type suffix. One glance at the name tells you the operational semantics.

```text
              Local (in-process,           Remote-capable (boundary,
               tx-aware, shared fate)       independent failure domain)
              ----------------------       ---------------------------

Offers        {Noun}Embedded               {Noun}Api
              EventProducerEmbedded        NotificationApi
              (outbox, workers,            NotificationApiRest
               can participate in tx)      NotificationApiGrpc (future)

Needs         {Noun}Extension              {Noun}Backend
              NotificationFmtExtension     NotificationBackend
              (fast, in caller's tx,       NotificationBackendRest
               no timeout)                 NotificationBackendGrpc (future)
```

**Hard rules:**
- Every trait name ends with `Api`, `Embedded`, `Backend`, or `Extension`
- Transport projections append `Rest` or `Grpc` to the base name
- `Api` means remote-capable -- always. There is no "local Api"
- `Embedded` means local -- always. There is no "remote Embedded"
- `Backend` means remote-capable -- a `*Rest` projection may exist
- `Extension` means local -- always. No transport projections

**Transaction participation is a real capability, not aspirational.**

The claim "Embedded and Extension can participate in the caller's transaction" is grounded in concrete Rust patterns that already work today — it does not depend on any new compile-time machinery. Specifically:

- The outbox pattern is a direct application: an `EventProducerEmbedded` implementation writes to an `outbox` table using the caller's `&mut sqlx::Transaction<'_>` (or `&mut sea_orm::DatabaseTransaction`). The write commits atomically with the caller's business data. A background worker later relays the outbox rows to the external broker. The in-process contract *is* the transactional boundary.
- `sqlx::Transaction<'a>`, `sea_orm::DatabaseTransaction`, `tokio_postgres::Transaction` — all of these are ordinary Rust types passed by mutable reference with a lifetime bound to the enclosing scope. A method signature `fn produce(&self, tx: &mut Transaction<'_>, event: &Event)` cannot be called without a transaction. Rust's type and lifetime systems do the enforcement without any platform-level magic.
- Closure-table libraries, materialized-path libraries, and any read operation that must observe the caller's uncommitted state rely on the same mechanism. They pass the transaction handle through the call chain.

Remote contracts cannot participate in a local database transaction because the remote process does not have access to the handle. A remote method signature cannot accept `&mut Transaction<'_>` — there is no way to marshal a transaction across a process boundary in this design. This is the structural reason Api/Backend cannot claim tx participation, and it is why the split exists.

**TxGuard (see §7) is a separate idea** — a proposed compile-time mechanism that would *forbid* remote calls inside a transaction scope, as opposed to merely *allowing* local calls to participate in one. The tx-participation capability is already real. TxGuard would add the inverse enforcement: "inside a transaction, no remote calls allowed." The two are complementary, not the same.

**Segregation is based on signature, not implementation freedom.**

The hard rules above are about what the **signature promises** to the caller, not about what implementations are allowed to do. A signature-level promise is a subset relation:

- **Remote-capable → local is allowed.** A remote signature (Api, Backend) promises the caller will get timeout handling, retry, error mapping, independent failure domain. An implementation is free to skip the network entirely and do the work in-process — the caller's code is still correct because the remote promise is a *superset* of local behavior. This is the in-process plugin scenario.

- **Local → remote is NOT allowed.** A local signature (Embedded, Extension) promises the caller zero serialization overhead, shared failure domain, the ability to pass transaction handles, synchronous or near-synchronous latency. An implementation cannot secretly call over the network without breaking these promises. The caller did not write defensive code because none was needed; a hidden network call introduces timeouts, partial failures, and serialization semantics the caller never consented to.

The four-type segregation encodes this asymmetry in the type system. A migration from Embedded to Api is intentional — it changes the caller's contract and every caller must acknowledge the new obligations. A migration from Api to Embedded is implementation-level and requires no caller changes. Code reviewers and static analysis can trust the trait name as a contract, not as a hint.

**Alternative considered: umbrella interface (Java EE `@Local`/`@Remote` pattern).** A single interface with both local and remote views. Rejected because the umbrella obscures the operational contract at the call site — a caller holding `Arc<dyn FooService>` cannot tell whether defensive code is needed. Four explicit types force the caller to know what they are calling.

### 1.5 Architecture Drivers

| Requirement | Design Response |
|-------------|-----------------|
| `cpt-cf-binding-fr-base-trait-purity` | Base traits are plain Rust with zero annotations. No transport, no macros, no binding modes. Compile-time plugins implement the base trait directly. |
| `cpt-cf-binding-fr-transport-projection` | Transport traits extend the base and carry HTTP annotations. The `#[modkit_rest_contract]` macro generates the REST client, OpenAPI spec, and SSE support. |
| `cpt-cf-binding-fr-compile-time-safety` | Redeclared methods in the transport trait are checked by the Rust compiler against the base trait signatures. Missing methods, wrong param types, wrong return types are caught at compile time. |
| `cpt-cf-binding-fr-contract-types` | Four contract types (Api, Embedded, Backend, Extension) encode operational semantics in the trait name suffix. The name IS the contract. |
| `cpt-cf-binding-fr-naming-convention` | Every trait ends with its contract type suffix. Transport projections append `Rest` or `Grpc`. Hard rules enforced by convention and future lint. |
| `cpt-cf-binding-fr-rest-client-gen` | `#[modkit_rest_contract]` generates a `{Trait}Client` struct implementing both the base trait (HTTP dispatch) and the transport trait (default delegation). |
| `cpt-cf-binding-fr-openapi-gen` | The macro generates an `{trait}_openapi_spec()` function returning a valid OpenAPI 3.1 spec with endpoint paths, HTTP methods, and JSON schemas (via `schemars`). |
| `cpt-cf-binding-fr-sse-streaming` | Methods annotated with `#[streaming]` generate SSE-aware client code: `Accept: text/event-stream` header, SSE parser into typed `Stream`. |
| `cpt-cf-binding-fr-retryable` | Methods annotated with `#[retryable]` generate retry logic with exponential backoff. Retry policy configured via `ClientConfig`. |
| `cpt-cf-binding-fr-contract-error` | `#[derive(ContractError)]` generates Problem Details conversion with `error_code` (UPPER_SNAKE_CASE from variant name) and `error_domain` (from attribute). Round-trip serialization preserves the original variant. |
| `cpt-cf-binding-fr-problem-details` | Runtime provides `ProblemDetails` struct for RFC 9457 wire format with `error_code` and `error_domain` extension fields. |
| `cpt-cf-binding-fr-client-config` | Runtime provides `ClientConfig` carrying base URL, timeout, and retry policy. Generated clients accept `ClientConfig` for construction. |
| `cpt-cf-binding-fr-feature-gated` | REST client and its dependencies (`reqwest`, `schemars`) are behind a `rest-client` feature flag. SDK crates without the feature compile with no HTTP dependencies. |
| `cpt-cf-binding-fr-directory-contract` | Service directory trait defined for GTS ID resolution and OpenAPI validation at registration. Implementation out of scope (cluster work). |
| `cpt-cf-binding-fr-openapi-validation` | Directory fetches `/.well-known/openapi.json` from remote services and validates endpoint presence, HTTP methods, and content types before registration. |
| `cpt-cf-binding-fr-clienthub-fallback` | ClientHub supports fallback resolution: compile-time registration takes priority, REST proxy instantiated from directory when no compile-time plugin exists. |
| `cpt-cf-binding-fr-proxy-wiring` | Module lifecycle includes a proxy wiring phase after plugin discovery and before post-init. REST proxies instantiated only for traits with no compile-time registration. |
| `cpt-cf-binding-fr-consumer-agnostic` | Consumer code is binding-mode-agnostic. `hub.get::<dyn NotificationBackend>()` works identically whether backed by a compile-time plugin or a REST proxy. |
| `cpt-cf-binding-fr-versioning` | `#[non_exhaustive]` on request/response structs. Default trait methods for new methods. Breaking changes require new major version. |

### 1.6 Architecture Layers

```text
  Module business logic
         │
         │  hub.get::<dyn NotificationBackend>()
         v
  ┌──────────────────────┐
  │  ClientHub           │  binding-mode agnostic resolution
  │  (fallback: compile  │
  │   → REST proxy)      │
  └──────┬───────────────┘
         │
    ┌────┴────────────────────────────────────┐
    │                                         │
    v                                         v
  ┌──────────────────┐           ┌─────────────────────────────┐
  │ Compile-time     │           │ REST proxy (generated)      │
  │ plugin           │           │ impl NotificationBackend    │
  │ impl Base trait  │           │ via HTTP dispatch           │
  └──────────────────┘           └──────────┬──────────────────┘
                                            │
                                            │ HTTP + JSON
                                            v
                                 ┌─────────────────────────────┐
                                 │ Remote service              │
                                 │ /.well-known/openapi.json   │
                                 │ validated by directory      │
                                 └─────────────────────────────┘
```

## 2. Principles & Constraints

### 2.1 Design Principles

#### Contract Type Encodes Operational Semantics

- [ ] `p1` - **ID**: `cpt-cf-binding-principle-contract-type-semantics`

The contract type suffix (Api, Embedded, Backend, Extension) is the operational contract. The name tells the caller what to expect: failure domain, transaction scope, timeout requirements. There is no configuration override. `Api` means remote -- always. `Extension` means local -- always. This is a hard rule, not a guideline.

**Decisions**: `cpt-cf-binding-decision-four-contract-types`

#### Base Trait Purity

- [ ] `p1` - **ID**: `cpt-cf-binding-principle-base-trait-purity`

Base traits carry zero transport annotations, zero macros, zero binding-mode awareness. They define the domain contract in pure Rust. A base trait is usable without any macro crate dependency. Compile-time plugins implement the base trait directly without pulling in HTTP, serialization, or schema libraries.

**Decisions**: `cpt-cf-binding-decision-two-layer-architecture`

#### Transport Is Additive

- [ ] `p1` - **ID**: `cpt-cf-binding-principle-transport-additive`

Adding a transport projection (`NotificationBackendRest`) is a non-breaking change. The base trait, all existing compile-time plugins, and all consumers are unaffected. Removing a transport projection is also non-breaking for consumers (they depend on the base trait). Transport is layered on top, never baked in.

**Decisions**: `cpt-cf-binding-decision-two-layer-architecture`

#### Structural Enforcement

- [ ] `p1` - **ID**: `cpt-cf-binding-principle-structural-enforcement`

The absence of a transport projection is a compile-time guarantee that the contract is local-only. If `NotificationFormatterExtension` has no `*Rest` trait, no REST client can be generated for it. The structure of the code enforces the constraint -- no runtime check, no configuration flag, no lint. An Extension with no projection is provably local.

**Decisions**: `cpt-cf-binding-decision-four-contract-types`

#### Consumer Binding-Mode Ignorance

- [ ] `p1` - **ID**: `cpt-cf-binding-principle-consumer-ignorance`

Consumer code never knows or cares whether the underlying implementation is a compile-time plugin or a REST proxy. The consumer depends on `Arc<dyn NotificationBackend>` and calls methods on it. ClientHub resolves the implementation. This enables swapping between binding modes without any consumer code change.

**Decisions**: `cpt-cf-binding-decision-clienthub-fallback`

### 2.2 Constraints

#### Compile-Time Signature Checking

- [ ] `p1` - **ID**: `cpt-cf-binding-constraint-signature-checking`

Every method redeclared in a transport projection must match the corresponding method in the base trait. Enforcement is two-tiered:

1. **Delegation-time check** (automatic): the macro generates a default method body `Base::method(self, params).await`. If the projection's signature diverges from the base, this delegation fails to type-check and the code does not compile. This catches most drift.

2. **Witness functions** (macro-generated, explicit): the macro also emits `const _` witness blocks that assert exact signature equality between projection and base methods. A witness block looks like:

```rust
const _: () = {
    fn _witness_deliver<T: NotificationBackendRest>(
        this: &T,
        req: &DeliverRequest,
    ) -> impl ::core::future::Future<Output = Result<DeliverResponse, GreeterError>> + '_ {
        <T as NotificationBackend>::deliver(this, req)
    }
};
```

If the projection's parameter types or return type diverge from the base, the witness fails with a clear error message naming the mismatch. This gives us true compile-time signature conformance — not just "catch-by-delegation."

**Note on the Rust trait system:** A `: Base` supertrait bound alone does NOT force signature equality for methods with shared names. Redeclared methods in the subtrait are distinct methods that happen to share a name with the base. The witness functions close this gap.

#### Feature-Gated HTTP Dependencies

- [ ] `p1` - **ID**: `cpt-cf-binding-constraint-feature-gated-http`

All HTTP dependencies (`reqwest`, `schemars`) are behind a `rest-client` Cargo feature flag. SDK crates without the feature compile with zero network dependencies. This ensures that compile-time-only consumers do not pay for transport they do not use.

#### RFC 9457 Error Wire Format

- [ ] `p1` - **ID**: `cpt-cf-binding-constraint-rfc9457-errors`

All error responses from generated REST clients use RFC 9457 Problem Details with the `error_code` and `error_domain` extension fields. `error_code` is UPPER_SNAKE_CASE derived from the Rust enum variant name. `error_domain` is a dot-separated module namespace. Round-trip serialization preserves the original error variant.

#### No Server Generation

- [ ] `p1` - **ID**: `cpt-cf-binding-constraint-no-server-gen`

Only the client is generated from the transport projection. Remote services implement their REST endpoints independently. The generated OpenAPI spec serves as the conformance contract validated by the directory. This preserves server flexibility -- services may extend the API, support version coexistence, and use any HTTP framework.

#### Async Trait Strategy

- [ ] `p1` - **ID**: `cpt-cf-binding-constraint-async-trait`

All contract traits use `#[async_trait]` from the `async-trait` crate. This includes base traits, transport projections, and manually-implemented client structs. Rationale:

- **Trait objects are required.** The entire design rests on consumers holding `Arc<dyn Backend>`. Native `async fn in Trait` (stable since Rust 1.75) is not dyn-compatible without boxing the returned future, which `async_trait` does transparently.
- **The macro generates `async_trait`-compatible code.** The generated REST client is annotated `#[async_trait::async_trait]` and boxes returned futures accordingly. Native `async fn` in the same trait would require `#[trait_variant]` or `#[allow(async_fn_in_trait)]` gymnastics and would not be dyn-compatible.
- **Consistency across the codebase.** ModKit SDK crates already use `async_trait`. This design keeps that convention.

The cost is one `Box<dyn Future>` allocation per async method call — negligible compared to HTTP serialization or network I/O. When native dyn-compatible `async fn` is stable and ergonomic, the platform can migrate, but that is not today.

#### Security Context on Remote Contracts

- [ ] `p1` - **ID**: `cpt-cf-binding-constraint-security-context`

Every method on a remote-capable contract (Api, Backend, and their `*Rest`/`*Grpc` projections) MUST accept `&SecurityContext` as its first non-self argument. This is a hard rule enforced by convention and validated by the macro and by CI.

```rust
// Remote-capable — SecurityContext required.
pub trait NotificationBackend: Send + Sync {
    async fn deliver(
        &self,
        ctx: &SecurityContext,            // required first arg
        req: &DeliverRequest,
    ) -> Result<DeliverResponse, NotificationError>;
}

// Local — SecurityContext optional (caller already has it in scope).
pub trait NotificationFormatterExtension: Send + Sync {
    async fn format(&self, message: &str, channel: &Channel) -> Result<String, FormatError>;
}
```

**Rationale:** Every cross-boundary call carries some authorization context — a bearer token, a service identity, a tenant scope. Making it the first parameter ensures:

1. Authors cannot forget it. Missing `ctx` is a compile error at every call site.
2. The macro generates consistent propagation. The generated REST client maps `ctx` into the appropriate transport-level carrier (Authorization header, metadata, etc.) in one place.
3. Code review and audit are mechanical. Every remote call has `ctx` visible in the signature; calls without it are locally scoped by construction.
4. Middleware composes cleanly. Layers that inject tenant context, validate tokens, or emit audit logs hook into a single, uniform slot.

Local contracts (Embedded, Extension) do not require `SecurityContext` because they run in the caller's scope and inherit the caller's context directly (task-local storage, explicit parameters, or module-owned state). Authors MAY pass `SecurityContext` explicitly to local methods when the contract needs it.

#### OpenAPI at Well-Known Path

- [ ] `p1` - **ID**: `cpt-cf-binding-constraint-openapi-well-known`

Remote services expose their OpenAPI spec at `/.well-known/openapi.json`. The service directory fetches and validates this spec at registration time. This is the single integration point between the generated contract and the remote implementation.

## 3. Key Decisions

### D1: Two-Layer Trait Architecture

- [ ] `p1` - **ID**: `cpt-cf-binding-decision-two-layer-architecture`

**Decision**: Separate base traits (domain contract, zero annotations) from transport projections (annotated traits extending the base). The Rust compiler checks that redeclared method signatures in the projection match the base trait.

**Rationale**: Decouples domain contracts from transport concerns. Compile-time plugins depend only on the base trait and never pull in HTTP or schema dependencies. Transport projections are additive and independently versionable.

**Alternatives considered**:

| Alternative | Why Rejected |
|-------------|-------------|
| Single-trait annotations (`#[modkit_contract(binding = [compile, rest])]`) | Mixes transport with domain. REST annotations do not apply to gRPC. Compile-time plugins carry unnecessary annotation weight. Cannot add a new transport without modifying the base trait. |
| Separate module for transport mapping (not a trait) | Loses compile-time signature checking. The mapping between base methods and HTTP endpoints would be a configuration file or a separate struct, not compiler-verified. A method rename in the base trait would silently break the mapping. |

### D2: Four Contract Types

- [ ] `p1` - **ID**: `cpt-cf-binding-decision-four-contract-types`

**Decision**: Four contract types -- Api (offers, remote), Embedded (offers, local), Backend (needs, remote-capable), Extension (needs, local). The suffix is the contract. The name encodes operational semantics.

**Rationale**: The distinction is not about deployment topology. It is about operational semantics. A `TokenValidator` doing local JWT parsing (microseconds, in your transaction) and one calling remote OAuth (network, timeout, own failure domain) have fundamentally different caller requirements. Collapsing to two types forces the caller to add timeout handling and retry for a local function call, or removes the signal that a remote call requires defensive code.

**Alternatives considered**:

| Alternative | Why Rejected |
|-------------|-------------|
| Two types (Api + Backend) | Collapses local and remote semantics. An `EventProducerEmbedded` with an internal outbox (participates in your DB transaction, commits atomically) gets the same contract as a remote HTTP ingestion endpoint. Retry logic wrapping a transactional commit is wrong. |
| No types (just traits) | No readability signal for operational semantics. The caller must read documentation or trace the implementation to know whether a call can timeout, whether it participates in a transaction, whether retry is needed. |

### D3: Transport Projection Default Delegation

- [ ] `p1` - **ID**: `cpt-cf-binding-decision-default-delegation`

**Decision**: The contract and its protocol projection are **two distinct Rust traits** in the SDK crate. The base trait (e.g., `NotificationBackend`) carries the domain contract with zero transport annotations. The protocol projection (e.g., `NotificationBackendRest`) **extends** the base via a `: Base` supertrait bound and redeclares each method with protocol-specific annotations (`#[post]`, `#[streaming]`, `#[retryable]`). The `#[modkit_rest_contract]` macro converts the redeclared methods into default methods that delegate to the base trait via fully-qualified call syntax: `Base::method(self, ...)`. The generated REST client provides a single HTTP dispatch implementation of the base trait; the projection trait's empty impl picks up the delegating defaults.

**Why two traits, not one**:

1. **The contract is the domain, the projection is the wire format**. A compile-time plugin or an in-process implementation depends only on the base trait. It does not import `reqwest`, `axum`, or any transport crate. The base trait is what consumers write against (`Arc<dyn NotificationBackend>`). Mixing HTTP path annotations and streaming markers into the domain trait would pollute the plugin author's world with transport they don't care about.

2. **The projection is protocol-specific**. Multiple projections can coexist on the same base: `NotificationBackendRest` (HTTP), `NotificationBackendGrpc` (protobuf, future). Each lives in its own file, feature-gated independently. Adding a projection is non-breaking — it introduces a new trait extending the existing base, leaves the base untouched, and generates its own client struct.

3. **Compile-time enforcement, not runtime convention**. Because the projection uses `: Base` as a supertrait, the Rust trait system enforces that implementors of the projection also implement the base. Because each method is redeclared, the compiler verifies that parameter types, return types, and error types in the projection match the base exactly. Drift between the two is impossible — the code will not compile.

4. **The generated client implements both traits**. The base impl is the real work (HTTP dispatch, error reconstruction, retry, SSE parsing). The projection impl is empty; it inherits the default methods the macro synthesized, which call back into the base. A consumer holding `Arc<dyn NotificationBackend>` gets direct HTTP dispatch. A caller who needs the protocol-specific surface (e.g., REST-only batch endpoints declared only in the projection) can also access it through the same concrete client type.

**What the developer writes vs what the macro emits:**

```rust
// 1. The contract — plain Rust trait, domain only, no transport awareness.
//    Lives in the SDK crate. This is what consumers and compile-time plugins depend on.
pub trait NotificationBackend: Send + Sync {
    async fn deliver(&self, req: &DeliverRequest) -> Result<DeliverResponse, Err>;
}

// 2. The protocol projection — extends the base with REST annotations.
//    Lives alongside the contract, feature-gated behind `rest-client` if needed.
#[modkit_rest_contract]
pub trait NotificationBackendRest: NotificationBackend {
    #[post("/v1/deliver")]
    async fn deliver(&self, req: &DeliverRequest) -> Result<DeliverResponse, Err>;
}
```

After macro expansion:

```rust
// The projection trait after the macro processes it — methods now have defaults
// that delegate to the base via fully-qualified call syntax.
pub trait NotificationBackendRest: NotificationBackend {
    async fn deliver(&self, req: &DeliverRequest) -> Result<DeliverResponse, Err> {
        NotificationBackend::deliver(self, req).await
    }
}

// The generated REST client struct carries config + HTTP client.
pub struct NotificationBackendRestClient {
    config: ClientConfig,   // base_url, timeout, retry policy
    http: reqwest::Client,
}

// Single source of actual HTTP dispatch — on the BASE trait.
impl NotificationBackend for NotificationBackendRestClient {
    async fn deliver(&self, req: &DeliverRequest) -> Result<DeliverResponse, Err> {
        let resp = self.http.post(format!("{}/v1/deliver", self.config.base_url))
            .json(req).send().await?;
        if !resp.status().is_success() { return Err(self.parse_error(resp).await); }
        resp.json().await.map_err(Err::from_transport)
    }
}

// The projection impl is EMPTY. Default methods inherited from the trait
// delegate back to NotificationBackend::deliver, which is the HTTP dispatch above.
impl NotificationBackendRest for NotificationBackendRestClient {}
```

**Three call scenarios, three perspectives:**

The three diagrams below each answer a different question. They share a common vocabulary: the **base trait** (`NotificationBackend`) is the domain contract, the **projection trait** (`NotificationBackendRest`) is the protocol surface, and `*Client` is the macro-generated struct.

---

**Scenario 1: Consumer holding a trait object.** *Shows that the consumer calls the base trait and never sees the REST transport — the same code works for compile-time and REST bindings.*

```
  // Consumer code (e.g., the notification module using a delivery plugin)
  let backend: Arc<dyn NotificationBackend> = hub.get();
  backend.deliver(&req).await;
            │
            │   dynamic dispatch through Arc<dyn NotificationBackend>
            v
  impl NotificationBackend for NotificationBackendRestClient
      reqwest POST /v1/deliver → response → deserialize → error mapping
      (this is the only place HTTP actually happens)
```

Perspective: **consumer**. Point: the consumer sees one trait. Whether the implementation behind the trait object is a compile-time plugin or a macro-generated REST client is invisible.

---

**Scenario 2: A caller that needs the protocol surface.** *Shows that calling through the projection trait ends up in the same base impl — the default delegation prevents code duplication.*

```
  // Caller holding the concrete generated client type (rare — usually tests or
  // code that needs protocol-specific methods declared only on the projection)
  let client: NotificationBackendRestClient = ...;
  <_ as NotificationBackendRest>::deliver(&client, &req).await;
            │
            │   dispatches to the projection trait
            v
  default method in NotificationBackendRest (synthesized by the macro):
      fn deliver(&self, req) -> ... {
          NotificationBackend::deliver(self, req).await
      }
            │
            │   delegates via fully-qualified call syntax
            v
  impl NotificationBackend for NotificationBackendRestClient
      (same HTTP POST /v1/deliver as Scenario 1 — ONE implementation)
```

Perspective: **code that uses the protocol-specific surface** (projection-only methods, testing hooks). Point: there is one real implementation of `deliver`, on the base trait. The projection trait's methods are thin delegating shims that the macro generates — no hand-written duplication, no drift.

---

**Scenario 3: A compile-time plugin.** *Shows that in-process plugins depend only on the base trait and know nothing about REST.*

```
  // Plugin crate — does NOT depend on reqwest, modkit-contract-runtime, or the
  // projection trait. Depends only on the SDK crate's base trait.
  struct InProcEmailPlugin { /* SMTP client, whatever */ }

  impl NotificationBackend for InProcEmailPlugin {
      async fn deliver(&self, req) -> ... {
          // direct function call — no serialization, no HTTP, no retry wrapper
      }
  }

  // At wiring time:
  hub.register::<dyn NotificationBackend>(Arc::new(InProcEmailPlugin::new()));
```

Perspective: **plugin author**. Point: the plugin implements the base trait like any regular Rust trait. It has no awareness of transport projections, no macro involvement, no REST dependencies. This is the zero-cost path.

**What this guarantees:**

- Consumer code is identical regardless of binding mode (`Arc<dyn NotificationBackend>` always).
- The contract author writes the domain trait once; the protocol author writes the projection once. Neither has to duplicate the other's work.
- Signature drift between the contract and the projection is a compile error.
- Adding a gRPC projection later is purely additive — a new trait `NotificationBackendGrpc: NotificationBackend`, a new macro `#[modkit_grpc_contract]`, a new generated client. The base trait and existing REST projection are untouched.
- A compile-time plugin that only implements the base trait works everywhere — because the projection trait's default delegation bridges the gap automatically.

**Manual implementation is always allowed.** The macro generates a default client for the common case (POST + JSON, SSE streaming, exponential-backoff retry). When a module needs behavior the macro does not express — complex path templates, query parameter composition, custom authentication, connection pooling with per-tenant routing, bespoke retry strategies, request signing — the author can write a hand-crafted client that implements the same base trait directly. The consumer still gets `Arc<dyn NotificationBackend>`; the generated client and the hand-written client are indistinguishable from the consumer's perspective. The macro is a convenience for the common case, not a lock-in. Hand-written clients can coexist with macro-generated ones in the same codebase.

### D4: ContractError Derive

- [ ] `p1` - **ID**: `cpt-cf-binding-decision-contract-error`

**Decision**: `#[derive(ContractError)]` on an error enum generates RFC 9457 Problem Details conversion with `error_code` (UPPER_SNAKE_CASE from variant name) and `error_domain` (from `#[contract_error(domain = "...")]` attribute). The generated code supports round-trip serialization: `to_problem_details()` and `from_problem_details()` preserve the original variant including all structured context fields.

**Rationale**: Machine-readable error reconstruction across module boundaries. The `error_code` + `error_domain` pair uniquely identifies the error variant. Unknown codes or domains fall back to an `Internal` variant, ensuring the system never panics on unrecognized errors.

```rust
#[derive(Debug, Clone, ContractError)]
#[contract_error(domain = "cf.notification")]
pub enum NotificationError {
    #[error(status = 404, problem_type = "not-found")]
    NotificationNotFound { notification_id: String },

    #[error(status = 503, problem_type = "service-unavailable")]
    DeliveryUnavailable { channel: String, retry_after_seconds: Option<u64> },

    #[error(status = 500, problem_type = "internal")]
    Internal { description: String },
}
```

Generated `error_code` values: `NOTIFICATION_NOT_FOUND`, `DELIVERY_UNAVAILABLE`, `INTERNAL`.

### D5: OpenAPI at /.well-known/openapi.json

- [ ] `p1` - **ID**: `cpt-cf-binding-decision-openapi-well-known`

**Decision**: Remote services expose their OpenAPI spec at `/.well-known/openapi.json`. The service directory validates this spec at registration time by checking endpoint presence, HTTP methods, and content types against the expected contract spec generated by the macro.

**Rationale**: A single, predictable discovery point for the contract spec. Validation at registration time catches mismatches before any request is routed, not at runtime when the first call fails. The generated spec serves as a minimum conformance contract -- remote services may extend the API with additional endpoints.

### D6: Naming Convention as Hard Rule

- [ ] `p1` - **ID**: `cpt-cf-binding-decision-naming-convention`

**Decision**: Every trait name ends with `Api`, `Embedded`, `Backend`, or `Extension`. Transport projections append `Rest` or `Grpc`. `Api` means remote -- always, with no exceptions. There is no "local Api".

**Rationale**: The name IS the operational contract. A developer reading `fn process(backend: &dyn NotificationBackend)` knows immediately: this can timeout, this can fail independently, this needs retry logic, this cannot participate in my transaction. No need to open another file, check a configuration, or read documentation. The naming convention eliminates an entire class of architectural misunderstandings.

**Enforcement**: Currently by convention. Future work may add a Dylint lint that rejects traits with incorrect suffixes or transport projections on Extension/Embedded types.

## 4. Crate Structure

| Crate | Type | Responsibility |
|-------|------|----------------|
| `cf-modkit-contract-macros` | proc-macro | `#[modkit_rest_contract]` -- generates REST client struct, OpenAPI spec function, SSE streaming, retryable methods. `#[derive(ContractError)]` -- generates Problem Details conversion with `error_code` + `error_domain`. Method annotations: `#[get]`, `#[post]`, `#[put]`, `#[delete]`, `#[patch]`. Parameter annotations: `#[path]`, `#[query]`, `#[header]`, `#[streaming]`, `#[retryable]`. |
| `cf-modkit-contract-runtime` | lib | `ProblemDetails` struct (RFC 9457 with extension fields). SSE stream parser (byte stream to typed events). `ClientConfig` (base URL, timeout, retry policy). `RetryConfig` and `with_retry()` helper for exponential backoff. |
| Module SDK crates (e.g., `notification-sdk`) | lib | Base traits (zero annotations, no macro dependency). Transport projection traits (behind `rest-client` feature). Feature-gated: `rest-client` enables `reqwest`, `schemars`, and the generated REST client. Without the feature, only the base trait is available. |
| `cf-modkit` (modified) | lib | ClientHub: fallback resolution (compile-time first, then REST proxy from directory). Module lifecycle: new proxy wiring phase after plugin discovery, before post-init. |
| `cf-modkit-macros` (modified) | proc-macro | Alignment with ADR-0004 module/plugin declaration macros. |

### SDK Crate Layout (per module)

```text
notification-sdk/
  src/
    lib.rs              -- re-exports
    types.rs            -- request/response structs (#[non_exhaustive])
    error.rs            -- #[derive(ContractError)] enum
    api.rs              -- NotificationApi (base) + NotificationApiRest (projection)
    backend.rs          -- NotificationBackend (base) + NotificationBackendRest (projection)
    extension.rs        -- NotificationFormatterExtension (base only, no projection)
  Cargo.toml
    [features]
    rest-client = ["reqwest", "schemars", "cf-modkit-contract-macros", "cf-modkit-contract-runtime"]
```

## 5. Contract Enforcement

Contract integrity is enforced at multiple levels, from compile-time through to service registration:

| Tier | When | Mechanism | What It Catches |
|------|------|-----------|-----------------|
| 1. Compile-time | `cargo build` | Rust trait system (`: Base` supertrait), typed enums, `#[non_exhaustive]` on request/response structs | Signature mismatches between base and projection, missing methods, wrong param/return/error types, direct struct construction outside the crate |
| 2. Macro-time | `cargo build` | `#[modkit_rest_contract]` macro validates that annotated methods cover the base trait surface | Missing REST annotations for base trait methods |
| 3. Test-time | `cargo test` | Round-trip tests for `ContractError` (serialize to Problem Details, deserialize back, assert variant match) | Error code drift, serialization schema changes, lost context fields |
| 4. Registration-time | Service boot | Directory fetches `/.well-known/openapi.json` and validates endpoint presence, HTTP methods, content types against the expected spec | Missing endpoints on remote services, wrong HTTP methods, content type mismatches |
| 5. Design-time | Architecture | Naming convention (suffix = operational semantics), structural enforcement (no projection = local-only) | Architectural misuse (calling a remote contract in a transaction, adding a projection to an Extension) |

## 6. Risks / Trade-offs

### [Risk] Method Redeclaration Is Duplication

Every base trait method must be redeclared in the transport projection with HTTP annotations. This is textual duplication.

**Mitigation**: The Rust compiler rejects mismatches immediately. The duplication is enforced, not accidental. If the base trait changes a signature, the projection fails to compile until updated. This is strictly safer than a mapping file or configuration that can silently drift.

### [Risk] Proc Macro Complexity

The `#[modkit_rest_contract]` macro must parse trait definitions, generate client structs, produce OpenAPI specs, handle SSE streaming, and implement retry logic. Proc macros are notoriously hard to debug.

**Mitigation**: The PoC (`modkit-binding-poc`) proves feasibility for the common patterns: POST/GET endpoints, streaming, retryable methods, ContractError round-trip. Edge cases (generics, lifetimes, complex associated types) are explicitly out of scope for phase 1.

### [Risk] schemars Dependency

OpenAPI schema generation requires `schemars` as a dependency on all request/response types. This adds a transitive dependency tree.

**Mitigation**: Feature-gated behind `rest-client`. Compile-time-only consumers never pull in `schemars`. The dependency is paid only by crates that actually generate or consume OpenAPI specs.

### [Trade-off] Four Types Add Naming Ceremony

Developers must choose the correct suffix for every trait. This adds cognitive overhead compared to "just write a trait."

**Justification**: Intentional friction. The name IS the operational contract. The alternative -- implicit semantics where you must trace the implementation to know if a call can timeout -- is worse for every subsequent reader of the code. The ceremony pays for itself on the first code review.

### [Trade-off] No Server Generation

Only the client is generated. Remote services must implement their REST endpoints manually.

**Justification**: Preserves server flexibility. Remote services may be written in any language, use any HTTP framework, support multiple API versions, and extend the API surface beyond what the contract specifies. The generated OpenAPI spec serves as a minimum conformance contract, not a straitjacket.

### [Trade-off] No gRPC in Phase 1

Only REST transport projections are supported. gRPC follows the same pattern but is deferred.

**Justification**: REST covers the immediate need (out-of-process plugins, third-party integrations). The two-layer architecture is transport-agnostic by design -- adding `#[modkit_grpc_contract]` later requires no changes to base traits, consumers, or the contract type system.

### [Constraint] Observability Hooks

- [ ] `p1` - **ID**: `cpt-cf-binding-constraint-observability`

The generated REST client carries retry, timeout, and error mapping. Without traces, metrics, and logs it is unusable in production. Bolting observability on later is an API break because the hook points live on `ClientConfig`. Observability is therefore wired in from day one.

**Tracing (spans).** Every generated method call opens a span that carries the method name (e.g., `NotificationBackendRest::deliver`), target URL, HTTP method, status code, retry attempt number, request duration, and a correlation ID propagated outbound via the `traceparent` header.

**Metrics.** The generated client emits: request count (labeled by method, status), request duration histogram, retry count, error count (labeled by `error_code`, `error_domain`), and an in-flight requests gauge.

**Structured logs.** Request initiated at `debug`, retry attempts at `warn`, errors with full Problem Details context at `error`.

**Where the hooks live.** `ClientConfig` gains an optional parent `tracing::Span` and an optional `MetricsRegistry` handle. A new `ContractObservability` trait abstracts all three channels so platforms on OpenTelemetry, Prometheus, or a bespoke stack can supply their own wiring. A default implementation backed by the `tracing` and `metrics` crates ships with the runtime.

```rust
// Runtime crate: cf-modkit-contract-runtime
pub struct ClientConfig {
    pub base_url: String,
    pub timeout: Duration,
    pub retry: RetryConfig,
    pub parent_span: Option<tracing::Span>,
    pub metrics: Option<MetricsRegistry>,
    pub observability: Option<Arc<dyn ContractObservability>>, // None = default impl
}

pub trait ContractObservability: Send + Sync {
    fn on_request_start(&self, method: &'static str, url: &str) -> RequestScope;
    fn on_retry(&self, scope: &RequestScope, attempt: u32, reason: &dyn std::fmt::Display);
    fn on_response(&self, scope: RequestScope, status: u16, duration: Duration);
    fn on_error(&self, scope: RequestScope, err: &ProblemDetails, duration: Duration);
}
```

The macro expands each method into a span-wrapped dispatch:

```rust
impl NotificationBackend for NotificationBackendRestClient {
    async fn deliver(&self, ctx: &SecurityContext, req: &DeliverRequest)
        -> Result<DeliverResponse, NotificationError>
    {
        let obs = self.config.observability();
        let scope = obs.on_request_start("NotificationBackendRest::deliver", &url);
        let span = tracing::info_span!(
            parent: self.config.parent_span.as_ref(),
            "NotificationBackendRest::deliver",
            http.method = "POST", http.url = %url,
            http.status_code = tracing::field::Empty,
            retry.attempt = tracing::field::Empty,
        );
        async move {
            tracing::debug!(?req, "request initiated");
            let started = Instant::now();
            match self.dispatch_with_retry(ctx, req, &scope, &span).await {
                Ok(resp) => { obs.on_response(scope, 200, started.elapsed()); Ok(resp) }
                Err(e)   => {
                    tracing::error!(error_code = %e.error_code(), error_domain = %e.error_domain(), "request failed");
                    obs.on_error(scope, &e.to_problem_details(), started.elapsed());
                    Err(e)
                }
            }
        }.instrument(span).await
    }
}
```

**Design decision.** Observability is not optional. The generated client always emits at least tracing spans. Metrics and structured logs can be silenced by supplying a no-op `ContractObservability`, but the hook points exist from day one. Adding them later would change `ClientConfig`'s public surface -- an API break for every consumer.

**Consumer transparency.** Consumers never interact with observability directly. `backend.deliver(&ctx, &req).await` automatically produces spans and metrics. Whether telemetry is enabled, disabled, or routed to a custom backend is a construction-time concern for `ClientConfig`; call-site code is unchanged.

## 7. Open Questions

### TxGuard -- Compile-Time Transaction Scope Restriction

A type-state mechanism that restricts which contracts can be called inside a transaction scope. Within a `TxGuard<'tx>`, only Embedded/Extension contracts are callable -- the compiler rejects calls to Api/Backend traits. This turns the operational semantics table from a naming convention into a compile-time guarantee.

```rust
async fn process_order(tx: &mut TxGuard<'_>, producer: &dyn EventProducerEmbedded) {
    // This compiles -- Embedded can participate in the transaction
    producer.produce_in_tx(tx, &event).await?;

    // This would NOT compile -- Backend cannot be called in a tx scope
    // payment_backend.charge(tx, &amount).await?;  // compile error
}
```

The guard would enforce that remote-capable contracts (Api, Backend) are never invoked within a transaction boundary, preventing a class of bugs where a remote call inside a transaction holds locks while waiting on the network. Needs its own ADR to design the type-state mechanism and how it interacts with database transactions (SeaORM/SQLx).

### Versioning and v1/v2 Coexistence

This design covers non-breaking evolution (`#[non_exhaustive]` types, default trait methods, new enum variants) but does not yet specify how Rust traits coexist across major versions. The reasonable strategies are:

- **Parallel traits**: `NotificationBackendV1` and `NotificationBackendV2` as separate traits. ClientHub resolves by type. Consumers choose at compile time. Plugins may implement both during a transition window.
- **Trait inheritance**: `trait NotificationBackendV2: NotificationBackendV1` with additive methods only. Works for extensions, fails for breaking changes.
- **Separate SDK crates per major version**: `notification-sdk-v1` and `notification-sdk-v2` published independently. Maximum isolation, maximum duplication.

The **remote-side requirement** is clear: remote services MUST preserve backwards compatibility within a major version. A service exposing `/v1/deliver` must continue to accept requests that the Rust `V1` trait generates for as long as `V1` is supported. This is an operational requirement, not a code-generation concern.

The Rust-side strategy is deferred to an ADR. Whichever strategy is chosen must support: simultaneous presence of V1 and V2 traits in the same SDK crate, a migration window where plugins implement both, and clear deprecation of V1 on a documented timeline.

### Remote Backend Unavailability

Circuit breakers, fallback methods, and degraded-mode behavior when remote plugins are temporarily unavailable. The `#[retryable]` annotation handles transient failures, but sustained unavailability (minutes, not seconds) requires a different strategy: circuit breaker state, fallback to a default implementation, or graceful degradation with cached data. Needs a separate ADR.

### gRPC Transport Projection

`#[modkit_grpc_contract]` macro design following the same two-layer pattern. Open questions include: proto-first vs. code-first generation, interaction with `tonic`, streaming semantics (server-streaming, client-streaming, bidirectional), and whether the gRPC projection can coexist with the REST projection on the same base trait.

### Complex REST Annotations

Path variables (`#[path]`), query parameters (`#[query]`), header injection (`#[header]`), and multi-part request bodies are supported in the annotation vocabulary but their full semantics need implementation-phase design. The PoC covers `#[post]` and `#[get]` with JSON body; path variable extraction and query parameter mapping are deferred.

### Method Annotation Naming Collision

`#[post]`, `#[get]`, `#[streaming]` are short attribute names that may collide with other proc macro crates. If collisions arise, the annotations may need namespacing: `#[modkit_post]`, `#[modkit_get]`, `#[modkit_streaming]`. The PoC uses the short names without issue, but production may require the longer forms depending on the dependency graph.

## 8. Traceability

- **PRD**: [`./PRD.md`](./PRD.md)
- **DESIGN** (this document): [`./DESIGN.md`](./DESIGN.md)
- **ADR-0001** — contract source of truth: [`./ADR/0001-cpt-cf-binding-adr-contract-source-of-truth.md`](./ADR/0001-cpt-cf-binding-adr-contract-source-of-truth.md)
- **ADR-0002** — OpenAPI spec limits: [`./ADR/0002-cpt-cf-binding-adr-openapi-spec-limits.md`](./ADR/0002-cpt-cf-binding-adr-openapi-spec-limits.md)
- **PoC**: [striped-zebra-dev/modkit-binding-poc](https://github.com/striped-zebra-dev/modkit-binding-poc)
- **Module/plugin declaration and resolution**: [PR #1380](https://github.com/cyberfabric/cyberfabric-core/pull/1380)
