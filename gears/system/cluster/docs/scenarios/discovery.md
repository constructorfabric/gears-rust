# Service Discovery — Scenario Details (`SC-DISC-*`)

> Detail for §4 of the [scenario catalog](./README.md). Template: *Intent / Steps /
> Expected / Done-when*. Cache-only backends obtain discovery via
> `CacheBasedServiceDiscoveryBackend` (metadata filtering client-side,
> `metadata_pushdown == false`); native backends may push filtering down.

**SC-DISC-001 — registration assigns id; defaults Enabled** · L2 · ☐
- *Intent:* a registrant need not mint its own id, and instances are serving by default.
- *Steps:* `register(ServiceRegistration { instance_id: None, .. })`; then `discover` with the default filter.
- *Expected:* an `instance_id` is assigned; the instance appears with `state: Enabled`.
- *Done-when:* asserts a non-empty id and `Enabled` default.

**SC-DISC-002 — default filter is Enabled-only** · L2 · ☐
- *Intent:* the default discovery path must never route to drained instances.
- *Steps:* register two instances, set one `Disabled`; `discover(name, DiscoveryFilter::default())`.
- *Expected:* only the `Enabled` instance is returned.
- *Done-when:* asserts the disabled instance is excluded under the default filter.

**SC-DISC-003 — metadata predicates AND-combine** · L2 · ☐ · gate `metadata_pushdown` (else client-side)
- *Intent:* multi-attribute routing (e.g. region AND topic-shard) needs conjunctive matching with `Equals`/`OneOf`.
- *Steps:* register instances with varied metadata; `discover` with two predicates (`Equals`, `OneOf`).
- *Expected:* only instances matching *every* predicate are returned.
- *Done-when:* asserts AND-semantics for both predicate kinds. Pushdown backends assert server-side; others assert the SDK's client-side filter produces the same set.

**SC-DISC-004 — result order is unspecified** · L2 · ☐
- *Intent:* the contract makes no ordering guarantee; consumers needing determinism sort client-side.
- *Steps:* register several instances; `discover`; compare to expected *as a set*.
- *Expected:* the returned set equals the expected set, independent of order.
- *Done-when:* the suite compares result sets (e.g. via a `HashSet` of instance ids) and never depends on backend order.

**SC-DISC-005 — drain and deregister** · L2 · ☐
- *Intent:* gears flip serving intent for graceful drain, and remove themselves on shutdown.
- *Steps:* register, `set_state(Disabled)` (drain), then `deregister()`; a watcher observes events.
- *Expected:* after drain the instance is absent from the default filter but present under `Any`; after deregister the watcher sees `Left` and it is fully gone.
- *Done-when:* asserts the drain (filter exclusion) and the `Left` on deregister.

**SC-DISC-006 — liveness via TTL, not intent** · L2 · ☐
- *Intent:* intent ≠ health (ADR-008) — a registration disappears when its heartbeat/TTL stops, independent of `state`.
- *Steps:* register (Enabled), stop heartbeating, advance time past the registration TTL; `discover`.
- *Expected:* the instance disappears purely via TTL lapse, with no `set_state` call.
- *Done-when:* asserts TTL-driven disappearance distinct from the intent flag.

**SC-DISC-007 — topology watch is unfiltered** · L2 · ☐
- *Intent:* watches surface raw `Joined`/`Left`/`Updated`; filtering is the consumer's job, client-side.
- *Steps:* `watch(name)`, then register / update-metadata / deregister instances.
- *Expected:* `Change(Joined)`, `Change(Updated)`, `Change(Left)` events in mutation order, unfiltered.
- *Done-when:* asserts each `TopologyChange` variant is observed for the matching mutation.

**SC-DISC-008 — name scoped, metadata not scoped** · L2 · ☐
- *Intent:* the coordination namespace lives on the service `name`; metadata keys/values pass through unchanged (`fr-namespacing-sd-metadata-unscoped`).
- *Steps:* `scoped("eb").register(reg { name: "delivery", metadata: { region: "us-east" } })`; inspect backend keys and the discovered instance.
- *Expected:* backend sees service name `eb/delivery`; metadata key stays `region` (not `eb/region`).
- *Done-when:* asserts the name is prefixed and metadata is verbatim.

**SC-DISC-009 — recover membership after lag/reset** · L4 · ☐
- *Intent:* after a `Lagged`/`Reset`, re-reading via `discover` reconciles the consumer's view.
- *Steps:* establish a watch, induce lag/reset (Toxiproxy/backpressure), then `discover` to re-read.
- *Expected:* a `Lagged`/`Reset` signal followed by a `discover` that returns the true current membership.
- *Done-when:* asserts recovery to correct membership after the signal. *(L4 — fault injection.)*
