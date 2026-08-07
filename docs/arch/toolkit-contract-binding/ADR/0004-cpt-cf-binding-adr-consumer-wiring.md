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
  through `DirectoryService`, not hard-coded in `config.yaml` or source code.
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
Each is independent; they may name the same or different provider gears.

### What the macro generates

For each `#[toolkit::consumes(contract = C, from = "name")]` the macro emits an `inventory::submit!`
of a `ConsumerRegistration`:

```rust
inventory::submit! {
    toolkit::discovery::ConsumerRegistration {
        owner_gear: "orders",
        dep_gear:   "billing",
        wire: |hub: &ClientHub, resolver: Arc<dyn EndpointResolver>|
              -> anyhow::Result<WireOutcome> {
            // Short-circuit: Profile 1 in-process impl already present.
            if hub.try_get::<dyn billing_sdk::BillingApi>().is_some() {
                return Ok(WireOutcome::Local);
            }
            let client = billing_sdk::BillingApiRestResolvingClient::new(resolver);
            hub.register::<dyn billing_sdk::BillingApi>(Arc::new(client));
            Ok(WireOutcome::Remote)
        },
    }
}
```

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

After establishing the `DirectoryClient` connection and before calling `gear.run()`, the bootstrap
wires every `ConsumerRegistration` whose `owner_gear` matches the current gear, then spawns a
resolve loop for those that bound remotely:

```text
for each ConsumerRegistration where owner_gear == this_gear:
    outcome = ConsumerRegistration.wire(client_hub, resolver_for(dep_gear))?
    if outcome == Local:
        mark dep_gear readiness-resolved       // no directory probe at all
    else:
        spawn probe: loop with exponential backoff (100 ms → 200 ms → … → 30 s cap):
            if DirectoryService.resolve_rest_service(dep_gear) is Ok:
                mark dep_gear readiness-resolved
                break

when no dep_gear remains unresolved:
    state = ready   →   /readyz responds HTTP 200
```

Wiring happens once and up front; the probe only drives *readiness*, not the binding. The client
registered for a `Remote` dep resolves its endpoint per call through the same
`EndpointResolver`, so a provider that restarts elsewhere is followed without re-wiring.

Re-resolution is triggered on `DirectoryClient` reconnect to handle provider restarts. The backoff
policy and reconnect behaviour are consistent with the self-registration retry already specified in
vision ADR-0007.

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

### Static endpoint override (escape hatch)

For local development and integration tests a static endpoint overrides discovery, set per gear
under the same `config` block the provider side already uses for `client_wiring`:

```yaml
# config.yaml (development / test only)
gears:
  orders:
    config:
      consumer_wiring:
        billing: "http://localhost:8081"
```

When the key is present the proxy-wiring phase (`host_runtime::run_proxy_wiring_phase`) reads it via
`static_endpoint_override(...)` and wires the dep through a `StaticEndpointResolver`
(`toolkit::discovery`), which bypasses the directory entirely and is readiness-resolved immediately —
no probe loop. Every use is logged at `warn!`.

The `<owner>` segment is `ConsumerRegistration::owner_gear`, which `#[toolkit::consumes]` derives as
the **kebab-case of the annotated struct's ident** — a separate attribute cannot read the
`name = "..."` given to `#[toolkit::gear]`. So `orders` above assumes `struct Orders`, and
`api-contracts-consumer` assumes `struct ApiContractsConsumer`. If a gear's declared name is not the
kebab form of its struct ident the key will not resolve; the proxy-wiring phase emits a `warn!`
naming both rather than ignoring the override silently.

Known limits of the escape hatch: exactly one address (no list, weights or failover), no fallback to
the directory if that address is dead, no metadata or version predicate, and it is read once at
startup with no hot reload.

**The production guard is deferred.** This ADR originally specified that the key's presence in a
production configuration should be a fatal startup error. The runtime has no deployment-profile or
environment concept to key that on — `AppConfig` carries no `profile` field — so the guard would
require a deliberate config-schema change first. Until then the `warn!` on every static-override use
is the only safety signal.

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
