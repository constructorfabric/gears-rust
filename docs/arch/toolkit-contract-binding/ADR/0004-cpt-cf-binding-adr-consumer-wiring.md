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
  the explicit directory-less mode (`discovery: none`), where a static `consumer_wiring.<dep>.endpoint` is the
  supported resolution mechanism (e.g. a k8s Service URL) rather than a dev/test-only escape hatch.
* **Transparency across runtime profiles** — in Profile 1 (embedded), the local in-process
  implementation is used without any HTTP hop; in Profile 2/3 (OoP), the generated REST client is
  wired automatically. Gear business logic sees `Arc<dyn BillingApi>` in both cases.
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
            // `consumer_wiring.<dep>` config and already carries the process's
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

**Each consumer gets its own proxy.** The runtime calls `wire` with the owner gear's view of the
hub, `ClientHub::for_gear(owner_gear)`. Local and scoped registrations are shared by all views;
a proxy registered through `register_remote_proxy` belongs to the view's owner. So two gears that
consume the same contract each get a proxy built from their own `consumer_wiring.<dep>` settings
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
  dependency satisfied in-process (`Local`) from one bound to a remote client (`Remote`). Only
  `Remote` deps gate `/readyz` and get a background resolve loop; a co-located local impl is marked
  readiness-resolved immediately and spawns no directory probe. Returning `()` — as the original
  sketch did — would have forced every embedded-profile gear to wait on a directory it never needs.

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
    (endpoint, tuning) = read_consumer_wiring(owner_gear, dep_gear)  // parsed ConsumerWiring
    hub = client_hub.for_gear(owner_gear) if owner_gear is a gear here, else client_hub
    outcome = ConsumerRegistration.wire(hub, resolver_for(dep_gear, endpoint), tuning)?
    if outcome == Local or endpoint is static:
        resolve_now += dep_gear
    else:
        needs_probe += dep_gear

mark readiness-resolved: resolve_now - needs_probe   // no directory probe at all
spawn probe for needs_probe: loop with exponential backoff (100 ms → 200 ms → … → 30 s cap):
    if DirectoryService.resolve_rest_service(dep_gear) is Ok:
        mark dep_gear readiness-resolved

when no dep_gear remains unresolved:
    state = ready   →   /readyz responds HTTP 200
```

A dep is resolved at wiring time only when no consumer of it needs discovery: with one consumer on
a static endpoint and another on discovery, the dep waits for the probe.

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
Only successes are cached, and a failed call does not invalidate the entry, so a
provider that moves is followed within roughly the TTL rather than instantly.

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

Each consumed dependency is configured per gear under the same `config` block the provider side uses
for `client_wiring`, keyed by the dependency (provider) gear name. `consumer_wiring.<dep>` is an
**object** (`ConsumerWiring`) carrying an optional `endpoint` plus a flattened `ClientTuning`:

```yaml
# config.yaml
gears:
  orders:
    config:
      consumer_wiring:
        api-contracts:
          timeout: "5s"
          max_concurrent_requests: 256
          pool_max_idle_per_host: 256
          # endpoint: "http://localhost:8081"   # optional; omit to keep discovery
```

The bare-string form earlier drafts accepted (`billing: "http://localhost:8081"`) is **no longer
supported**: the override lives on the optional `endpoint` field, and a bare string is a boot error
whose message names the object form.

Because the consumer path is **REST-only** (`#[toolkit::consumes]` always wires a
`<Contract>RestResolvingClient`; there is no gRPC resolving client), `ConsumerWiring` carries **no**
`transport` tag and every `ClientTuning` knob applies — no gRPC disambiguation and no
`rest_only_knobs_set` warning (contrast the provider-side `client_wiring`).

This is the caller-owned config surface that [DESIGN §1.3](../DESIGN.md#13-operational-semantics)
specifies for remote contracts — "Own config (URL, timeout, retry policy) via `ClientConfig`" — now
reachable on the actual high-concurrency gear-to-gear caller. `ConsumerWiring` deserializes into a
`ClientTuning` whose `apply_to(endpoint)` produces that per-call `ClientConfig`.

The proxy-wiring phase (`host_runtime::run_proxy_wiring_phase`) reads the entry via
`read_consumer_wiring(...)`, splits it into `(endpoint, tuning)`, threads the process's platform-plane
credential onto the tuning, and hands it to `ConsumerRegistration::wire`. A present-but-unparseable
entry — wrong shape, wrong-typed tuning, or a non-absolute `endpoint` (validated at parse time by
`ConsumerWiring`'s `deserialize_with`) — is a boot error (`RegistryError::ProxyWiring`), matching the
provider side's `client_wiring` (`toolkit::wiring::read_wiring`); it is never downgraded to discovery.

**`endpoint` present** → the dep is wired through a `StaticEndpointResolver` (`toolkit::discovery`),
bypassing the directory and readiness-resolved immediately (no probe loop). This is the dev/test
escape hatch, logged at `warn!`. **`endpoint` omitted** → discovery is untouched; only tuning applies.

The `<owner>` segment is `ConsumerRegistration::owner_gear`. `#[toolkit::consumes]` takes it from
`name = "..."` of a `#[toolkit::gear]` written below it on the same struct; otherwise it uses the
**kebab-case of the struct's ident** (`struct ApiContractsConsumer` → `api-contracts-consumer`).
If `owner_gear` is not a registered gear, the key will not resolve; the proxy-wiring phase emits a
`warn!` naming both rather than ignoring the override silently.

Known limits of the escape hatch: exactly one address (no list, weights or failover), no fallback to
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

Once every `Remote` dependency has resolved:

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
  added in `c280de1`; this ADR depends on that method being present.
* Topo-sort entry point: `libs/toolkit/src/registry.rs` — `build_topo_sorted`; it sees only the
  `deps` declared on `#[toolkit::gear]`, never anything from `#[toolkit::consumes]`.
* `ClientHub`: `libs/toolkit/src/client_hub.rs` — `try_get`, `register`, `get` are the three methods
  used by the generated `wire` closure.
* Wiring integration point: `libs/toolkit/src/runtime/host_runtime.rs` —
  `run_proxy_wiring_phase`, after `DirectoryClient` is in the hub and before `gear.run()`. It is
  shared by both runtime paths (in-process host and OoP serving); `bootstrap/oop.rs` contains no
  consumer-wiring code.
* Consumer client tuning: this ADR's original scope was *discovery* (the endpoint alone), so the
  wired client used a hardcoded `ClientTuning::default()`. It now parses `consumer_wiring.<dep>` into
  `ConsumerWiring` (optional `endpoint` + flattened `ClientTuning`) and threads it through `WireFn`
  into the resolving client — the caller-owned config specified for remote contracts.
