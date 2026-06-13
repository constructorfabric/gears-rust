---
status: proposed
date: 2026-06-02
---

# Discovery-Driven Consumer Wiring via `#[modkit::consumes]`

**ID**: `cpt-cf-binding-adr-consumer-wiring`

## Table of Contents

1. [Context and Problem Statement](#context-and-problem-statement)
2. [Decision Drivers](#decision-drivers)
3. [Considered Options](#considered-options)
4. [Decision Outcome](#decision-outcome)
5. [Pros and Cons of the Options](#pros-and-cons-of-the-options)
6. [More Information](#more-information)

## Context and Problem Statement

ADR-0001 defines how a module *provides* a contract to its consumers via `#[modkit::provides]`. There
is no symmetric consumer-side counterpart. In the current PoC (`c280de1`), a consumer that needs a
remote implementation must either:

1. Load the **provider's implementation crate** as a stub compiled with `transport = rest` — coupling
   the consumer's binary to the provider's internal types and build artifacts, or
2. Hard-code a static endpoint string in `ClientWiring::Rest { endpoint }` and call `wire_*()`
   manually in `init()` — with no integration with service discovery and no readiness gating.

The `DirectoryService` was extended in `c280de1` with `resolve_rest_service(name)`, which maps a
logical module name to a live REST endpoint. Vision ADR-0007 (`cpt-cf-adr-eventual-readiness`,
PR #1957) specifies that the OoP runtime should poll `DirectoryService.ResolveRestService(dep)` for
each declared dependency and wire the resulting endpoint into `ClientHub`, gating `/readyz` until all
critical dependencies are resolved. No developer-facing API for that polling loop was defined.

This ADR introduces `#[modkit::consumes]` as that developer-facing API and specifies how it integrates
with the OoP bootstrap and the embedded (in-process) runtime.

## Decision Drivers

* **SDK-crate-only dependency** — the consumer must depend only on the provider's `*-sdk` crate. Loading
  the provider's implementation crate as a process-time stub must not be required.
* **No static endpoint configuration** — the endpoint of a remote provider is resolved at runtime
  through `DirectoryService`, not hard-coded in `config.yaml` or source code.
* **Transparency across runtime profiles** — in Profile 1 (embedded), the local in-process
  implementation is used without any HTTP hop; in Profile 2/3 (OoP), the generated REST client is
  wired automatically. Module business logic sees `Arc<dyn BillingApi>` in both cases.
* **Readiness gating** — a module must not signal readiness (`/readyz` returning 200) until all of its
  critical dependencies are resolved and wired, consistent with vision ADR-0007.
* **Init ordering preserved** — declaring `from = "billing"` on the consumer automatically adds
  `"billing"` to the module's `deps`, which the existing topo-sort in Profile 1 already honours for
  correct startup sequencing.
* **Escape hatch** — a developer must be able to override discovery with a static endpoint for local
  development and integration testing.

## Considered Options

* **Option A**: Convention-based auto-wiring — the bootstrap scans every `deps` entry, calls
  `resolve_rest_service(dep)` for each, and attempts to match a registered SDK-trait factory by
  name. No new macro required on the consumer side.
* **Option B**: `#[modkit::consumes]` explicit macro — consumers declare the contract trait type and the
  logical dep name; the macro registers a typed `ConsumerRegistration`; the bootstrap calls its `wire`
  closure after discovery.
* **Option C**: Retain `ClientWiring::Rest { endpoint }` as the primary wiring path; document it as
  the supported pattern and require authors to configure static endpoints.

## Decision Outcome

Chosen option: **Option B — `#[modkit::consumes]` explicit macro.**

Option A requires the framework to infer, for every module name, which Rust trait `TypeId` to wire.
There is no static mapping — a module may provide multiple contracts — and a convention-based
name→`TypeId` lookup would require either fragile string matching or a global side-table populated
by provider-side inventory items, reintroducing provider-crate linkage. Option C is the status quo;
it does not satisfy the SDK-only or discovery requirements.

### Macro shape

```rust
#[modkit::module(name = "orders")]
#[modkit::consumes(contract = billing_sdk::BillingApi, from = "billing")]
#[modkit::consumes(contract = inventory_sdk::InventoryApi, from = "inventory")]
pub struct OrdersModule { … }
```

Multiple `#[modkit::consumes]` attributes are allowed on the same struct, one per dependency trait.
Each is independent; they may name the same or different provider modules.

### What the macro generates

For each `#[modkit::consumes(contract = C, from = "name")]` the macro emits an `inventory::submit!`
of a `ConsumerRegistration`:

```rust
inventory::submit! {
    modkit::contract::ConsumerRegistration {
        owner_module: "orders",
        dep_module:   "billing",
        wire: |hub: &ClientHub, endpoint: &str| -> anyhow::Result<()> {
            // Short-circuit: Profile 1 in-process impl already present.
            if hub.try_get::<dyn billing_sdk::BillingApi>().is_some() {
                return Ok(());
            }
            let client = billing_sdk::BillingApiRestClient::new(
                ClientConfig::new(endpoint),
            );
            hub.register::<dyn billing_sdk::BillingApi>(Arc::new(client));
            Ok(())
        },
    }
}
```

The macro also inserts `"billing"` into the module's `deps` list so that the existing topo-sort in
`RegistryBuilder::build_topo_sorted` includes it for Profile 1 startup ordering.

### Runtime integration — OoP bootstrap (`bootstrap/oop.rs`)

After establishing the `DirectoryClient` connection and before calling `module.run()`, the bootstrap
spawns one background task per `ConsumerRegistration` whose `owner_module` matches the current
module:

```text
for each ConsumerRegistration where owner_module == this_module:
    loop with exponential backoff (100 ms → 200 ms → … → 30 s cap):
        result = DirectoryService.resolve_rest_service(dep_module)
        if Ok(endpoint):
            ConsumerRegistration.wire(client_hub, endpoint)?
            mark dep_module as wired
            break

when all ConsumerRegistrations are wired:
    set readiness_flag = true   →   /readyz responds HTTP 200
```

Re-resolution is triggered on `DirectoryClient` reconnect to handle provider restarts. The backoff
policy and reconnect behaviour are consistent with the self-registration retry already specified in
vision ADR-0007.

### Runtime integration — embedded profile (Profile 1)

For in-process modules the topo-sort guarantees the provider is initialised before the consumer. The
`wire` closure calls `hub.try_get::<dyn BillingApi>()` first; if the local implementation is already
registered it returns immediately without constructing an HTTP client or making a discovery call. No
polling task is spawned for Profile 1 builds.

### Static endpoint override (escape hatch)

For local development and integration tests a static endpoint overrides discovery:

```toml
# config.yaml (development / test only)
modules.orders.wiring.billing = "http://localhost:8081"
```

When this key is present the bootstrap skips `resolve_rest_service` and calls
`wire(hub, static_endpoint)` directly at startup. The key is validated at boot time; its presence
in a production configuration is a fatal startup error.

### Readiness response shape

`/readyz` is managed by the OoP bootstrap. While wiring is in progress:

```json
HTTP 503
{ "status": "starting", "deps": { "billing": "waiting", "inventory": "resolved" } }
```

Once all `ConsumerRegistration` closures have returned `Ok`:

```json
HTTP 200
{ "status": "ready", "deps": { "billing": "resolved", "inventory": "resolved" } }
```

This response shape is the same format specified in vision ADR-0007.

### Relationship to `#[modkit::provides]`

`#[modkit::provides]` (ADR-0001, producer side) generates a `wire_<contract>()` method on the
module struct. `ClientWiring::Rest { endpoint }` within `#[modkit::provides]` remains valid as a
standalone-mode override for provider modules that also act as self-contained OoP processes
(e.g., the `api-contracts` example with `transport = rest`). It is not the primary wiring path for
consumers in a multi-module topology. The `wire_*` methods are not removed; they remain usable in
unit tests and manual integration setups.

### Consequences

* Consumers depend only on the `*-sdk` crate (e.g., `billing-sdk`). The provider's implementation
  crate (`billing`) is never a direct or transitive dependency of the consumer binary.
* `ConsumerRegistration` and its `inventory::submit!` become part of the public API surface of
  `modkit-contract`; changes to its fields are semver breaking changes.
* Modules declaring `#[modkit::consumes]` that are built as in-process libraries still compile; the
  generated `inventory::submit!` is emitted unconditionally. In Profile 1 builds the bootstrap
  iterates the registrations and the `try_get` short-circuit fires for all of them.
* Init cycle detection: because `"billing"` is added to `deps` by the macro, a dependency cycle
  `orders → billing → orders` is caught as a hard error by the existing topo-sort at startup.
* `SecurityContext` propagation: the generated `BillingApiRestClient` extracts the raw bearer token
  from the passed `SecurityContext` and forwards it in the `Authorization` header. The full
  `SecurityContext` struct is not serialised over the wire; the receiving module reconstructs context
  from the incoming `Authorization` and `x-secctx-bin` headers via its own middleware, consistent
  with vision ADR-0002 (`cpt-cf-adr-auth-edge-only`).

### Confirmation

* Unit test: macro expansion for `#[modkit::consumes(contract = BillingApi, from = "billing")]`
  produces a `ConsumerRegistration` with the correct `owner_module`, `dep_module`, and a `wire`
  closure that compiles against `billing_sdk` alone (no `billing` impl crate in scope).
* Unit test: `wire` closure short-circuits when `hub.try_get::<dyn BillingApi>().is_some()`.
* Integration test (Profile 1): `OrdersModule` initialises after `BillingModule` (topo-sort);
  `wire` short-circuits; `hub.get::<dyn BillingApi>()` returns the local implementation.
* Integration test (Profile 2/OoP): `OrdersModule` starts as OoP; bootstrap polls
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
* Bad, because a module that provides multiple contracts (e.g., `BillingApi` and `AuditApi`) requires
  an additional disambiguation step that cannot be expressed by name alone.
* Bad, because the factory is only available if the provider's inventory item was linked into the
  binary, reintroducing provider-crate linkage — the exact problem this ADR exists to eliminate.
* Bad, because convention-based name→type mapping is fragile across rename refactors; the framework
  cannot distinguish a missing dep from a misspelled dep name until runtime.

### Option B: `#[modkit::consumes]` Explicit Macro (chosen)

Consumer declares the contract type and dep name explicitly. Macro generates a typed factory owned
by the consumer binary. No inference, no provider linkage.

* Good, because the trait type is explicit — the compiler verifies it exists in the SDK crate at
  `cargo check` time.
* Good, because the factory is owned by the consumer binary; no provider code needs to be linked.
* Good, because one macro attribute per consumed contract serves as clear, greppable documentation
  of module dependencies.
* Neutral, because requires a new macro attribute; adds a small amount of syntax to learn.
* Neutral, because adding a consumed contract requires both a `Cargo.toml` dep on the SDK crate and
  a `#[modkit::consumes]` attribute — two places. Mitigated: omitting either produces a loud
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
  — `#[modkit::provides]` (producer side); this ADR adds the symmetric consumer-side counterpart.
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
* Topo-sort entry point: `libs/modkit/src/registry.rs` — `build_topo_sorted`; the `deps` injection
  from `#[modkit::consumes]` plugs into this existing mechanism.
* `ClientHub`: `libs/modkit/src/client_hub.rs` — `try_get`, `register`, `get` are the three methods
  used by the generated `wire` closure.
* OoP bootstrap integration point: `libs/modkit/src/bootstrap/oop.rs:533–598` — the background
  wiring task is inserted here, after `DirectoryClient` is connected and before `module.run()`.
