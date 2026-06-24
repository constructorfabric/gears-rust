# Resolution & Capability Validation — Scenario Details (`SC-RESV-*`)

> Detail for §5 of the [scenario catalog](./README.md). Template: *Intent / Steps /
> Expected / Done-when*. These exercise the SDK resolver (`*V1::resolver(hub).profile(P)
> .require(cap).resolve()`); SC-RESV-001..003 are already covered by the SDK smoke tests
> (`tests/resolution.rs`).

**SC-RESV-001 — resolution succeeds for a satisfied backend** · L2 · ☑
- *Intent:* a consumer that declares only capabilities the bound backend provides gets a working facade.
- *Steps:* register a backend under profile `P`; `resolver(hub).profile(P).require(cap_it_has).resolve()`.
- *Expected:* `Ok(*V1)` — a usable, cheap-clone facade.
- *Done-when:* asserts a successful resolve and a working call through the facade.

**SC-RESV-002 — capability mismatch fails startup** · L2 · ☑
- *Intent:* a guarantee the backend cannot meet must fail loudly at resolution, never silently degrade in production.
- *Steps:* register a backend lacking a feature; `require` that feature and `resolve()`.
- *Expected:* `Err(CapabilityNotMet { primitive, capability, provider })` naming all three.
- *Done-when:* asserts the variant and that the message names primitive, capability, and the concrete provider.

**SC-RESV-003 — unbound profile** · L2 · ☑
- *Intent:* resolving a profile no operator bound is a clear, distinct error.
- *Steps:* `resolver(hub).profile(Unbound).resolve()` with nothing registered under it.
- *Expected:* `Err(ProfileNotBound { profile })`. (Omitting `.profile(...)` entirely → `ProfileNotSpecified`.)
- *Done-when:* asserts `ProfileNotBound`, and separately `ProfileNotSpecified` for the no-profile path.

**SC-RESV-004 — honest-declaration enforcement** · L2 · ☐
- *Intent:* the capability gate must bind to the backend's *declared* `features()`/`consistency()`, so an honestly-under-declaring backend is correctly rejected (the inverse of a backend that lies).
- *Steps:* a backend declaring `features().prefix_watch == false`; a consumer `require(CacheCapability::PrefixWatch)`.
- *Expected:* `Err(CapabilityNotMet)` — the gate trusts the declaration, not the runtime behavior.
- *Done-when:* asserts rejection driven purely by the declared characteristic. *(Cross-checked in the conformance suite: a backend whose declaration disagrees with its observed behavior fails some `SC-*` scenario.)*
