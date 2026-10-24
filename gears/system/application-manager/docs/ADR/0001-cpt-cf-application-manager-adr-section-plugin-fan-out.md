---
status: accepted
date: 2026-10-20
description: Every section plugin takes part in each seed and uninstall, and section plugins are discovered once at startup into a key map and priority order fixed for the process lifetime
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

<!-- /toc -->

**ID**: `cpt-cf-application-manager-adr-section-plugin-fan-out`

## Context and Problem Statement

Each kind of seeded data is owned by a section plugin, and every section plugin takes part in each seed and uninstall in a fixed order. The platform plugin model usually selects one plugin instance per request, or looks plugins up in the types registry when needed. How should the Application Manager find and route to its section plugins so that key and priority conflicts show up at deploy time?

## Decision Drivers

* `cpt-cf-application-manager-fr-required-sections`, `cpt-cf-application-manager-fr-plugin-extensibility`
* `cpt-cf-application-manager-fr-unique-section-keys`, `cpt-cf-application-manager-fr-section-priority`, `cpt-cf-application-manager-fr-ordered-dispatch`
* `cpt-cf-application-manager-fr-startup-checks`
* `cpt-cf-application-manager-constraint-toolkit-plugin-model`

## Considered Options

* Discovery at startup into a fixed set
* Discovery on every request
* A periodically refreshed plugin set
* A manifest-declared list of plugins to call
* A single plugin chosen with `choose_plugin_instance`

## Decision Outcome

Chosen option: "Discovery at startup into a fixed set", because it gives one plugin set for the process lifetime, makes conflicts and discovery failures fail at deploy time, and keeps the types registry off the seed and uninstall paths.

* At `start`, the gear lists the instances of the section plugin GTS type, resolves each client in ClientHub by exact GTS id, and reads each `key` through the plugin call guard. The registered instance priority orders the plugins.
* The key map and priority order stay fixed until the next start; an empty set is a valid start.
* Startup fails with no retry on any condition of `cpt-cf-application-manager-fr-startup-checks`, naming a reason from [DESIGN §4.1](../DESIGN.md#41-observability).
* Seed calls `apply` and uninstall calls `remove` on every plugin, one at a time in priority order.

### Consequences

* Each plugin registers its instance, priority, and scoped client in its own `init`.
* A manifest cannot select or add a plugin; a key no plugin owns makes it invalid.
* One broken registration blocks startup for the whole gear.
* A plugin change needs a restart (`cpt-cf-application-manager-constraint-restart-for-plugin-change`).

### Confirmation

* One startup test per startup check, each ending with its named reason and no retry; an empty registry starts with an empty key set.
* Seed and uninstall dispatch in priority order; uninstall reaches every plugin after a failure.
* No types-registry call after startup, and listing keys makes no plugin call.
* A manifest with an unowned key is invalid with an `unknown_section` problem naming it.

## Pros and Cons of the Options

### Discovery at startup into a fixed set

* Good, because conflicts and an unreachable registry fail at deploy time.
* Good, because requests have no registry dependency and an operation never sees the set change.
* Bad, because one broken registration blocks startup.

### Discovery on every request

* Good, because the set is always current without a restart.
* Bad, because a registry outage becomes a seed outage, and conflicts surface on a caller's request.
* Bad, because the set can change between a seed's validation and dispatch.

### A periodically refreshed plugin set

* Good, because plugin changes apply without a restart, and requests do not wait on the registry.
* Bad, because the set can change while an operation runs, and a refresh can introduce a conflict at run time.

### A manifest-declared list of plugins to call

* Good, because a manifest could target a subset of plugins.
* Bad, because every section is required, and an uninstall has no manifest to name plugins.

### A single plugin chosen with `choose_plugin_instance`

* Good, because it is the platform's common pattern.
* Bad, because a seed needs every plugin, not one winner, and it gives no key-to-owner routing.
