---
status: accepted
date: 2026-10-31
---

# Compose Generated REST Clients with Typed Manual Method Handlers

**ID**: `cpt-cf-binding-adr-manual-client-methods`

## Table of Contents

1. [Context and Problem Statement](#context-and-problem-statement)
2. [Decision Drivers](#decision-drivers)
3. [Considered Options](#considered-options)
4. [Decision Outcome](#decision-outcome)
5. [Pros and Cons of the Options](#pros-and-cons-of-the-options)
6. [More Information](#more-information)

## Context and Problem Statement

ADR-0002 keeps REST generation deliberately narrow and allows a hand-written
`impl Base for CustomClient` for other protocols. Today that escape hatch
replaces the whole client: `rest_contract.rs::generate_client_impl` emits one
complete base-trait implementation. Rust does not allow a second implementation
to supply just the methods omitted from the first. `#[server_manual]` supports
per-method server customization, but has no client counterpart.

OAGW exposes the gap. Its SDK combines ordinary management operations with
`proxy_request(SecurityContext, Request<Body>) -> Result<Response<Body>, CanonicalError>`.
Management operations fit JSON generation. The proxy operation needs the HTTP
method, path, query, status, headers and opaque body stream preserved; its adapter
also restores OAGW's error-source marker and distinguishes gateway errors from
provider responses. The generated typed SSE parser would change that protocol.

Adding only a `#[raw_http]` generator mode would still leave OAGW-specific error
handling without a composition point. Reimplementing every management method in
a custom client would duplicate code the generator already supplies.

This ADR proposes a general composition mechanism first and a shared raw HTTP
runtime primitive for its initial proxy use case. It does not implement these
features, decide the gateway-to-OAGW identity exchange, or cover WebSocket
upgrades.

## Decision Drivers

* Keep one base-trait implementation and one `Arc<dyn Base>` for consumers.
* Inject stateful dependencies explicitly, with compiler-checked signatures.
* Reuse discovery, credentials, tracing and transport configuration.
* Keep manual methods visible to contract coverage and transport validation.
* Preserve existing constructors, feature gates and generated method behavior.
* Keep SDK constructors and generated wiring source-compatible when the
  handler needs no state.
* Keep manual handler implementations inside the SDK in the first version.
* Keep provider-specific protocols and errors in their SDK adapters.

## Considered Options

* **Option A**: Add raw HTTP passthrough directly to the generator as its next
  built-in serialization mode.
* **Option B**: Add per-method manual handlers through typed constructor
  injection, supported by shared runtime primitives.
* **Option C**: Keep replacing the entire client with a hand-written base-trait
  implementation, optionally wrapping generated clients for smaller contracts.
* **Option D**: Register per-method callbacks dynamically by contract/method name.

## Decision Outcome

Chosen option: **Option B — typed composition, with a generic handler parameter,
a sealed SDK-owned handler trait, an optional SDK-chosen default handler and
explicit constructor injection.** This extends the manual escape hatch of
ADR-0002; it preserves the two-layer design of ADR-0001 and ADR-0003.

### 1. One generated implementation, with explicit delegation

A new bare projection marker, `#[client_manual]`, selects manual client dispatch
for that method. The marker belongs on the REST projection, not the base trait.
It does not imply `#[server_manual]`; the two directions remain independent.

For a projection containing manual methods, the macro generates:

* A public, sealed `{Projection}Manual: Send + Sync` trait containing exactly
  those methods, with no default implementations. A supertrait in a crate-visible
  generated module allows implementations inside the SDK but cannot be named
  outside it. Each method signature preserves the base method's arguments and
  return shape, with an additional borrowed runtime call context.
* A `{Projection}Client<H: ?Sized>` storing an `Arc<H>`. The struct carries no
  trait bounds; every bound sits on its `impl` blocks.
* A single `impl<H: {Projection}Manual + ?Sized> Base for {Projection}Client<H>`:
  ordinary methods use generated dispatch; manual methods call the corresponding
  method on `H`.
* Matching projection and directory-resolving implementations with the same
  handler bound. Immediate and fallible streaming signatures keep their existing
  distinction; an opaque HTTP response body is not a typed SSE method.
* Manual counterparts of the existing constructors, each taking an additional
  `Arc<H>`: `with_manual` for `new` and the resolving `new`, and
  `with_http_client_and_manual` for `with_http_client`.

A projection may name a default handler:
`#[toolkit::rest_contract(..., manual_handler = path::Type)]`, where
`Type: {Projection}Manual + Default`. The macro then:

* defaults the type parameter to it: `{Projection}Client<H: ?Sized = Type>`, and
  the same for the resolving client;
* generates `new` and `with_http_client`, and the resolving `new`, in an `impl`
  for the default type only, with their current signatures.

This follows `HashMap<K, V, S = RandomState>`, where `new` exists only for the
default hasher and `with_hasher` accepts any.

The following is a schematic API, not syntax available in the current release:

```rust
// Generated only for a projection with #[client_manual] methods.
pub(crate) mod egress_api_rest_manual_sealed {
    pub trait Sealed {}
}

#[async_trait::async_trait]
pub trait EgressApiRestManual: egress_api_rest_manual_sealed::Sealed + Send + Sync {
    async fn proxy_request(
        &self,
        call: &ManualCallContext,
        ctx: SecurityContext,
        req: http::Request<Body>,
    ) -> Result<http::Response<Body>, CanonicalError>;
}

// SDK code implements both traits for its exported handler types.
impl egress_api_rest_manual_sealed::Sealed for OagwProxy {}

// Generated for `manual_handler = crate::rest::OagwProxy`.
pub struct EgressApiRestClient<H: ?Sized = OagwProxy> {
    http: toolkit_http::HttpClient,
    config: ClientConfig,
    manual: Arc<H>,
}

impl EgressApiRestClient {
    pub fn new(config: ClientConfig) -> Result<Self, toolkit_http::HttpError> { /* ... */ }
}

impl<H: EgressApiRestManual + ?Sized> EgressApiRestClient<H> {
    pub fn with_manual(
        config: ClientConfig,
        manual: Arc<H>,
    ) -> Result<Self, toolkit_http::HttpError> { /* ... */ }
}

#[async_trait::async_trait]
impl<H: EgressApiRestManual + ?Sized> EgressApi for EgressApiRestClient<H> { /* ... */ }

// Default handler: existing call sites and wiring expansions are unchanged.
let api: Arc<dyn EgressApi> = Arc::new(EgressApiRestClient::new(config.clone())?);

// Application code configures a handler supplied by the SDK; H is inferred.
let handler = Arc::new(OagwProxy::new(proxy_options));
let api: Arc<dyn EgressApi> = Arc::new(EgressApiRestClient::with_manual(
    config.clone(), Arc::clone(&handler),
)?);

// Runtime selection also uses SDK-supplied implementations.
let handler: Arc<dyn EgressApiRestManual> = handler;
let api: Arc<dyn EgressApi> = Arc::new(EgressApiRestClient::with_manual(config, handler)?);
```

`Body` above denotes the SDK's body type; Toolkit does not depend on OAGW's SDK.
`ManualCallContext` is a new runtime type; today's generated client holds only
`http` and `config`.
Existing OAGW traits still need normal contract adoption, including the naming
rules; this ADR does not silently rename `ServiceGatewayClientV1`.

The handler trait does not extend the base trait: that would require the manual
handler to implement the generated methods too and risk recursive delegation.
The generator never reads a hand-written projection default body as the handler.

### 2. Ownership and injection in Rust

The concrete handler type is a generic parameter, rather than a mandatory trait
object or an `Any` service locator. One handler object covers all manual methods
in a projection. This keeps related dependencies in an ordinary SDK-owned Rust
struct and avoids a generic parameter or callback slot per method.

Applications may configure and choose among the SDK's exported handlers, but
cannot implement `{Projection}Manual` for their own types, including external
test doubles. The sealing supertrait has no blanket implementation for arbitrary
types. `with_manual` remains public; it injects an SDK implementation rather than
opening a downstream implementation point. The existing option to implement the
whole base contract on a custom client remains available; the base trait is not
sealed by this decision.

The SDK's own integration tests are outside the crate too: each file under
`tests/` builds as a separate crate and cannot implement the sealed trait.
Unit-test handlers live in `src/` under `#[cfg(test)]`. Integration tests link
the library without that cfg, so they use SDK-exported handlers behind an
explicitly enabled, dedicated test feature that production builds do not enable.

`Arc<H>` gives endpoint-specific client instances shared ownership without
requiring `H: Clone`. `H = dyn {Projection}Manual` is also supported through
`?Sized` when the application needs runtime selection.

The struct declares only `?Sized`, the one relaxation needed to define it. Trait
bounds on the struct would have to be repeated at every use of the type and add
nothing (Rust API Guidelines, C-STRUCT-BOUNDS). `Clone` or `Debug` impls, if
needed, are written by hand. A manual `Clone` clones the `Arc` without requiring
`H: Clone`, unlike a derived implementation. `Arc<H>: Debug` does require
`H: Debug`; to avoid that bound, a manual client `Debug` omits the handler and
uses `finish_non_exhaustive`.

A type parameter default applies where the type is named, not to inference in
expressions. `with_manual(config, handler)` infers `H` from `handler`; the bare
type name `EgressApiRestClient` means the default handler;
`EgressApiRestClient::new(config)` resolves because `new` exists only for the
default type. The generated trait uses
the existing `async_trait` convention; this choice does not introduce native
async-trait object-safety requirements or remove existing future allocations.
`'static` is required where ownership crosses into stored factory closures or
`ClientHub`, not on every borrowed method argument.

Mixed clients always hold a real handler. A default handler is an implementation
the SDK names explicitly, not an empty stub; `Default` is required only of that
named type. There is no runtime "method not implemented" fallback. A projection
without `manual_handler` has no type parameter default and no `new`; its clients
are built only through `with_manual`. Missing or mismatched handler methods fail
to compile. Projections without `#[client_manual]` retain their current
non-generic client types and constructors.

### 3. Discovery and wiring preserve the handler

The existing `DirectoryResolvingClient<C>` already accepts a capturing
`Fn(ClientConfig) -> Result<C, TransportError>`. The generated mixed wrapper
captures an `Arc<H>` in that factory. Each endpoint rebuild clones the `Arc`,
constructs a new transport context and keeps the same handler state. Credentials
and tenant context are supplied per call, never cached in that shared handler.

The rebuild callback stays synchronous: it runs under the resolver's build lock
and must not perform network I/O or initialize application dependencies.
In-flight streams retain the resources they need when the cached client changes.
The handler receives the endpoint for the current call rather than retaining a
previously resolved URL.

`#[toolkit::provides]` currently invokes `RestClient::new(config)`;
`#[toolkit::consumes]` invokes `RestResolvingClient::new(resolver, from, tuning)`.
With a default handler both expansions compile unchanged, because `new` keeps
its signature on the default type. This covers OAGW: its adapter reads
`x-oagw-error-source` and rebuilds canonical errors. It holds only immutable
configuration, such as the `proxy_options` in the example above, and no per-call
or dependency state; `Default` supplies the default configuration.

A stateful handler source in generated wiring is deferred until the first use
case needs it. When introduced, the following constraints apply:

* `#[toolkit::consumes]` registers a plain `WireFn` pointer through `inventory`.
  It is synchronous, captures nothing and receives only `&ClientHub`, the
  endpoint resolver and the tuning. The source is therefore a function path with
  a fixed signature, `fn(&ClientHub) -> anyhow::Result<Arc<H>>`, not a closure.
  For `consumes`, it runs in the proxy-wiring phase, after every gear's `init`,
  and can use dependencies registered during those initializations.
* `#[toolkit::provides]` calls the same source with `ctx.client_hub()` from
  inside the provider gear's `init`. It may rely only on dependencies declared
  by that gear and already registered by gears initialized before it. There is
  no all-gears-initialized guarantee on this path. The SDK runtime does not
  depend on `GearCtx`.
* `ClientHub` holds one remote proxy per contract per process, and a later
  consumer reuses the proxy an earlier one registered. A handler chosen per
  consumer would let the first consumer's choice win silently. The source is
  declared once per contract, on the projection (`manual_handler_factory = path`),
  not in `#[consumes]`.

Implement the stateful source when the first stateful handler needs it. Until
then, a projection without `manual_handler` cannot use the default `provides` or
`consumes` expansion: it fails to compile because `new` does not exist, and the
author registers the client by hand. A source that cannot build its handler is a
startup wiring error on both paths, including a required dependency absent from
the hub. Do not silently install a stub or wait for a later lifecycle phase.
Existing client-type overrides and default constructor paths remain valid for
ordinary clients.

Preserve local-client precedence, remote-proxy reuse, static endpoint overrides,
readiness behavior and per-process credential-provider injection from ADR-0004.
Do not add a global callback registry, downcasts, or configuration strings naming
Rust implementation types. Factories are assembly points, not per-call dispatch.

### 4. Shared call context and raw HTTP primitive

The runtime call context supplies the current endpoint, method identity,
transport tuning and common request execution helpers. Manual handlers own their
encoding and response interpretation; shared helpers own destination binding,
credential attachment, request/trace propagation and transport execution.
Returning a stream must not retain a borrow of the call context: it owns or
clones the resources required for subsequent polling.

The raw helper sends an opaque HTTP body and returns the response head plus an
opaque body. Both bodies use a Toolkit-owned type that is `Send` but not `Sync`,
such as `http_body_util::combinators::UnsyncBoxBody<Bytes, BoxError>`: OAGW's
`Body::Stream` is `Send` only. The SDK adapter converts its `Body` at the
boundary. Its contract is:

* Preserve status, end-to-end headers, ordered payload bytes and trailers,
  except the trailer fields `toolkit-http-proxy` already drops (framing,
  connection-scoped fields, `Host`). HTTP chunk boundaries are not part of the
  contract. Perform only protocol-required framing and hop-by-hop header
  normalization.
* Reject upgrade requests (`Connection: upgrade` with `Upgrade`, or `CONNECT`)
  with an explicit error. Hop-by-hop normalization would otherwise strip the
  upgrade headers and send a plain request.
* Stream requests and responses with backpressure and bounded internal buffers.
  Do not collect the whole body, parse SSE/JSON, or automatically decompress it.
* Return non-2xx responses intact. Failures before response headers are transport
  errors; subsequent read failures are body errors, never a clean EOF.
* Use the configured service endpoint. A caller-supplied URI authority or `Host`
  cannot redirect credentials. Replace conflicting auth carriers from the raw
  request using the method's security plane; mark credentials sensitive.
* Apply the existing tenant/platform plane rules. Manual dispatch does not
  authenticate a serialized `SecurityContext` or invent an identity exchange.
* Bound opening the exchange and apply the configured body/deadline policy
  separately. Cancelling the open or dropping the returned body releases the
  exchange: HTTP/1.1 may close its connection; HTTP/2 resets the affected stream.
* Do not automatically follow redirects, replay requests, retry statuses, or
  reconnect streams. Enforce this through the underlying transport too;
  disabling only the contract-level retry wrapper is insufficient.

Reject `#[retryable]` on the initial manual-client path rather than silently
ignoring it. A future replay facility must require explicit replayability and
idempotency. OAGW's adapter uses the raw helper and then reconstructs
`ErrorSource` and canonical gateway errors; those rules do not enter Toolkit.

WebSocket is out of scope. OAGW's `proxy_request` also carries WebSocket, with
`Body::Stream` in both directions. That needs a `101 Switching Protocols`
exchange and an upgraded connection, or extended CONNECT on HTTP/2 (RFC 8441).
`toolkit-http-proxy` keeps upgrades in a separate API, `ProxyClient::upgrade`,
for the same reason. A remote WebSocket path needs its own upgrade primitive and
ADR; until then the OAGW REST adapter returns an error for upgrade requests.

The current `toolkit-http` service takes `Request<Full<Bytes>>` and its builder
installs decompression and redirect layers. It cannot yet satisfy this raw
contract unchanged. `toolkit-http-proxy` already supports streaming request
bodies, sanitization and body-drop cancellation, but has a different policy stack.
Implement the raw helper on a shared Toolkit transport foundation, reusing that
machinery where appropriate; preserve auth, TLS, tracing, metrics and tuning
explicitly. This ADR does not require replacing the normal HTTP client or adding
a second unconfigured Hyper client inside each SDK.

### 5. Metadata, server generation and compatibility

Manual dispatch changes who implements a method, not whether it is in the
contract. Keep it in base/projection parity, coverage, descriptors and binding
validation. A method marked both `#[client_manual]` and `#[server_manual]` needs
no `Serialize`, `Deserialize`, `QueryParams` or JSON-schema bounds on its types.
A method marked only `#[client_manual]` still gets a generated server route, so
its types keep the bounds that route requires.

Bindings for fixed-method manual operations can retain existing path/verb
metadata: the parser requires exactly one verb attribute per method, which a
fixed-route manual method satisfies and OAGW's proxy does not. OAGW additionally
needs explicit binding metadata for an allowed HTTP method set, a wildcard path
and opaque request/response bodies. The current single-verb JSON binding cannot
represent that honestly. Extend the IR and its
validators before exposing this operation through generated wiring; never label
it as a fictitious `POST + JSON` endpoint or drop it from validation.

Opaque operations use manual server registration and its OpenAPI descriptions.
Do not infer a JSON success schema or complete proxy protocol from their Rust
types. Consumers of unsupported binding kinds must report that limitation rather
than accept invented metadata. This preserves ADR-0002's minimum-conformance
scope; it does not fix ADR-0004's existing lack of a discovery-time conformance
handshake.

`HttpBindingIr` and `HttpMethodBindingIr` are public structs constructed with
literals, including from macro expansions and hand-written SDK bindings. Adding
fields is a Rust source change even with `#[serde(default)]`. Evolve this boundary
with a compatible representation or an explicitly coordinated version change;
cover old serialized IR separately from Rust construction compatibility.

Keep handler code, transport contexts and HTTP dependencies behind `rest-client`.
Base-only and server-only builds remain independent of manual client handlers.
Existing typed SSE/multipart framing, error conversion and gRPC projections retain
their behavior. A later `#[raw_http]` convenience may generate the common adapter
after the runtime contract is proven; it is not a prerequisite for this design.

### Consequences

* SDK authors implement exceptional methods while retaining generated ordinary
  methods and a single consumer interface.
* Generic client and handler-trait symbols become public SDK API for mixed
  projections; their naming, constructors and feature gates require compile tests.
* Adding `#[client_manual]` to an existing projection that names a
  `manual_handler` can be a compatible change: the client gains a defaulted type
  parameter, and `new` and `with_http_client` keep their signatures. Existing
  method behavior and public API guarantees must also be preserved. Without
  `manual_handler` it removes `new`, which is a breaking change.
* `{Projection}Manual` follows the projection's contract major version, with no
  independent handler version. For example, `FooApiV2Rest` has its own
  `FooApiV2RestManual`. Its implementation set is owned by the SDK, while its
  publicly callable surface remains part of the SDK's Rust API.
* ADR-0007 permits a new base method with a default body as an additive change.
  Adding a required method to the sealed handler trait can also be compatible:
  the SDK updates all its implementations together. Moving an existing generated
  method to manual can be compatible under the same constructor and behavior
  conditions; neither change inherently requires a new contract major version.
* Sealing does not make every handler change compatible. Removing public methods,
  changing their signatures or losing object safety remains breaking. SDK Rust
  API compatibility and wire compatibility are checked separately; `oasdiff`
  does not detect a change in client dispatch that leaves the binding unchanged.
* Manual code still requires review and protocol tests. Shared execution helpers
  preserve common behavior but cannot prove arbitrary handler code correct.
* Full remote OAGW support still needs its identity contract, missing remote
  operations, any server-side upload-streaming work and a WebSocket upgrade
  path. This mechanism supplies client composition, not those separate features.

### Confirmation

Before implementing, verify the following dependency boundaries and acceptance
criteria. This is a verification plan, not a claim that the new behavior exists.

| Boundary | Required verification |
|---|---|
| Parser and codegen | A mixed client implements both base and projection traits; missing SDK handler methods, signature drift and incompatible markers fail in trybuild. Types on a method marked both `client_manual` and `server_manual` need no serde/schema derives. |
| Sealing | An SDK implements both the private supertrait and `Manual`; a separate consumer crate cannot implement `Manual` for its own handler. Use a real SDK crate boundary in the trybuild fixture, not two modules of the same crate. |
| Ownership | Concrete SDK handlers and `dyn Manual` handlers work with `with_manual`; the client struct carries only `?Sized`; non-`Clone` state is shared through `Arc`; borrowed arguments and both existing stream-open shapes retain their lifetimes. Manual `Clone` and handler-omitting `Debug` need no `H: Clone` or `H: Debug`. |
| Construction and wiring | With `manual_handler`, `{Projection}Client::new(config)` and the existing `provides`/`consumes` expansions compile unchanged; without it, `new` is rejected in trybuild. Explicit handlers work with fixed endpoints and `with_http_client`-style construction; existing unannotated constructor calls still compile. |
| Stateful factory lifecycle (deferred) | When factory integration is implemented, `consumes` can use dependencies registered by all `init` calls; `provides` succeeds with earlier declared dependencies and fails with a clear startup wiring error if a required dependency is not yet registered. Both paths reject unavailable required dependencies. |
| Discovery | Endpoint churn preserves handler state, rebuilds transport with current tuning/credentials and leaves in-flight streams valid; unresolved-provider and concurrency tests still pass. |
| SDK evolution | Build the same consumer fixture against baseline and extended SDK versions: add an optional base method and its required sealed handler method, and separately move an existing generated method to manual while preserving its behavior and constructors. Both versions must compile and preserve the consumer's behavior. |
| IR and server metadata | Coverage includes manual methods; wildcard/method-set/opaque bindings validate without fictitious JSON; hand-written binding literals and serialized old IR have compatibility coverage. |
| Raw transport | Bytes, repeated end-to-end headers, non-2xx status and trailers survive; a `Send + !Sync` body is accepted; upgrade requests are rejected; upload/response streams are incremental; read failures, deadlines and drop cancellation remain observable. |
| Common policies | Tenant/platform credentials rotate correctly; hostile authority/auth headers cannot override the destination or identity; tracing/metrics work; no decompression, redirects, replay or SSE reconnect occurs. |
| Existing consumers | Existing REST client/server/platform-plane and wiring tests, trybuild fixtures, gRPC projection tests and SDK feature combinations pass unchanged. |

Trybuild verifies compilation and rejection cases; a sealing fixture alone does
not verify compatibility between SDK versions. The release-plz configuration
already enables `cargo-semver-checks` by default. Check the generated SDK surface
with that tool and the baseline/extended consumer fixtures; do not assume a check
of the proc-macro crate alone covers its generated API or that a particular new
trait-item case has been verified without running it.

Deliver in this order: typed manual composition with a default handler for a
fixed-route operation; endpoint-churn coverage; opaque binding metadata and raw
runtime support; OAGW SDK adoption and end-to-end tests. Add the stateful handler
source only when a handler needs one. Each increment must preserve existing
generated clients.

## Pros and Cons of the Options

### Option A: Raw HTTP Generation First

* Good, because a common proxy operation could need little hand-written code.
* Bad, because protocol-specific response handling still needs an extension point.
* Bad, because raw transport policy and binding metadata must be resolved anyway.

### Option B: Typed Manual Composition (chosen)

* Good, because signatures, ownership and missing handlers are checked by Rust.
* Good, because one generic handler covers related methods and supports concrete
  or dynamic dispatch without changing consumers.
* Good, because a default handler keeps existing constructors and wiring
  unchanged.
* Good, because sealing lets the SDK add manual methods without imposing new
  implementation requirements on consumers.
* Bad, because codegen needs additional construction paths, and manual behavior
  needs focused tests.
* Bad, because applications cannot supply their own handler implementations or
  external handler test doubles; they must use SDK-provided types or implement
  the whole base contract on a separate client.

### Option C: Replace the Entire Client

* Good, because the existing escape hatch needs no framework changes.
* Bad, because a single unusual method forces duplication or another split of
  the public contract merely to accommodate the generator.

### Option D: Dynamic Callback Registry

* Good, because callbacks can be replaced at runtime.
* Bad, because missing registrations become runtime failures and erased payloads
  weaken signature and lifetime checks. No current requirement needs hot swapping.

## More Information

* [ADR-0001: contract source of truth](./0001-cpt-cf-binding-adr-contract-source-of-truth.md).
* [ADR-0002: spec limits and manual escape hatch](./0002-cpt-cf-binding-adr-openapi-spec-limits.md).
* [ADR-0003: projection parity and manual server methods](./0003-cpt-cf-binding-adr-projection-server-gen.md).
* [ADR-0004: consumer wiring and readiness](./0004-cpt-cf-binding-adr-consumer-wiring.md).
* [ADR-0006: client observability](./0006-cpt-cf-binding-adr-client-observability.md).
* [ADR-0007: contract versioning](./0007-cpt-cf-binding-adr-contract-versioning.md).
* [Rust API Guidelines: sealed traits and their compatibility limits](https://rust-lang.github.io/api-guidelines/future-proofing.html#c-sealed).
* Release compatibility checks: [`release-plz.toml`](../../../../release-plz.toml)
  and [the release workflow](../../../../.github/workflows/release-plz.yml).
* Codegen: `libs/toolkit-contract-macros/src/{rest_contract,rest_contract_parse,projection,provides,consumes}.rs`.
* Runtime and metadata: `libs/toolkit-contract/src/{runtime/resolving.rs,runtime/client.rs,ir/binding.rs,ir/validation.rs}`.
* Existing hand-written bindings: `gears/system/types-registry/types-registry-sdk/src/rest_binding.rs`.
* Transport: `libs/toolkit-http/src/{client,request,builder}.rs` and
  `libs/toolkit-http-proxy/src/{client,body,sanitize}.rs`.
* Tests: `libs/toolkit-contract/tests`, `libs/toolkit-contract-macros-tests/tests/ui`,
  and consumer-wiring tests in `libs/toolkit/src/discovery.rs`.
