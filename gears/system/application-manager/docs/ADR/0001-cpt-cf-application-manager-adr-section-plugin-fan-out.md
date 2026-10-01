---
status: proposed
date: 2026-09-30
description: Every section plugin takes part in each seed and uninstall, and section plugins are discovered once at startup into an in-memory key map and priority order
---
# Every Section Plugin Participates, Discovered Once at Startup


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Fan-out to every section plugin, discovered once at startup into an in-memory key map](#fan-out-to-every-section-plugin-discovered-once-at-startup-into-an-in-memory-key-map)
  - [A single plugin chosen with `choose_plugin_instance`](#a-single-plugin-chosen-with-choose_plugin_instance)
  - [A types-registry lookup on every seed](#a-types-registry-lookup-on-every-seed)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-application-manager-adr-section-plugin-fan-out`

## Context and Problem Statement

A manifest carries one section per kind of seeded data, and each kind is owned by a section plugin. The Application Manager must decide how it finds section plugins and how it routes each section to its owner. The platform plugin model offers two familiar patterns: select a single plugin instance for a request, or look plugins up in the types-registry when needed. Which pattern fits a manifest that must reach every section plugin, in a fixed order, with routing that stays clear and conflicts that show up at deploy time?

## Decision Drivers

* Every registered section is required, so every section plugin takes part in every seed and uninstall (`cpt-cf-application-manager-fr-required-sections`).
* Adding or removing a kind of seeded data must need no change to the gear's own code (`cpt-cf-application-manager-fr-plugin-extensibility`).
* A section key must map to exactly one section plugin, and a conflict must be found at startup rather than at seed time (`cpt-cf-application-manager-fr-unique-section-keys`).
* Dispatch and removal order must be fixed and announced by the section plugins, and priority collisions must be found at startup (`cpt-cf-application-manager-fr-section-priority`).
* Startup must fail loudly when discovery cannot answer, so an unreachable registry is never read as "no section plugins" (`cpt-cf-application-manager-fr-startup-checks`).
* Seed and uninstall run at deploy time and their duration is bounded by the ordered plugin calls only (`cpt-cf-application-manager-nfr-bounded-request-time`).
* Follow the platform plugin model (scoped registration by GTS instance id in ClientHub) rather than invent a parallel one.

## Considered Options

* Fan-out to every section plugin, discovered once at startup into an in-memory key map
* A single plugin chosen with `choose_plugin_instance`
* A types-registry lookup on every seed

## Decision Outcome

Chosen option: "Fan-out to every section plugin, discovered once at startup into an in-memory key map", because it is the only option that matches the meaning of a manifest: all sections are required, so all section plugins take part, and each section goes to the section plugin that owns its key. It also moves key and priority conflicts, and discovery failures, to startup, and keeps the seed and uninstall paths free of registry traffic.

Decision scope: this ADR covers how section plugins are found, keyed and ordered inside the Application Manager. It does not decide where schemas come from ([ADR 0002](./0002-cpt-cf-application-manager-adr-plugin-served-section-schemas.md)), how validation and dispatch proceed ([ADR 0003](./0003-cpt-cf-application-manager-adr-validate-all-then-dispatch.md)), the audit plugin (the single registered audit plugin, see [ADR 0004](./0004-cpt-cf-application-manager-adr-audit-via-plugin.md)), or how outcomes are reported over HTTP ([ADR 0006](./0006-cpt-cf-application-manager-adr-outcome-http-mapping.md)).

### Consequences

* Each section plugin registers a GTS instance of the Application Manager's section plugin type in the types-registry and a scoped ClientHub client under that instance id. The instance's registered priority is the section-plugin priority; the plugin itself announces its one section key.
* At startup the Application Manager lists the plugin type's instances once, resolves each one by scoped lookup, reads its key, and builds an in-memory key-to-plugin map and a dispatch order sorted by priority, lowest value first. Both are fixed until the next start. The key set is small and bounded, so an in-memory map is sufficient.
* Startup fails on a repeated key, a repeated priority, a plugin whose key or priority cannot be read, or an unreachable types-registry. A registry that answers with no section plugins is valid: the key set is empty and a valid manifest has an empty set of sections.
* The seed, uninstall and discovery-of-keys paths never touch the types-registry and never select a plugin per request. Listing section keys needs no plugin call (`cpt-cf-application-manager-fr-discovery`).
* Adding or removing a section plugin takes effect on restart. Seeding is unavailable during that restart and callers retry (`cpt-cf-application-manager-nfr-availability`). The strict rules apply at once, with no default sections and no grace period (`cpt-cf-application-manager-fr-plugin-rollout`).
* A priority change that reorders dispatch is treated as intentional; the Application Manager does not warn about it.
* Reviewer-visible trade-off: a section plugin that is registered but broken blocks startup for the whole gear. This is accepted as the deploy-time failure the drivers ask for.
* Concurrent requests for the same application id are resolved in [DESIGN §4.2](../DESIGN.md#42-concurrent-requests-for-the-same-application): they are left to the section plugins, the system adds no coordination, the last registry writer wins, and both requests are audited.

Impact by concern:

* Performance: no per-request discovery cost; per-request cost is the ordered plugin calls already bounded by the plugin time limit.
* Security: routing depends only on the startup-built map, so a manifest cannot select or add a section plugin. A key that no section plugin owns makes the manifest invalid. No section plugin is called before authorization (`cpt-cf-application-manager-nfr-authorized-before-plugin`).
* Reliability: a stale or changing plugin set cannot appear mid-request; the trade is that plugin changes need a restart.
* Data: the map and order are process memory only; nothing is persisted for this decision.
* Integration: uses the standard plugin registration model; section plugin authors need no Application Manager change.
* Operations: conflicts surface in startup logs and failed rollouts, not in seed failures. No runbook is defined here.
* Testing: see Confirmation.
* Compliance: no direct compliance effect.
* UX: no end-user UX effect; there is no end-user UI.
* Business: new kinds of seeded data do not wait on a shared gear change.

### Confirmation

* Startup tests: repeated key, repeated priority, unreadable key or priority, unreachable registry (fails), and an empty plugin set (starts) each give the expected result.
* A test with a section plugin added and then removed, with no change to gear code, demonstrates `cpt-cf-application-manager-fr-plugin-extensibility`.
* A test with a counting registry double shows that no types-registry call happens during seed, uninstall or listing keys.
* Dispatch-order tests confirm lowest priority value first on seed and on uninstall.
* Review of record: the [DESIGN](../DESIGN.md) describes the init flow as this ADR states.

## Pros and Cons of the Options

### Fan-out to every section plugin, discovered once at startup into an in-memory key map

Enumerate all plugin instances at startup, read key and priority, keep a key map and priority order in memory.

* Good, because it matches the requirement that every section is required and every section plugin takes part.
* Good, because key and priority conflicts, and unreadable plugins, fail at startup.
* Good, because seed and uninstall have no discovery dependency and no discovery latency.
* Good, because routing by key is a direct map lookup with a fixed, announced order.
* Neutral, because plugin changes need a restart, which the platform model and the PRD accept.
* Bad, because one broken registration blocks startup for the gear.

### A single plugin chosen with `choose_plugin_instance`

Pick one instance per request by the platform's usual priority-based selection.

* Good, because it is the most common pattern in the platform plugin model and needs little code.
* Bad, because it selects one plugin, while a manifest needs every section plugin; the other plugins would never see their sections.
* Bad, because selection by priority means priority chooses a winner, whereas here priority must only order all participants.
* Bad, because it gives no key-to-owner routing and no startup detection of key conflicts.

### A types-registry lookup on every seed

Enumerate and resolve section plugins from the types-registry on each request.

* Good, because the plugin set is always current with no restart.
* Bad, because it adds a registry dependency and latency to every seed and uninstall, and a registry outage would turn into a seed outage.
* Bad, because the key set and order could change between validation and dispatch, or between a seed and its uninstall.
* Bad, because key and priority conflicts would surface at seed time, on a caller's request, rather than at deploy time.
* Bad, because a failed lookup could be misread as "no section plugins", making an incomplete dispatch look valid.

## More Information

* Review: revisit if section plugins must be added without a restart, or if the key set stops being small and bounded. Such a change would supersede this ADR.
* Related decisions: [ADR 0002](./0002-cpt-cf-application-manager-adr-plugin-served-section-schemas.md), [ADR 0003](./0003-cpt-cf-application-manager-adr-validate-all-then-dispatch.md), [ADR 0004](./0004-cpt-cf-application-manager-adr-audit-via-plugin.md), [ADR 0005](./0005-cpt-cf-application-manager-adr-minimal-application-registry.md), [ADR 0006](./0006-cpt-cf-application-manager-adr-outcome-http-mapping.md).
* Platform context: [ClientHub and plugins](../../../../../docs/toolkit_unified_system/03_clienthub_and_plugins.md).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-application-manager-fr-discovery` — section keys are listed from the in-memory map without a plugin call.
* `cpt-cf-application-manager-fr-unique-section-keys` — one key per section plugin, checked at startup, fixed until the next start.
* `cpt-cf-application-manager-fr-section-priority` — priority is read at startup, fixes dispatch and removal order, and must be unique.
* `cpt-cf-application-manager-fr-startup-checks` — discovery failures and conflicts stop startup.
* `cpt-cf-application-manager-fr-required-sections` — every registered key is required, so every section plugin participates.
* `cpt-cf-application-manager-fr-plugin-extensibility` — plugins are added or removed with no gear code change.
* `cpt-cf-application-manager-fr-plugin-rollout` — the strict rules apply at once after the restart.
* `cpt-cf-application-manager-nfr-availability` — seeding is unavailable during the restart a plugin change needs.
* `cpt-cf-application-manager-nfr-bounded-request-time` — no discovery cost per request.
* `cpt-cf-application-manager-nfr-authorized-before-plugin` — routing is fixed at startup and no section plugin is called before authorization.
* `cpt-cf-application-manager-contract-section-plugin` — the contract exposes the key and the registered priority read at startup.
* `cpt-cf-application-manager-actor-section-plugin` and `cpt-cf-application-manager-actor-section-plugin-author` — the actors who register plugins under this model.
* `cpt-cf-application-manager-usecase-plugin-change` — the use case of adding or removing a section plugin.
