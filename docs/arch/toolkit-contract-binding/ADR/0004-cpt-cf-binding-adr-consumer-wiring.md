---
status: proposed
date: 2026-06-02
---

# Discovery-Driven Consumer Wiring via `#[toolkit::consumes]`

**ID**: `cpt-cf-binding-adr-consumer-wiring`

## Table of Contents

1. [Context and Problem Statement](#context-and-problem-statement)
2. [Decision Drivers](#decision-drivers)
3. [Considered Options](#considered-options)
4. [Decision Outcome](#decision-outcome)
5. [Pros and Cons of the Options](#pros-and-cons-of-the-options)
6. [More Information](#more-information)

## Context and Problem Statement

ADR-0001 defines how a gear *provides* a contract to its consumers via `#[toolkit::provides]`. There
is no symmetric consumer-side counterpart. In the current PoC (`c280de1`), a consumer that needs a
remote implementation must either:

1. Load the **provider's implementation crate** as a stub compiled with `transport = rest` — coupling
   the consumer's binary to the provider's internal types and build artifacts, or
2. Hard-code a static endpoint string in `ClientWiring::Rest { endpoint }` and call `wire_*()`
   manually in `init()` — with no integration with service discovery and no readiness gating.

The `DirectoryService` was extended in `c280de1` with `resolve_rest_service(name)`, which maps a
logical gear name to a live REST endpoint. Vision ADR-0007 (`cpt-cf-adr-eventual-readiness`,
PR #1957) specifies that the OoP runtime should poll `DirectoryService.ResolveRestService(dep)` for
each declared dependency and wire the resulting endpoint into `ClientHub`, gating `/readyz` until all
critical dependencies are resolved. No developer-facing API for that polling loop was defined.

This ADR introduces `#[toolkit::consumes]` as that developer-facing API and specifies how it integrates
with the OoP bootstrap and the embedded (in-process) runtime.

## Decision Drivers

* **SDK-crate-only dependency** — the consumer must depend only on the provider's `*-sdk` crate. Loading
  the provider's implementation crate as a process-time stub must not be required.
* **No static endpoint configuration** — the endpoint of a remote provider is resolved at runtime
  through `DirectoryService`, not hard-coded in `config.yaml` or source code. **Amended by
  `cpt-cf-adr-optional-service-discovery`:** this driver holds for directory-based deployments, but is superseded for
  the explicit directory-less mode (`discovery: none`), where a static `clients.consumes.<dep>.endpoint` is the
  supported resolution mechanism (e.g. a k8s Service URL) rather than a dev/test-only escape hatch.
* **Transparency across runtime profiles** — in Profile 1 (embedded), the local in-process
  implementation is used without any HTTP hop; in Profile 2/3 (OoP), the generated REST or gRPC
  client is wired automatically. Gear business logic sees `Arc<dyn BillingApi>` in both cases.
* **Readiness gating** — a gear must not signal readiness (`/readyz` returning 200) until all of its
  critical dependencies are resolved and wired, consistent with vision ADR-0007.
* **Startup must not block on a dependency** — a consumer has to start, and keep starting cleanly,
  while its provider is absent. Ordering is therefore *not* solved by declaring the dependency; it is
  solved by resolving lazily (see the topology note under
  [What the macro generates](#what-the-macro-generates)).
* **Escape hatch** — a developer must be able to override discovery with a static endpoint for local
  development and integration testing.

## Considered Options

* **Option A**: Convention-based auto-wiring — the bootstrap scans every `deps` entry, calls
  `resolve_rest_service(dep)` for each, and attempts to match a registered SDK-trait factory by
  name. No new macro required on the consumer side.
* **Option B**: `#[toolkit::consumes]` explicit macro — consumers declare the contract trait type and the
  logical dep name; the macro registers a typed `ConsumerRegistration`; the bootstrap calls its `wire`
  closure after discovery.
* **Option C**: Retain `ClientWiring::Rest { endpoint }` as the primary wiring path; document it as
  the supported pattern and require authors to configure static endpoints.

## Decision Outcome

Chosen option: **Option B — `#[toolkit::consumes]` explicit macro.**

Option A requires the framework to infer, for every gear name, which Rust trait `TypeId` to wire.
There is no static mapping — a gear may provide multiple contracts — and a convention-based
name→`TypeId` lookup would require either fragile string matching or a global side-table populated
by provider-side inventory items, reintroducing provider-crate linkage. Option C is the status quo;
it does not satisfy the SDK-only or discovery requirements.

### Macro shape

```rust
#[toolkit::gear(name = "orders")]
#[toolkit::consumes(contract = billing_sdk::BillingApi, from = "billing")]
#[toolkit::consumes(contract = inventory_sdk::InventoryApi, from = "inventory")]
pub struct OrdersGear { … }
```

Multiple `#[toolkit::consumes]` attributes are allowed on the same struct, one per dependency trait.
Each is independent; they may name the same or different provider gears. One attribute can also
list several contracts. A top-level `from` applies to every entry, and an entry can set its own
`from` or `resolving_client`:

```rust
#[toolkit::consumes(
    from = "types-registry",
    contract = [
        types_registry_sdk::PlatformTypesRegistryApi,
        types_registry_sdk::TypesRegistryApi,
        billing_sdk::BillingApi(from = "billing"),
    ],
)]
pub struct OrdersGear { … }
```

The list form expands to the same registrations as one attribute per contract. A contract consumed
twice on one struct, in one list or across stacked attributes, is a compile error.

`transport = grpc` (top level, or on one entry) consumes the contract over gRPC; `transport = rest`
is the default:

```rust
#[toolkit::consumes(contract = payments_sdk::PaymentApi, from = "payments", transport = grpc)]
pub struct OrdersGear { … }
```

The transport is chosen here, in code, not in config. REST wires
`<parent>::<Contract>RestResolvingClient` (from `#[toolkit::rest_contract]`), resolved by gear
name; gRPC wires `<parent>::<Contract>GrpcResolvingClient` (from `#[toolkit::grpc_contract]` on a
projection named `<Contract>Grpc`), resolved by its gRPC service name. The SDK must be built with
`rest-client` or `grpc-client` respectively and re-export the resolving client next to the base
trait; `resolving_client = path` overrides the path.

### What the macro generates

For each `#[toolkit::consumes(contract = C, from = "name")]` the macro emits an `inventory::submit!`
of a `ConsumerRegistration`:

```rust
inventory::submit! {
    toolkit::discovery::ConsumerRegistration {
        owner_gear: "orders",
        dep_gear:   "billing",
        wire: |hub: &ClientHub,
               resolver: Arc<dyn EndpointResolver>,
               tuning: ClientTuning|
              -> anyhow::Result<WireOutcome> {
            // Short-circuit: Profile 1 in-process impl already present.
            if hub.try_get_local::<dyn billing_sdk::BillingApi>().is_some() {
                return Ok(WireOutcome::Local);
            }
            // This view already holds a proxy for the contract: in a gear's
            // view, a second `#[consumes]` of it (boot error); in the
            // process-wide hub, reuse it.
            if hub.has_remote_proxy::<dyn billing_sdk::BillingApi>() {
                if let Some(owner) = hub.owner() {
                    anyhow::bail!("gear `{owner}` declares #[toolkit::consumes] for \
                                   `billing_sdk::BillingApi` more than once");
                }
                return Ok(WireOutcome::Remote);
            }
            // `tuning` is prepared by the proxy-wiring phase from the consumer's
            // `clients.consumes.<dep>` config and already carries the process's
            // platform-plane credential source, so the resolving client's
            // platform-plane methods attach `X-ToolKit-Internal-Token`
            // (no credential in Profile 1 attaches nothing).
            let client = billing_sdk::BillingApiRestResolvingClient::new(resolver, "billing", tuning);
            hub.register_remote_proxy::<dyn billing_sdk::BillingApi>(Arc::new(client));
            Ok(WireOutcome::Remote)
        },
    }
}
```

With `transport = grpc` the closure differs in two places: it builds
`billing_sdk::BillingApiGrpcResolvingClient::new(resolver, "billing", tuning)`, and both remote
returns are `WireOutcome::RemoteGrpc { service: billing_sdk::BillingApiGrpcResolvingClient::GRPC_SERVICE }`,
so the runtime knows which gRPC service to probe for readiness. `WireFn` and
`ConsumerRegistration` are the same for both transports.

**Each consumer gets its own proxy.** The runtime calls `wire` with the owner gear's view of the
hub, `ClientHub::for_gear(owner_gear)`. Local and scoped registrations are shared by all views;
a proxy registered through `register_remote_proxy` belongs to the view's owner. So two gears that
consume the same contract each get a proxy built from their own `clients.consumes.<dep>` settings
(endpoint, timeout, retry, `require_tls`, pool), whatever the wiring order. `GearCtx::client_hub()`
returns the gear's view, and `get`/`try_get` there return, in order: a local impl, the gear's own
proxy, a process-wide proxy. A local impl registered after wiring still wins. A registration whose
`owner_gear` is not a gear in the process (an SDK-level one such as cluster-sdk's) is wired into
the plain hub, and its proxy is process-wide. One gear declaring the same contract twice would get
one proxy for both declarations. The macro rejects that at compile time for declarations on one
struct. The wire fn also refuses it at boot, but only on the remote path: with a co-located
provider both declarations resolve to the local impl.

**The short-circuit must use `try_get_local`, not `try_get`.** Proxies are stored apart from
local registrations, and `try_get_local` sees only the latter. `try_get` also returns a
process-wide proxy; reporting that as `Local` would mark the `DependencyChecker` entry resolved.
The checker is sticky, so `/readyz` would go 200 with no provider up.

Two details differ from the sketch this ADR originally carried, both load-bearing:

* **`wire` takes an `Arc<dyn EndpointResolver>`, not a resolved `&str` endpoint.** Wiring therefore
  happens once, at startup, and the client resolves (and re-resolves) the provider address per call.
  A provider that restarts on a new address is picked up without re-wiring the consumer.
* **`wire` returns [`WireOutcome`](../../../../libs/toolkit/src/discovery.rs)**, distinguishing a
  dependency satisfied in-process (`Local`) from one bound to a remote client: `Remote` for a REST
  client, `RemoteGrpc { service }` for a gRPC client. Only remote deps gate `/readyz` and get a
  background resolve loop; a co-located local impl is marked readiness-resolved immediately and
  spawns no directory probe. Returning `()` — as the original sketch did — would have forced every
  embedded-profile gear to wait on a directory it never needs. `WireOutcome` is
  `#[non_exhaustive]`.

**Topology: `#[toolkit::consumes]` does not inject a topo-sort dependency.** An earlier draft of this
ADR had it insert `from` into the gear's `deps`; the shipped macro deliberately does not. Two reasons:
a separate attribute cannot mutate the `&'static` deps baked in by `#[toolkit::gear]`, and
auto-injecting `from` would make the topo-sort *fail* whenever the provider is remote and therefore
absent from the local registry — the opposite of the non-blocking-startup model this ADR is built on.
Co-located hard dependencies stay explicit in `#[toolkit::gear(deps = [...])]`; everything else is
tolerated lazily by the resolving client.

### Runtime integration — proxy-wiring phase (`runtime/host_runtime.rs`)

After init and before post-init, the bootstrap wires every `ConsumerRegistration` into its owner's
view of the hub, then spawns a resolve loop for the deps that bound through discovery:

```text
for each ConsumerRegistration:
    (endpoint, grpc_endpoint, tuning) = read_consumer_wiring(owner_gear, dep_gear)  // ConsumerWiring
    hub = client_hub.for_gear(owner_gear) if owner_gear is a gear here, else client_hub
    resolver = directory, with endpoint / grpc_endpoint (else endpoint) as static
               overrides for REST / gRPC lookups
    outcome = ConsumerRegistration.wire(hub, resolver, tuning)?
    match outcome:
        Local                                      → resolve_now += dep_gear
        Remote, endpoint set                       → resolve_now += dep_gear
        RemoteGrpc, grpc_endpoint or endpoint set  → resolve_now += dep_gear
        Remote, in-process dep serves no REST API  → boot error
        RemoteGrpc, in-process dep serves no gRPC  → boot error
        Remote                                     → probes[dep_gear] += Rest
        RemoteGrpc { service }                     → probes[dep_gear] += Grpc(service)
    if owner_gear wires dep_gear over REST and gRPC and only `endpoint` is set → boot error

mark readiness-resolved: resolve_now - keys(probes)   // no directory probe at all
spawn probe for probes: loop with exponential backoff (100 ms → 200 ms → … → 30 s cap):
    for each dep_gear, for each pending target:
        Rest          → DirectoryService.resolve_rest_service(dep_gear)
        Grpc(service) → DirectoryService.resolve_grpc_service(service)
    mark dep_gear readiness-resolved once all of its targets resolved
    after ~10 rounds, warn once per gRPC service still missing (grpc-hub listen_addr / advertise_addr)

when no dep_gear remains unresolved:
    state = ready   →   /readyz responds HTTP 200
```

A dep is resolved at wiring time only when no consumer of it needs discovery: with one consumer on
a static endpoint and another on discovery, the dep waits for the probe. A dep consumed over both
transports (by one gear or by two) is probed for both targets and becomes ready only when its REST
endpoint and its gRPC service both resolve.

"Serves no gRPC" means the dep is a gear in this process without a gRPC service capability; a
`RemoteGrpc` binding to it could never resolve. When a `RemoteGrpc` client's `clients.consumes` entry sets
knobs a gRPC client does not apply (the REST-only `pool_max_idle_per_host`, `pool_idle_timeout`,
`max_concurrent_requests`, and `stream_reconnect`, since gRPC streams do not reconnect yet), the
phase logs a `warn!` that they have no effect. `clients.provides` with `transport: grpc` warns the same way.

Wiring happens once and up front; the probe only drives *readiness*, not the binding. The client
registered for a `Remote` dep resolves its endpoint per call through the same
`EndpointResolver`, so a provider that restarts elsewhere is followed without re-wiring.

Re-resolution is triggered on `DirectoryClient` reconnect to handle provider restarts. The backoff
policy and reconnect behaviour are consistent with the self-registration retry already specified in
vision ADR-0007.

**The not-found sentinel is load-bearing.** `EndpointResolver` distinguishes
`Ok(None)` ("the lookup succeeded; nothing is registered yet") from `Err` ("the
directory itself failed"), and that distinction decides whether a routine startup
race is logged at `debug` or `warn`. It only works if the directory client
constructs the typed `DirectoryNotFound` sentinel: `DirectoryEndpointResolver`
downcasts to it, and a bare `anyhow` falls through to the `Err` arm. Both clients
must therefore produce it — `LocalDirectoryClient` on an empty `GearManager`
lookup, and `DirectoryGrpcClient` on `tonic::Code::NotFound` — and the
orchestrator's gRPC server must map only genuine not-found to `NotFound` rather
than blanket-mapping every failure. Getting this wrong is not a loud failure: the
system still converges, but every unresolved dependency is reported as a
directory outage, and the resolving client warns on *every* business call to a
provider that has simply not started yet.

**Endpoint resolution is memoized** for `DirectoryEndpointResolver::TTL` (1500 ms).
Only successes are cached; REST answers are keyed by gear name and gRPC answers by
service name, apart. An attempt that the client's circuit breaker classifies as an
endpoint failure (`cpt-cf-binding-adr-client-circuit-breaker`) calls
`EndpointResolver::invalidate(target)`, at most once per second per endpoint, which
drops that entry; the next call asks the directory again. A provider that moves is
therefore followed at the next call after its old endpoint fails, and otherwise
within roughly the TTL.

**gRPC resolves by service name, not by gear.** `ResolveTarget::Grpc { gear, service }`
resolves through `DirectoryClient::resolve_grpc_service(service)`, which answers with
any healthy instance advertising that service, whichever gear it belongs to (see
`cpt-cf-adr-instance-addressable-discovery`). `from` names the providing gear for
logs, errors, readiness and the in-process check; it does not filter the lookup.
The directory refuses a gRPC service name already owned by another gear, so a name
cannot silently move between gears. The service name carries the contract's major
version (`cpt-cf-binding-adr-contract-versioning`), so each major resolves on its own.

`EndpointResolver` gained default methods for this: `resolve_grpc_endpoint(service)`
(default `Ok(None)`, so a resolver written before gRPC consumers leaves them
unresolved rather than misrouted), `resolve_target(&ResolveTarget)`, and
`invalidate(&ResolveTarget)` (default no-op). `DirectoryResolvingClient` serves both
transports and keeps one client per endpoint URI (up to 16), each with its own
breaker, so the directory's round-robin neither rebuilds clients nor resets breakers.

### Runtime integration — embedded profile (Profile 1)

For a co-located provider the `wire` closure calls `hub.try_get::<dyn BillingApi>()` first; if the
local implementation is already registered it returns `WireOutcome::Local` immediately — no HTTP
client, no discovery call, no polling task, and no `/readyz` gating.

Ordering here comes from `#[toolkit::gear(deps = [...])]`, not from `#[toolkit::consumes]`. If the
provider is declared as a hard dep the topo-sort initialises it first and the short-circuit always
hits; if it is not, the consumer may wire before the provider registers and will fall through to the
resolving client. That is a correctness-preserving fallback rather than a failure — the client
resolves per call — but it costs an HTTP hop to an in-process gear, so declare co-located providers
in `deps` when you want the local path guaranteed.

### Consumer wiring: endpoint override + client tuning

Client settings live in their own section of the gear's entry, `gears.<gear>.clients`, next to
`config`, `database` and `runtime`, not inside the gear's `config`:

```yaml
# config.yaml
gears:
  orders:
    config: { ... }                         # the gear's own configuration only
    clients:
      defaults:                             # tuning for every client this gear builds
        timeout: "5s"
        circuit_breaker: { open_for: "30s" }  # see cpt-cf-binding-adr-client-circuit-breaker
      consumes:
        api-contracts:                      # #[toolkit::consumes(from = "api-contracts")]
          max_concurrent_requests: 256
          pool_max_idle_per_host: 256
          # endpoint: "http://localhost:8081"       # optional; omit to keep discovery
          # grpc_endpoint: "http://localhost:50051" # optional; static address for gRPC clients
      provides:
        payment_api:                        # #[toolkit::provides], contract in snake case
          transport: rest
          endpoint: "http://payments:8080"
```

* `clients.consumes.<dep>` is keyed by the dependency (provider) gear name. It is an **object**
  (`ConsumerWiring`) with an optional `endpoint`, an optional `grpc_endpoint`, and a flattened
  `ClientTuning`.
* `clients.provides.<contract>` is the provider side's `ClientWiring`, keyed by the contract trait in
  snake case.
* `clients.defaults` holds `ClientTuning` keys only. It is merged under every entry key by key,
  nested objects (`circuit_breaker`, `retry`) included; the entry wins. A dep with no entry of its own
  still gets the defaults. `defaults` alone never makes a provided contract remote.
* Unknown keys anywhere in the section, and `endpoint` / `grpc_endpoint` / `transport` in `defaults`,
  are boot errors that name the full path (`gears.orders.clients.consumes.billing.timout`). serde's
  `deny_unknown_fields` does not work with a flattened struct, so the reader
  (`toolkit::clients_config`) checks against `ClientTuning::KEYS`.
* An OoP child receives the section from the master with its rendered config (`RenderedGearConfig`);
  a `clients` section in the child's local `--config` replaces it, as `config` does.

**Why a separate section.** The keys belong to the framework, not to the gear. Inside `config` they
reached the gear's own deserializer: a gear with `#[serde(deny_unknown_fields)]` refused them, so
gears declared placeholder fields only to let them through. The same arrangement is common elsewhere:
Spring Cloud OpenFeign keeps per-client settings under `spring.cloud.openfeign.client.config.<name>`
with a `default` entry, Quarkus REST Client under `quarkus.rest-client."<key>"`, Resilience4j under
`instances.<name>` with `configs.default`. The earlier `config.consumer_wiring` and
`config.client_wiring` keys are not read any more.

`ConsumerWiring` carries **no** `transport` tag (contrast the provider-side `ClientWiring`): the
transport is chosen in code, by `transport = …` on `#[toolkit::consumes]`. One `clients.consumes.<dep>`
entry therefore serves every client the gear builds for that dep. A gRPC client reads `timeout`,
`retry`, `require_tls` and `circuit_breaker`; it ignores the REST pool and concurrency knobs, and
the phase warns when they are set for it.

The circuit breaker is on by default for every client wired here, REST and gRPC; `circuit_breaker:
{ enabled: false }` turns it off for the dep (`cpt-cf-binding-adr-client-circuit-breaker`).

This is the caller-owned config surface that [DESIGN §1.3](../DESIGN.md#13-operational-semantics)
specifies for remote contracts — "Own config (URL, timeout, retry policy) via `ClientConfig`" — now
reachable on the actual high-concurrency gear-to-gear caller. `ConsumerWiring` deserializes into a
`ClientTuning` whose `apply_to(endpoint)` produces that per-call `ClientConfig`.

The proxy-wiring phase (`host_runtime::run_proxy_wiring_phase`) reads the entry via
`toolkit::clients_config::consumer_wiring(...)`, splits it into `(endpoint, tuning)`, threads the process's platform-plane
credential onto the tuning, and hands it to `ConsumerRegistration::wire`. A present-but-unparseable
entry — wrong shape, wrong-typed tuning, or a non-absolute `endpoint` (validated at parse time by
`ConsumerWiring`'s `deserialize_with`) — is a boot error (`RegistryError::ProxyWiring`), matching the
provider side (`toolkit::wiring::read_wiring`); it is never downgraded to discovery.

**`endpoint` present** → REST lookups for the dep return that address, bypassing the directory, and
a REST client of it is readiness-resolved immediately (no probe loop). gRPC lookups return
`grpc_endpoint` when set, else the same `endpoint`. Lookups for a transport with no override still
go to the directory, so `grpc_endpoint` alone pins only the gRPC client. The override is logged at
`warn!`. **Both omitted** → discovery is untouched; only tuning applies. (`StaticEndpointResolver`
in `toolkit::discovery` has the same split through `with_grpc_endpoint`.)

A gear that consumes one dep over **both** REST and gRPC with only `endpoint` set fails at boot
(`RegistryError::ProxyWiring`): one address cannot serve an HTTP listener and a gRPC listener, and
the error asks for `grpc_endpoint`.

The `<owner>` segment is `ConsumerRegistration::owner_gear`. `#[toolkit::consumes]` takes it from
`name = "..."` of a `#[toolkit::gear]` written below it on the same struct; otherwise it uses the
**kebab-case of the struct's ident** (`struct ApiContractsConsumer` → `api-contracts-consumer`).
If `owner_gear` is not a registered gear, the key will not resolve; the proxy-wiring phase emits a
`warn!` naming both rather than ignoring the override silently.

Known limits of the escape hatch: exactly one address per transport (no list, weights or failover), no fallback to
the directory if that address is dead, no metadata or version predicate, and it is read once at
startup with no hot reload.

**The production guard is deferred, and now superseded for directory-less mode.** This ADR originally specified that
the key's presence in a production configuration should be a fatal startup error. The runtime has no
deployment-profile or environment concept to key that on — `AppConfig` carries no `profile` field — so the guard
would require a deliberate config-schema change first.
`cpt-cf-adr-optional-service-discovery` resolves the direction rather than adding that guard: under the explicit
`discovery: none` mode the static `endpoint` is a **supported** production resolution mechanism, so a blanket
"fatal in production" guard would be wrong. The key remains discouraged (and should still warn) under
`discovery: directory`, where directory resolution is the intended path. That ADR also owns updating the `warn!`
text and the "dev/test" / "Not for production use" doc comments on `ConsumerWiring::endpoint` and
`StaticEndpointResolver` so they distinguish the two modes.

### Readiness response shape

`/readyz` is served from `ReadinessReport` (`toolkit::runtime::readiness`). While a consumed
dependency is still unresolved:

```json
HTTP 503
{ "state": "starting", "ready": false, "unresolved_deps": ["billing"] }
```

Once every remote (`Remote` or `RemoteGrpc`) dependency has resolved:

```json
HTTP 200
{ "state": "ready", "ready": true }
```

`state` is the primary signal and has four variants — `starting`, `ready`, `degraded`, `draining`.
`degraded` maps to `200` (serving with a healthcheck reporting reduced functionality), `draining`
to `503` during graceful shutdown. `ready` is a convenience mirror of `state ∈ {ready, degraded}`
for probes that would rather not encode the state→status mapping. `unresolved_deps` lists only
dependencies bound remotely and is omitted from the body when empty — a dependency satisfied
in-process ([`WireOutcome::Local`](#what-the-macro-generates)) never appears there.

This is the shape specified by the accepted eventual-readiness ADR
(`cpt-cf-adr-eventual-readiness`).

### Known limitation: "resolved" is not "conformant"

**Accepted, not fixed.** A dependency is marked resolved as soon as the directory
returns *any* live endpoint for the gear name. Nothing checks that the provider
actually serves this contract: not the base path, not the schema, not the major
version. A consumer of `PaymentApiV2` therefore goes Ready against a provider
that only serves `/v1/**`, and discovers the mismatch as a 404 on its first call.
There is no contract identity on the wire — no hash, no version handshake — so
schema evolution rests entirely on `oasdiff` in CI.

Two things make this tolerable rather than dangerous. Both versions of a contract
are normally served by the *same* provider (ADR-0007: parallel traits on one
router), so "the gear is up but serving the wrong major" means a genuinely
mis-deployed topology rather than routine startup churn. And the failure is a
clean 404 at the call site, not silent data corruption.

A gRPC consumer is better placed: it resolves the service name, which carries the
major version (`billing.v2.PaymentApi`), so it does not go Ready against a provider
serving only another major. Within one major it still cannot tell an older minor
release apart; a call to an RPC that release lacks returns canonical `Unimplemented`
(`cpt-cf-binding-adr-contract-versioning`).

Related and deliberate: `DependencyChecker` stores a bare `bool` per dependency,
so `Local`, directory-resolved, and static-override all collapse into one
`true`. `/readyz` cannot distinguish them. `unresolved_deps` is the only
per-dependency detail exposed.

Closing this needs a contract identity exchanged at resolve time — the provider
advertising which contracts and majors it serves, and the consumer checking its
own `base_path`/version from the IR against that. That is a wire-protocol
addition on both sides and belongs in its own ADR; tracked as follow-up.

### Relationship to `#[toolkit::provides]`

`#[toolkit::provides]` (ADR-0001, producer side) generates a `wire_<contract>()` method on the
gear struct. `ClientWiring::Rest { endpoint }` within `#[toolkit::provides]` remains valid as a
standalone-mode override for provider gears that also act as self-contained OoP processes
(e.g., the `api-contracts` example with `transport = rest`). It is not the primary wiring path for
consumers in a multi-gear topology. The `wire_*` methods are not removed; they remain usable in
unit tests and manual integration setups.

### Consequences

* Consumers depend only on the `*-sdk` crate (e.g., `billing-sdk`). The provider's implementation
  crate (`billing`) is never a direct or transitive dependency of the consumer binary.
* `ConsumerRegistration` and its `inventory::submit!` become part of the public API surface of
  `toolkit` (`toolkit::discovery`); changes to its fields are semver breaking changes.
* Gears declaring `#[toolkit::consumes]` that are built as in-process libraries still compile; the
  generated `inventory::submit!` is emitted unconditionally. In Profile 1 builds the bootstrap
  iterates the registrations and the `try_get` short-circuit fires for all of them.
* Init cycle detection stays with `deps`, not with `consumes`: a cycle `orders → billing → orders`
  is caught by the existing topo-sort only for providers the author declared in
  `#[toolkit::gear(deps = [...])]`. A consumed-but-undeclared provider is invisible to cycle
  detection by design — it may not even be in this process. The resolving client's per-call lookup
  is what makes that safe: a mutual dependency degrades to `ServiceUnavailable` until both sides are
  up, instead of deadlocking startup.
* `SecurityContext` propagation: the generated `BillingApiRestClient` extracts the raw bearer token
  from the passed `SecurityContext` and forwards it in the `Authorization` header. The full
  `SecurityContext` struct is not serialised over the wire; the receiving gear reconstructs context
  from the incoming `Authorization` and `x-secctx-bin` headers via its own middleware, consistent
  with vision ADR-0002 (`cpt-cf-adr-auth-edge-only`).

### Confirmation

* Unit test: macro expansion for `#[toolkit::consumes(contract = BillingApi, from = "billing")]`
  produces a `ConsumerRegistration` with the correct `owner_gear`, `dep_gear`, and a `wire`
  closure that compiles against `billing_sdk` alone (no `billing` impl crate in scope).
* Unit test: `wire` short-circuits when `hub.try_get::<dyn BillingApi>().is_some()`, returning
  `WireOutcome::Local` so the dep never gates `/readyz`.
* Unit tests (`consumes.rs`): `transport = grpc` wires `<Contract>GrpcResolvingClient` and returns
  `RemoteGrpc { service: …::GRPC_SERVICE }`; REST is the default; an entry can set its own
  transport; an unknown transport is a compile error.
* Runtime tests (`host_runtime.rs`): a gRPC dep resolves only when its service is published (a REST
  endpoint of the gear does not count); a dep consumed over both transports waits for both;
  `grpc_endpoint` pins only the gRPC client; one `endpoint` for both transports is a boot error.
* Integration tests (`api-contracts`): `grpc_consumer_runtime.rs` runs a gRPC consumer through the
  `HostRuntime` phases against a provider published by a gRPC hub; `grpc_resolving.rs` covers
  eventual readiness, a provider that moved, the breaker, per-major routing and `Unimplemented`.
* Integration test (Profile 1): `OrdersGear` initialises after `BillingGear` (topo-sort);
  `wire` short-circuits; `hub.get::<dyn BillingApi>()` returns the local implementation.
* Integration test (Profile 2/OoP): `OrdersGear` starts as OoP; bootstrap polls
  `resolve_rest_service("billing")`; wires `BillingApiRestClient`; `/readyz` transitions 503 → 200.
* Negative compile test: `cargo check --package orders` with only `billing-sdk` (not `billing`) in
  `Cargo.toml` must pass.
* Negative runtime test: static endpoint key present in a config tagged `profile = production`
  causes a fatal startup error with a clear message.

## Pros and Cons of the Options

### Option A: Convention-Based Auto-Wiring

Bootstrap iterates `deps`, calls `resolve_rest_service`, and for each resolved name searches a global
name → `TypeId` registry to find the matching factory.

* Good, because no new macro syntax is required on the consumer side.
* Bad, because a gear that provides multiple contracts (e.g., `BillingApi` and `AuditApi`) requires
  an additional disambiguation step that cannot be expressed by name alone.
* Bad, because the factory is only available if the provider's inventory item was linked into the
  binary, reintroducing provider-crate linkage — the exact problem this ADR exists to eliminate.
* Bad, because convention-based name→type mapping is fragile across rename refactors; the framework
  cannot distinguish a missing dep from a misspelled dep name until runtime.

### Option B: `#[toolkit::consumes]` Explicit Macro (chosen)

Consumer declares the contract type and dep name explicitly. Macro generates a typed factory owned
by the consumer binary. No inference, no provider linkage.

* Good, because the trait type is explicit — the compiler verifies it exists in the SDK crate at
  `cargo check` time.
* Good, because the factory is owned by the consumer binary; no provider code needs to be linked.
* Good, because one macro attribute per consumed contract serves as clear, greppable documentation
  of gear dependencies.
* Neutral, because requires a new macro attribute; adds a small amount of syntax to learn.
* Neutral, because adding a consumed contract requires both a `Cargo.toml` dep on the SDK crate and
  a `#[toolkit::consumes]` attribute — two places. Mitigated: omitting either produces a loud
  compiler error before any tests run.

### Option C: Static `ClientWiring::Rest { endpoint }` as Primary Path

Document the current pattern; require authors to configure static endpoints.

* Good, because no new framework code is required.
* Bad, because violates the SDK-only dependency requirement — the provider crate is still needed as
  a stub in many configurations.
* Bad, because static endpoints make multi-instance load-balancing and provider restart recovery
  impossible without manual configuration changes.
* Bad, because there is no readiness gating — a consumer that cannot reach its provider fails at
  the first call site with a generic HTTP error rather than at startup with a structured `/readyz`
  503 that identifies the unresolved dependency.

## More Information

* ADR-0001 — contract source of truth:
  [`0001-cpt-cf-binding-adr-contract-source-of-truth.md`](./0001-cpt-cf-binding-adr-contract-source-of-truth.md)
  — `#[toolkit::provides]` (producer side); this ADR adds the symmetric consumer-side counterpart.
* Vision ADR-0007 (PR #1957) `cpt-cf-adr-eventual-readiness` — specifies the background dependency
  resolution loop and readiness gating model; this ADR implements the developer-facing API for that
  mechanism.
* Vision DESIGN.md (PR #1957) § `cpt-cf-component-oop-bootstrap` — lists "background dependency
  resolution — poll `DirectoryService` for each `deps` entry, wire REST clients into `ClientHub`"
  as a gap; this ADR closes it.
* Vision ADR-0002 (PR #1957) `cpt-cf-adr-auth-edge-only` — defines `SecurityContext` propagation
  semantics (`bearer_token` forwarded via `Authorization` header, full context via `x-secctx-bin`);
  the generated REST client must conform to this protocol.
* Directory SDK extension: `libs/system-sdks/sdks/directory/src/api.rs` — `resolve_rest_service`
  added in `c280de1`; this ADR depends on that method being present. gRPC consumers use
  `resolve_grpc_service`.
* `cpt-cf-binding-adr-grpc-projection` (ADR-0008) — the generated `<Contract>GrpcResolvingClient`
  and `<BASE>_GRPC_SERVICE`.
* `cpt-cf-binding-adr-client-circuit-breaker` (ADR-0009 in this folder) — the breaker in every
  wired client and the endpoint-failure hook that calls `EndpointResolver::invalidate`.
* Resolving runtime: `libs/toolkit-contract/src/runtime/resolving.rs` (`DirectoryResolvingClient`,
  `ResolveTarget`, `EndpointResolver`), gated on `runtime-client`; resolvers in
  `libs/toolkit/src/discovery.rs`.
* Topo-sort entry point: `libs/toolkit/src/registry.rs` — `build_topo_sorted`; it sees only the
  `deps` declared on `#[toolkit::gear]`, never anything from `#[toolkit::consumes]`.
* `ClientHub`: `libs/toolkit/src/client_hub.rs` — `try_get`, `register`, `get` are the three methods
  used by the generated `wire` closure.
* Wiring integration point: `libs/toolkit/src/runtime/host_runtime.rs` —
  `run_proxy_wiring_phase`, after `DirectoryClient` is in the hub and before `gear.run()`. It is
  shared by both runtime paths (in-process host and OoP serving); `bootstrap/oop.rs` contains no
  consumer-wiring code.
* Consumer client tuning: this ADR's original scope was *discovery* (the endpoint alone), so the
  wired client used a hardcoded `ClientTuning::default()`. It now parses `clients.consumes.<dep>` into
  `ConsumerWiring` (optional `endpoint` + flattened `ClientTuning`) and threads it through `WireFn`
  into the resolving client — the caller-owned config specified for remote contracts.
