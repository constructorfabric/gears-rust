---
status: proposed
date: 2026-09-12
---

# ADR-0001: Admission Plugin Linked at Build Time, Not Discovered at Runtime

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [(a) Runtime GTS plugin discovery](#a-runtime-gts-plugin-discovery)
  - [(b) Build-time link](#b-build-time-link)
  - [(c) Runtime discovery with signed plugin instances](#c-runtime-discovery-with-signed-plugin-instances)
  - [(d) Plugin passed at construction](#d-plugin-passed-at-construction)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-license-enforcement-adr-compile-time-admission`

## Context and Problem Statement

A root grant may come into existence only through an admission plugin. The type of evidence that is sufficient is
different for each deployment:

- The own deployment of the vendor trusts the parameters of an authorized caller.
- A customer deployment must admit nothing without a vendor signature.

Resolvers on this platform bind their plugins at runtime. The plugin is a GTS plugin-spec instance in `types-registry`.
Vendor and priority select it, and ClientHub resolves it. In the deployment that is most important for licensing, the
operator owns the registry, the database, and the configuration.

How do we bind the admission plugin to the enforcement binary, so that the operator cannot replace the verifying plugin
with a permissive one?

## Decision Drivers

* The adversary of the untrusted build is the operator with root access to the host, the database, and the clock
  (PRD §3). A selection from data that the operator controls is not a defence.
* One domain model, lifecycle, check, and management semantics under every admission. Only the way root grants arrive
  is different (`cpt-cf-license-enforcement-fr-admission-plugin`).
* A build that never verifies evidence must link no verification code
  (`cpt-cf-license-enforcement-fr-verification-fixed`).
* Platform convention and operability: sibling resolvers discover plugins at runtime, and operators know that model.
* Configuration must not replace trust anchors. Issuing keys under the anchors still need rotation without a rebuild.

## Considered Options

* (a) Runtime GTS plugin discovery, as the sibling resolvers do
* (b) Build-time link: the binary links exactly one admission plugin crate, and the plugin registers itself at link
  time. The gear checks at init that exactly one plugin is registered. Trust anchors and verification code are only in
  the plugin crate that needs them.
* (c) Runtime discovery, restricted to plugin instances that carry a vendor signature
* (d) The binary passes the plugin to the gear at construction, so the type system enforces exactly one

## Decision Outcome

Chosen option: **(b) build-time link**.

Option (a) puts the choice of verifier in registry rows and configuration, and the operator can edit both at any time.
A licensing service with a verifier that is a runtime choice has no verifier.

Option (c) needs a compiled-in verifier for the allow-list. Thus it is (b) plus a runtime indirection. The indirection
adds attack surface and gives no benefit.

Option (d) gives a compile-time guarantee, but gears on this platform register at link time, not by construction in
the binary. (b) uses the platform mechanism, and the init check gives the same result before the gear serves any
request.

With (b), a deployment has no switch. Whoever assembles the binary decides the plugin, its trust anchors, and
whether verification code exists at all. A custom plugin is a crate that implements the trait, and its own binary
links it.

### Consequences

* The enforcement crate has no admission features. Each admission plugin is a separate crate that depends only on the
  SDK. The binary links one with `use <plugin> as _;`, as it links other gears and plugins.
* At init, the gear does not start if zero plugins or more than one plugin are registered.
* The verifier and the clock guard are in the plugin crate that verifies evidence. A binary that does not link such a
  crate contains no verification code. The check service calls the plugin through the trait at each check.
* The admission contract is a Rust trait in the SDK crate, not a GTS plugin spec. A new evidence format is a new plugin
  crate, never a change to the contract.
* Trust anchors are build inputs of the plugin crate that verifies evidence. Rotation of an anchor needs a rebuild.
  Issuing keys under the anchors rotate through signed trust bundles that verify against material that is already
  trusted. This rotation needs no rebuild.
* Telemetry reports the linked plugin, the anchor fingerprints, and whether verification code is present.
* This decision deviates from the runtime plugin convention of the platform. DESIGN §4 records the deviation, with
  platform architecture as review owner.
* The same conformance test suite runs the domain model, lifecycle, evaluator, and management API with each plugin
  crate. Thus builds with different plugins cannot drift into different products.

### Confirmation

* A test binary with zero plugins and a test binary with two plugins both fail at gear init.
* The dependency tree of a binary with a plugin that does not verify evidence contains no signature crate.
* The route table of a binary registers create routes only for the intakes that its plugin declares.
* No configuration key or registry entry exists that can select or disable the plugin.
* The shared conformance suite passes with each plugin crate.

## Pros and Cons of the Options

### (a) Runtime GTS plugin discovery

The platform convention: a plugin-spec instance in the registry, selected by vendor and priority.

* Good, because every operator and every other resolver already uses it.
* Good, because a plugin change needs no rebuild.
* Bad, because the operator of an untrusted deployment controls the registry and can select a permissive plugin.
* Bad, because a runtime choice puts both plugins and the verifier into every binary. This is against the goal to link
  no code that the selected plugin does not need.

### (b) Build-time link

The binary links exactly one plugin crate, which registers itself at link time. The plugin crate that verifies evidence
contains the anchors and the verifier.

* Good, because at runtime there is nothing to select, disable, or replace.
* Good, because the trusted build has no verification code, no verification dependencies, and no related attack
  surface.
* Good, because the plugin boundary stays a trait, so the domain model is identical under every plugin.
* Neutral, because rotation of a trust anchor needs a rebuild. Issuing keys still rotate through signed bundles.
* Bad, because it deviates from the runtime plugin discovery of the platform.
* Bad, because "exactly one" is checked at init, not at compile time.

### (c) Runtime discovery with signed plugin instances

The platform discovers plugins at runtime but admits a plugin only if the vendor signed its registration.

* Good, because it keeps the runtime plugin model that operators know.
* Bad, because verification of the allow-list needs a compiled-in verifier and anchors. Thus it has all the properties
  of (b) and adds a second trust path that can be wrong.
* Bad, because the plugin code is still in every binary.

### (d) Plugin passed at construction

The binary constructs the gear with the plugin as a parameter.

* Good, because the type system guarantees exactly one plugin at compile time.
* Bad, because gears on this platform register at link time. This gear would need a different registration path.

## More Information

The build-time mechanism is the platform mechanism that already registers gears and plugins at link time. This
decision only adds the init check that exactly one admission plugin is registered. The plugin that verifies evidence
decides the rotation and freshness of trust material.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses these requirements and design elements:

* `cpt-cf-license-enforcement-fr-admission-plugin` — exactly one plugin, linked, never selected at runtime.
* `cpt-cf-license-enforcement-fr-verification-fixed` — verification follows only from the linked plugin. A binary
  without a plugin that verifies evidence links no verification code.
* `cpt-cf-license-enforcement-contract-admission-plugin` — the contract is a Rust trait that one linked crate
  implements.
* `cpt-cf-license-enforcement-constraint-compile-time-admission` — this ADR is the rationale for that constraint.
