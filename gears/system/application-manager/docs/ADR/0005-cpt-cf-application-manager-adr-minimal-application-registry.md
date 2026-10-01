---
status: proposed
date: 2026-09-30
description: The registry keeps one minimal, rebuildable entry per seeded application, holding its id, version, last result, last seed time and audit id
---
# Keep a Minimal, Rebuildable Application Registry


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Minimal registry: id, version, last result, last seed time, audit id](#minimal-registry-id-version-last-result-last-seed-time-audit-id)
  - [Stateless: no registry, derive the list on demand](#stateless-no-registry-derive-the-list-on-demand)
  - [Registry that tracks applied state or diffs](#registry-that-tracks-applied-state-or-diffs)
  - [Registry that stores the manifest](#registry-that-stores-the-manifest)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-application-manager-adr-minimal-application-registry`

## Context and Problem Statement

The Application Manager must answer "what is installed here, at which version, and how did its last seed end?" without asking each consumer service ([PRD](../PRD.md) `cpt-cf-application-manager-fr-application-registry`, `cpt-cf-application-manager-fr-list-applications`). At the same time it holds no domain logic, passes sections through unchanged, and leaves re-seed meaning, versions and rollback to section plugins (`cpt-cf-application-manager-fr-pass-through`, `cpt-cf-application-manager-fr-plugin-owned-semantics`). How much state should the gear keep about applications, and what should that state mean?

## Decision Drivers

* The listing must give an operator each seeded application, its version and how its last seed ended, from one place, with no plugin call (`cpt-cf-application-manager-fr-list-applications`, `cpt-cf-application-manager-nfr-availability`).
* The gear must stay free of domain logic: it must not interpret versions, compare sections or keep per-section state (`cpt-cf-application-manager-fr-opaque-version`, `cpt-cf-application-manager-fr-pass-through`).
* Whatever the gear stores must be rebuildable by re-seeding, so no gear-specific backup, RPO or RTO is needed (`cpt-cf-application-manager-nfr-recovery`).
* Manifests are potentially sensitive and only the audit plugin may keep them (`cpt-cf-application-manager-fr-audit-event`); the registry must hold no personal data and no manifest content.
* Installed applications are a property of the environment, not of a tenant (`cpt-cf-application-manager-nfr-platform-scope`).
* Uninstall and the failure paths must have one simple, predictable effect on the registry (`cpt-cf-application-manager-fr-uninstall`, `cpt-cf-application-manager-fr-dependency-failures`).
* Small storage footprint: the registry grows by one entry per seeded application and nothing more.

## Considered Options

* Minimal registry: id, version, last result, last seed time, audit id
* Stateless: no registry, derive the list on demand
* Registry that tracks applied state or diffs
* Registry that stores the manifest

## Decision Outcome

Chosen option: "Minimal registry: id, version, last result, last seed time, audit id", because it is the smallest state that answers the listing question and links each entry to its audit event, while adding no domain meaning, no sensitive content and no state that re-seeding cannot recreate.

The decision has these parts:

* **Stored fields.** Each entry holds the application id (the key, compared only for equality), the version last seeded (opaque text), how the last seed ended, when it was seeded, and the audit id of that last seed's audit event. Nothing else of the application is stored.
* **Last result values.** The result is one of `succeeded`, `partially_failed` (at least one section succeeded and at least one failed), or the winning failure category after dispatch when no section succeeded. Category ranking after dispatch is set by `cpt-cf-application-manager-fr-outcome-precedence`, and its HTTP reporting is decided in [ADR 0006](./0006-cpt-cf-application-manager-adr-outcome-http-mapping.md).
* **Write rule.** A seed that passes validation creates or updates the entry, whatever the dispatch outcome, including a section plugin refusing as not permitted. A request that ends before dispatch (temporarily unavailable, not permitted by platform access control, not found, invalid manifest) leaves the registry untouched.
* **Re-seed carries no diff meaning.** A re-seed simply overwrites the entry with the newest values. The gear does not compare the new version with the old one, does not record what changed and does not decide whether a re-seed is an upgrade, a downgrade or a repeat.
* **Uninstall.** Once an uninstall is accepted (authorized, application found, request hand-off accepted by the audit plugin), the entry is deleted, whatever the section plugins return. Section plugin outcomes of an uninstall live only in the response and the audit event. The last seeded version is read from the entry before deletion and passed to the section plugins.
* **Recovery.** The registry is a rebuildable view of the last seeds. Recovery, including repair after a registry write failed post-dispatch, is a re-seed by each application's seeding caller.
* **Storage scoping.** The table also carries `owner_tenant_id`, a secure-ORM scoping value that is always the platform root tenant. It is not application data and not per-tenant ownership, and the sentence "nothing else of the application is stored" refers to application data.
* **Scope.** The registry is one platform-wide set held in platform storage, reachable only through the platform's access checks. It is not per tenant.

Decision scope: this ADR covers what the registry stores, when an entry is written or deleted, what a re-seed means for it, and how it is recovered. It does not decide validation and dispatch ([ADR 0003](./0003-cpt-cf-application-manager-adr-validate-all-then-dispatch.md)), the audit id and audit storage ([ADR 0004](./0004-cpt-cf-application-manager-adr-audit-via-plugin.md)), how outcomes are reported over HTTP ([ADR 0006](./0006-cpt-cf-application-manager-adr-outcome-http-mapping.md)), or concurrent seeds for the same application id, which [DESIGN §4.2](../DESIGN.md#42-concurrent-requests-for-the-same-application) resolves.

### Consequences

* The registry needs exactly one entry per application id, and the id is the only lookup key. The audit id makes the audit event of the last seed reachable through the audit plugin's own facilities, if it offers any ([ADR 0004](./0004-cpt-cf-application-manager-adr-audit-via-plugin.md)); the gear itself offers no audit read.
* Listing needs no plugin call, so it stays available during an audit outage. The trade-off is that the list shows only the last seed's outcome, not a history; history is the audit plugin's concern.
* The registry can disagree with section plugin state: a failed uninstall removes the entry although a plugin may still hold data, and a registry write failure after dispatch leaves the entry stale. Both are handled by the audit trail and by re-seeding, not by reconciliation logic in the gear. The post-dispatch registry failure outcome is defined in `cpt-cf-application-manager-fr-dependency-failures`. Likewise, a registry delete that fails after an uninstall was accepted still lets every section plugin be asked to remove the application, ends the request as an internal failure ranked by the after-dispatch order, is recorded in the final-result hand-off and logged, and is completed by uninstalling again (`cpt-cf-application-manager-fr-uninstall`).
* Registry storage must answer before anything is dispatched, so a storage outage makes seed, uninstall and listing temporarily unavailable, and it ranks first in the pre-dispatch outcome order for a permitted caller (`cpt-cf-application-manager-fr-outcome-precedence`); storage is consulted only for a permitted caller.
* Because the entry is written after dispatch, concurrent seeds of the same application id can interleave writes. This was open question OQ2 in the [PRD](../PRD.md) and is resolved in [DESIGN §4.2](../DESIGN.md#42-concurrent-requests-for-the-same-application), not here: the last registry writer wins.
* Adding or removing a section plugin never requires a registry migration, because the registry holds no per-section data ([ADR 0001](./0001-cpt-cf-application-manager-adr-section-plugin-fan-out.md)).

Impact by concern:

* Performance: reads and writes touch a single small entry per request, so registry cost is negligible next to the ordered sequence of plugin calls that bounds request time (`cpt-cf-application-manager-nfr-bounded-request-time`).
* Security: the registry is reachable only through the platform's access checks and holds nothing sensitive.
* Reliability: drift between the registry and section plugin state is repaired by re-seeding, not by reconciliation logic in the gear.
* Data: the registry holds no personal data and no manifest content. The caller subject, which can be personal data, lives only in the audit event.
* Integration: listing needs no plugin call; the audit id links to the audit plugin's own facilities, if it offers any.
* Operations: no gear-specific backup, retention or purge is needed. Platform storage baselines apply. Registry failures are surfaced through the failed-hand-off and outcome signals described in `cpt-cf-application-manager-nfr-operational-signals`.
* Testing: see Confirmation.
* Compliance: erasure requests for the registry are trivial; for audit events they belong to the audit plugin.
* UX: operators and product owners get one answer to "what is installed here?".
* Business: application teams recover from any failure the same way, by seeding again (`cpt-cf-application-manager-fr-retry`).

### Confirmation

* Confirmation of storage scoping: the registry table carries the platform root tenant as its scoping value and no other tenant value.
* Review of record: the registry section of the [DESIGN](../DESIGN.md) matches the field list above: no field beyond those named, and no manifest or section content.
* Tests that compare the registry before and after each outcome that ends before dispatch (unchanged) and each outcome after dispatch (created or updated, with the correct last result).
* A test that deletes the entry on an accepted uninstall regardless of section plugin outcomes, and leaves it unchanged on a not permitted, unavailable or not found uninstall.
* A test that empties the registry, re-seeds every application and finds one entry each (`cpt-cf-application-manager-nfr-recovery`).
* Inspection that no registry read or write skips the access check and that no per-tenant copy exists (`cpt-cf-application-manager-nfr-platform-scope`).
* Inspection that the public surface offers no operation that compares versions or reports differences between seeds.

## Pros and Cons of the Options

### Minimal registry: id, version, last result, last seed time, audit id

Keep one small entry per seeded application, written after dispatch and deleted on accepted uninstall.

* Good, because it answers the listing question directly, with no plugin call and no dependency on the audit plugin being up.
* Good, because it carries no domain meaning, so the gear stays free of domain logic and version interpretation.
* Good, because it holds nothing sensitive or personal, keeping the gear's data stewardship trivial.
* Good, because re-seeding fully rebuilds it, so no gear-specific backup or recovery objective is needed.
* Good, because the audit id links each entry to its audit event without the gear storing audit data.
* Neutral, because it shows only the latest outcome per application; history lives in the audit plugin.
* Bad, because it can drift from section plugin state after a failed uninstall or a post-dispatch registry failure, and only a re-seed corrects it.

### Stateless: no registry, derive the list on demand

Keep no application state; answer "what is installed" by asking section plugins or by reading the audit plugin.

* Good, because the gear owns no storage and has no registry outage mode.
* Bad, because the gear would need a plugin call or an audit read per listing, but section plugins have no obligation to know application ids and the gear offers no audit read by design ([ADR 0004](./0004-cpt-cf-application-manager-adr-audit-via-plugin.md)).
* Bad, because listing would then depend on the audit plugin or on every section plugin being up, breaking the availability rule for listing.
* Bad, because uninstall could not tell "not found" from "found", and could not pass the last seeded version to section plugins.
* Bad, because it recreates the original problem: each consumer would be asked one by one and could disagree.

### Registry that tracks applied state or diffs

Record, per application, what each section plugin applied or what changed between seeds.

* Good, because it could support tooling that reports drift or previews upgrades.
* Bad, because it requires the gear to compare or interpret sections and versions, which is domain logic owned by section plugins, and contradicts pass-through with no history.
* Bad, because it duplicates state that section plugins already hold and can go out of step with it, without the gear being able to repair it.
* Bad, because it makes the registry grow with section content and requires migration whenever a section plugin is added or removed.
* Bad, because the meaning of "applied" differs per section plugin (replace, partial update, idempotent or not), so no single model is correct.

### Registry that stores the manifest

Keep the last manifest per application in the registry so it can be re-dispatched or shown.

* Good, because the gear could replay a seed without the caller, and operators could see the last manifest.
* Bad, because manifests are potentially sensitive and only the audit plugin may keep them, under exposure rules the gear cannot enforce for its own table.
* Bad, because replaying or driving re-seeds is out of scope; each application's seeding caller owns re-seeding.
* Bad, because a second copy of the manifest creates a second place for erasure, retention and access rules, and could leak other section plugins' data past their authorization.

## More Information

Related decisions:

* [ADR 0001](./0001-cpt-cf-application-manager-adr-section-plugin-fan-out.md): the set of section keys, and therefore of uninstall targets, is fixed at startup, so the registry needs no per-section data.
* [ADR 0003](./0003-cpt-cf-application-manager-adr-validate-all-then-dispatch.md): validation before dispatch and the ordered dispatch whose section outcomes yield the last result value.
* [ADR 0006](./0006-cpt-cf-application-manager-adr-outcome-http-mapping.md): how outcome categories, ranked by `cpt-cf-application-manager-fr-outcome-precedence`, are reported over HTTP, with partial failure reported as an error.
* [ADR 0004](./0004-cpt-cf-application-manager-adr-audit-via-plugin.md): the audit plugin issues the audit id at the request hand-off, and owns audit storage and exposure.

Review and supersession: revisit this decision if a consumer needs history of past seeds or per-section applied state in the gear itself, or if per-tenant installs enter scope. Either change would supersede this ADR and would need to weigh the data, privacy and domain-logic costs recorded above. Concurrent seeds for the same application id are not decided here; they are resolved in [DESIGN §4.2](../DESIGN.md#42-concurrent-requests-for-the-same-application).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-application-manager-fr-application-registry` — defines the stored fields, the write rule and the last result values.
* `cpt-cf-application-manager-fr-uninstall` — the entry is deleted once the uninstall is accepted, whatever the section plugins return.
* `cpt-cf-application-manager-fr-list-applications` — listing reads the registry with no plugin call.
* `cpt-cf-application-manager-fr-dependency-failures` — registry storage must answer before dispatch; a write failure after dispatch is an internal failure repaired by re-seeding.
* `cpt-cf-application-manager-fr-application-id` — the id is opaque and used only for equality as the registry key.
* `cpt-cf-application-manager-fr-opaque-version` — the version is stored as opaque text and never compared.
* `cpt-cf-application-manager-fr-pass-through` — the registry keeps no section content or per-section state.
* `cpt-cf-application-manager-fr-retry` — retry is a fresh seed, because no applied state is kept.
* `cpt-cf-application-manager-nfr-recovery` — the registry is fully rebuilt by re-seeding.
* `cpt-cf-application-manager-nfr-platform-scope` — one platform-wide set, reachable only through access checks.
* `cpt-cf-application-manager-nfr-availability` — listing stays available while the audit plugin is down.
* `cpt-cf-application-manager-usecase-list-applications` — the operator lists seeded applications from the registry.
* `cpt-cf-application-manager-usecase-uninstall` — uninstall deletes the entry once accepted.
* `cpt-cf-application-manager-usecase-retry` — retry by seeding again.
* `cpt-cf-application-manager-actor-platform-storage` — holds the registry and nothing else of the gear's data.
* `cpt-cf-application-manager-actor-operator` — reads the registry through the listing.
