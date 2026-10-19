---
status: accepted
date: 2026-10-20
description: Every section plugin takes part in each seed and uninstall, and section plugins are discovered once at startup into an in-memory key map and priority order that stay fixed for the process lifetime
---
<!-- Created: 2026-10-05 by Constructor Tech -->
# Every Section Plugin Participates, Discovered Once at Startup

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Discovery at startup into a fixed set](#discovery-at-startup-into-a-fixed-set)
  - [Discovery on every request](#discovery-on-every-request)
  - [A periodically refreshed plugin set](#a-periodically-refreshed-plugin-set)
  - [A manifest-declared list of plugins to call](#a-manifest-declared-list-of-plugins-to-call)
  - [A single plugin chosen with `choose_plugin_instance`](#a-single-plugin-chosen-with-choose_plugin_instance)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-application-manager-adr-section-plugin-fan-out`

## Context and Problem Statement

A manifest carries one section per kind of seeded data, and each kind is owned by a section plugin. The Application Manager must decide how it finds section plugins and how it routes work to them. The platform plugin model offers two familiar patterns: select a single plugin instance for a request, or look plugins up in the types registry when needed. Which pattern fits a gear where every section plugin takes part in each seed and uninstall, in a fixed order, and where key and priority conflicts must show up at deploy time?

## Decision Drivers

* Every registered section is required, so every section plugin takes part in every seed and uninstall (`cpt-cf-application-manager-fr-required-sections`).
* A new kind of seeded data must need no change to the gear's own code (`cpt-cf-application-manager-fr-plugin-extensibility`).
* A section key must map to exactly one section plugin, and a conflict must be found at startup (`cpt-cf-application-manager-fr-unique-section-keys`).
* Dispatch order must be fixed by the registered priority, and an equal priority must be found at startup (`cpt-cf-application-manager-fr-section-priority`, `cpt-cf-application-manager-fr-ordered-dispatch`).
* Startup must fail loudly, with no retry, when discovery cannot answer, so an unreachable registry is never read as "no section plugins" (`cpt-cf-application-manager-fr-startup-checks`).
* Every plugin call, including the `key` call at startup, must end within the configured limit (`cpt-cf-application-manager-fr-bounded-plugin-calls`).
* Follow the platform plugin model (a GTS instance in the types registry and a scoped ClientHub client under that instance id) rather than invent a parallel one (`cpt-cf-application-manager-constraint-toolkit-plugin-model`).

## Considered Options

* Discovery at startup into a fixed set
* Discovery on every request
* A periodically refreshed plugin set
* A manifest-declared list of plugins to call
* A single plugin chosen with `choose_plugin_instance`

## Decision Outcome

Chosen option: "Discovery at startup into a fixed set", because discovery runs only at startup. The process keeps one fixed plugin set for its lifetime, key and priority conflicts and discovery failures fail at deploy time, and the seed and uninstall paths make no types-registry call.

The decision has these parts:

* **Participation.** Every section plugin takes part in each seed and uninstall.
* **Discovery once.** At `start`, the startup component lists the instances of the section plugin GTS type from the types registry and resolves each client in ClientHub by its exact GTS instance id. It reads each plugin's `key` through the plugin call guard. The registered GTS instance priority is the plugin's priority.
* **Fixed set.** The result is an in-memory key map and a priority order, sorted lowest value first. Both stay fixed for the process lifetime, and the startup component never runs again.
* **Startup checks.** Startup fails, with no retry, on any condition of `cpt-cf-application-manager-fr-startup-checks`. Each failure emits a startup failure event that names one reason, and the host exits with a non-zero status:
  * the types registry cannot answer: `types_registry_unreachable`, when the instance listing fails or does not answer in time, never read as empty;
  * a key or priority cannot be read: `priority_unreadable`, `plugin_client_unresolved` or `section_key_unavailable`, the last also when a `key` call does not answer within `plugin_call_timeout`;
  * a key is invalid: `invalid_section_key`;
  * two plugins share a key: `duplicate_section_key`;
  * two plugins share a priority: `equal_priority`;
  * a setting is missing or invalid: `invalid_setting`, raised in `init`.
* **Empty set.** A types registry that answers with no section plugins is a valid start. The key set is empty.
* **Seed dispatch.** A seed calls `apply` on the sections present, one at a time in priority order.
* **Uninstall.** An uninstall calls `remove` on every section plugin in priority order, even one never seeded for the application, and continues after failures (`cpt-cf-application-manager-fr-uninstall`). With an empty key set it succeeds with no plugin call.
* **Changes.** A plugin that is added, withdrawn or re-prioritized takes effect after a restart.

Decision scope: this ADR covers how section plugins are found, keyed and ordered inside the Application Manager. Schema sources are decided in [ADR 0002](./0002-cpt-cf-application-manager-adr-plugin-served-section-schemas.md). Audit ledger writes are decided in [ADR 0004](./0004-cpt-cf-application-manager-adr-audit-ledger.md). Permission decisions are decided in [ADR 0005](./0005-cpt-cf-application-manager-adr-platform-level-access.md).

### Consequences

* Each section plugin registers its GTS instance, its priority and a scoped ClientHub client in its own `init`, and answers `key` once `init` has finished.
* A manifest cannot select or add a section plugin. A key that no section plugin owns makes the manifest invalid, and the section plugin router calls only the plugins of the startup set. It never consults the types registry after startup and never retries a call.
* A section plugin that is registered but broken blocks startup for the whole gear. This is the deploy-time failure the drivers ask for.
* Listing section keys reads the in-memory map with no plugin call (`cpt-cf-application-manager-fr-discovery`).
* Seeding is unavailable while a restart for a plugin change runs (`cpt-cf-application-manager-constraint-restart-for-plugin-change`, `cpt-cf-application-manager-nfr-availability`).
* An operation never sees the plugin set change, so a seed and a later uninstall of one application use the same plugin set within one process.
* Settings are validated in `init`, so a missing or invalid setting fails the start before any discovery.

### Confirmation

* Startup tests, one per condition of `cpt-cf-application-manager-fr-startup-checks`: an unreachable or unanswering types registry (`types_registry_unreachable`), an unreadable priority, an unresolved client, a `key` call that fails or times out (`section_key_unavailable`), an invalid key, a duplicate key, an equal priority, and a missing or invalid setting. Each ends the start with its named reason and no retry.
* A startup test with a registry that answers with no plugins: the start succeeds with an empty key set, and an uninstall succeeds with no plugin call.
* Dispatch-order tests: a seed calls `apply` in priority order, lowest value first.
* An uninstall test with recording plugins: every plugin receives `remove` in priority order, including one never seeded, and the calls continue after a plugin fails.
* A test with a counting registry double: no types-registry call happens after startup.
* A key-listing test with a call-counting plugin: listing the section keys makes zero plugin calls.
* A validation test with a manifest that carries a key no section plugin owns: the manifest is invalid, and the report lists an `unknown_section` problem whose location names that key.

## Pros and Cons of the Options

### Discovery at startup into a fixed set

List the plugin instances once at `start`, read each key through the call guard, and keep the key map and priority order in memory.

* Good, because every section plugin takes part, as every section is required.
* Good, because key and priority conflicts, unreadable plugins and an unreachable registry fail at deploy time.
* Good, because seed and uninstall have no registry dependency and no discovery latency.
* Good, because routing by key is a map lookup with a fixed order, and an operation never sees the set change.
* Neutral, because a plugin change needs a restart, which the PRD accepts.
* Bad, because one broken registration blocks startup for the whole gear.

### Discovery on every request

List and resolve the plugins from the types registry on each seed and uninstall.

* Good, because the plugin set is always current, with no restart.
* Bad, because every seed and uninstall gains a registry dependency and latency, and a registry outage becomes a seed outage.
* Bad, because the set could change between validation and dispatch, or between a seed and its uninstall.
* Bad, because key and priority conflicts would surface on a caller's request, not at deploy time.
* Bad, because a failed lookup could be misread as "no section plugins".

### A periodically refreshed plugin set

Refresh the in-memory set from the types registry on a timer.

* Good, because plugin changes take effect with no restart.
* Good, because requests do not wait on the registry.
* Bad, because the set can change while an operation runs, so a seed and its uninstall may see different sets.
* Bad, because a conflict introduced by a refresh appears at run time, and the gear must decide whether to keep serving with a broken set.
* Bad, because it adds a background task and a staleness window that the startup failure model does not have.

### A manifest-declared list of plugins to call

Let each manifest name the plugins to call.

* Good, because a manifest could target a subset of plugins.
* Bad, because the caller would choose which plugins run, while every registered section is required.
* Bad, because an uninstall carries no manifest, so it could not reach every plugin.
* Bad, because a manifest could name a plugin that is not registered, which moves a deploy-time conflict to a request.

### A single plugin chosen with `choose_plugin_instance`

Pick one instance per request by the platform's usual priority-based selection.

* Good, because it is the most common pattern in the platform plugin model and needs little code.
* Bad, because it selects one plugin, while a seed needs every section plugin; the others would never see their sections.
* Bad, because priority would choose a winner, while here it only orders all participants.
* Bad, because it gives no key-to-owner routing and no startup detection of key conflicts.

## More Information

* Platform context: [ClientHub and plugins](../../../../../docs/toolkit_unified_system/03_clienthub_and_plugins.md).
* Revisit this decision if plugin changes must take effect with no restart.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-application-manager-fr-startup-checks` — every discovery, key, priority and setting failure stops startup with a named reason and no retry; an empty plugin set is a valid start.
* `cpt-cf-application-manager-fr-uninstall` — every section plugin receives `remove` in priority order, even one never seeded, and the calls continue after failures.
* `cpt-cf-application-manager-fr-ordered-dispatch` — seed dispatch follows the startup priority order.
* `cpt-cf-application-manager-fr-plugin-extensibility` — the gear finds section plugins by their GTS type in the types registry, so its code names no plugin and no kind of seeded data.
* `cpt-cf-application-manager-fr-required-sections` — every registered key is required, so every section plugin takes part.
* `cpt-cf-application-manager-fr-unique-section-keys` — one key per plugin, checked at startup and fixed until the next start.
* `cpt-cf-application-manager-fr-section-priority` — the registered priority is read at startup and must be unique.
* `cpt-cf-application-manager-fr-discovery` — section keys are listed from the in-memory map.
* `cpt-cf-application-manager-fr-bounded-plugin-calls` — the `key` call goes through the call guard.
* `cpt-cf-application-manager-constraint-restart-for-plugin-change` — plugin changes take effect on restart.
* `cpt-cf-application-manager-nfr-availability` — seeding pauses only while a restart runs.
* `cpt-cf-application-manager-component-startup-registration` — discovers the plugins once and publishes the startup result.
* `cpt-cf-application-manager-component-plugin-call-guard` — wraps every `key` call.
* `cpt-cf-application-manager-component-section-plugin-router` — dispatches in priority order over the fixed set.
* `cpt-cf-application-manager-component-request-orchestrator` — runs seed and uninstall dispatch through the router.
