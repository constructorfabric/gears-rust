---
status: accepted
date: 2026-06-26
decision-makers: BFF gear authors
---

# Session storage as a swappable plugin, not a gear dependency


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [`SessionStore` SDK trait implemented by plugins](#sessionstore-sdk-trait-implemented-by-plugins)
  - [Depend on Redis directly inside the gear crate](#depend-on-redis-directly-inside-the-gear-crate)
  - [Store as a generic type parameter on the gear](#store-as-a-generic-type-parameter-on-the-gear)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bff-adr-session-store`
## Context and Problem Statement

The BFF gear must persist server-side session state with low-latency lookups,
atomic refresh-rotation, per-user indexing, and TTL expiry. The gear is intended to
be **reusable across products** (`cpt-cf-bff-fr-pluggable-store`). How should the gear
obtain its storage backend so that different consumers can run different backends
(or supply their own) without changing gear code?

## Decision Drivers

* Reusability: consumers must choose a backend (or write one) without forking the gear.
* Testability: tests and local dev must run with no external infrastructure.
* Consistency: a backend-selection mechanism already exists in the workspace, and
  `credstore` is a direct precedent — a *storage* gear whose backend is a plugin
  (`CredStoreClientV1` consumer + `CredStorePluginClientV1` plugin, via types-registry
  + `ClientHub`). No session/KV/state-store trait exists today, so this is greenfield.
* Fail-closed and stateless requirements (`cpt-cf-bff-nfr-fail-closed`,
  `cpt-cf-bff-nfr-stateless`) — the backend is authoritative; the gear holds no session
  state in memory.

## Considered Options

* Define a `SessionStore` SDK trait, implemented by plugins selected at composition time
* Depend on Redis directly inside the gear crate
* Make the store a generic type parameter on the gear

## Decision Outcome

Chosen option: **`SessionStore` SDK trait implemented by plugins**, because it is the
only option that makes the gear backend-agnostic at composition time using the
workspace's established plugin mechanism, while keeping the gear crate free of any
concrete backend dependency.

### Consequences

* `bff-sdk` must define the `SessionStore` plugin trait, its models, and a
  `SessionStoreSpecV1` GTS schema; plugins register a `PluginV1<SessionStoreSpecV1>`
  in types-registry and a scoped `ClientHub` client (same mechanics as `credstore` /
  `oidc-authn-plugin`).
* The gear resolves the store via `ClientHub` in `init`; if none is present it must
  refuse to start (fail-closed).
* The project must ship at least two plugins: `redis-session-store-plugin`
  (production) and `inmem-session-store-plugin` (dev/test).
* Atomicity guarantees for refresh-rotation (§3.6 DESIGN) become part of the trait
  contract, so every plugin must honor them.
* **New external dependency:** the workspace has no Redis client today (no
  `redis`/`fred`/`deadpool` in any `Cargo.toml`). `redis-session-store-plugin` adds the
  first one, so it needs dependency sign-off per `guidelines/DEPENDENCIES.md`. Isolating
  it in the plugin crate (not the gear or SDK) keeps the new dependency opt-in: consumers
  that pick `inmem` or another backend never compile Redis.

### Confirmation

Design/code review confirms the gear crate has no Redis/backend dependency in
`Cargo.toml`; an integration test runs the gear against both shipped plugins
unchanged (`cpt-cf-bff-fr-pluggable-store` acceptance).

## Pros and Cons of the Options

### `SessionStore` SDK trait implemented by plugins

* Good: backend-agnostic; matches existing plugin convention; trivial dev/test via in-memory plugin.
* Good: consumers can supply proprietary backends.
* Bad: one indirection layer; atomic operations must be expressed in the trait, constraining its shape.

### Depend on Redis directly inside the gear crate

* Good: simplest to implement; direct access to Redis primitives.
* Bad: every consumer must run Redis; cannot swap backends; tests need a Redis or a mock; least reusable.

### Store as a generic type parameter on the gear

* Good: zero-cost abstraction, compile-time backend selection.
* Bad: generics fight the `inventory`/`ClientHub` dynamic gear registration model; awkward composition; no runtime selection by config.

## More Information

Closest precedent: `gears/credstore` — a storage gear with a plugin backend
(`gears/credstore/plugins/static-credstore-plugin`). Plugin mechanics also mirror
`gears/system/authn-resolver/plugins/oidc-authn-plugin`. No pre-existing
session/KV/state-store trait or Redis client exists in the workspace.

## Traceability

* PRD: `cpt-cf-bff-fr-pluggable-store`, `cpt-cf-bff-nfr-fail-closed`, `cpt-cf-bff-nfr-stateless`
* DESIGN: [DESIGN.md](../DESIGN.md) §3.4
