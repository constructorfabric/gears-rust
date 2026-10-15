---
status: proposed
date: 2026-10-15
description: Audit events go to a single in-process audit plugin through the transactional outbox, written in the same transaction as the change and delivered at least once
---
<!-- Created: 2026-10-05 by Constructor Tech -->
# Deliver Audit Events Through a Plugin Trait and the Transactional Outbox


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [In-process audit plugin trait, same-transaction outbox, at-least-once delivery](#in-process-audit-plugin-trait-same-transaction-outbox-at-least-once-delivery)
  - [Two synchronous hand-offs, each attempted once, with the plugin retrying](#two-synchronous-hand-offs-each-attempted-once-with-the-plugin-retrying)
  - [A gear-local append-only audit ledger](#a-gear-local-append-only-audit-ledger)
  - [An audit event schema on the platform event broker](#an-audit-event-schema-on-the-platform-event-broker)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-application-manager-adr-audit-via-plugin`

## Context and Problem Statement

Every seed and uninstall request must leave an audit trail: what was sent, by whom, and what the outcome was. The trail holds full manifests and caller subjects, so it is sensitive. Audit storage, retention and exposure belong to a replaceable audit plugin, not to the Application Manager. Quota Enforcement and License Enforcement in this repository already use plugin traits, with events written to the toolkit-db transactional outbox in the same transaction as the change and delivered at least once.

How should the Application Manager hand audit events to the audit plugin so that an audit outage does not block seeding and no change is dispatched without a durable audit record?

## Decision Drivers

* An audit outage must not block seeding or uninstall (`cpt-cf-application-manager-nfr-audit-completeness`).
* No dispatch without a durable audit record (`cpt-cf-application-manager-fr-audit-ordering`).
* One delivery mechanism shared with other gears, so operators and reviewers learn it once.
* A replaceable audit backend: storage, retention and exposure stay outside the gear.
* A simple protocol between the gear and the audit plugin.

## Considered Options

* In-process audit plugin trait, same-transaction outbox, at-least-once delivery
* Two synchronous hand-offs, each attempted once, with the plugin retrying
* A gear-local append-only audit ledger
* An audit event schema on the platform event broker

## Decision Outcome

Chosen option: "In-process audit plugin trait, same-transaction outbox, at-least-once delivery", because it keeps the request outcome independent of audit availability, gives a durable record before any dispatch, and matches the mechanism of Quota Enforcement and License Enforcement.

The decision has these parts:

* **Event catalog.** Three events: `accepted`, `finished` and `refused`.
  * `accepted` is committed to the outbox before any dispatch.
  * `finished` is written in the same transaction as the registry write or delete.
  * `refused` covers requests refused synchronously, before dispatch.
* **Event identity.** Every event carries an event id and the operation id or request id it belongs to.
* **Dispatcher.** A dispatcher (`cpt-cf-application-manager-component-audit-dispatcher`) delivers each event to the single registered audit plugin at least once, in order per operation.
* **Plugin obligations.** The plugin is idempotent by event id. It owns retention, purge, erasure and exposure; exposure is off by default (`cpt-cf-application-manager-interface-audit-plugin-api`).
* **No audit reads.** The gear never exposes audit data.

Decision scope: this ADR covers how audit events are produced, stored until delivery, and delivered. It does not decide dispatch order ([ADR 0003](./0003-cpt-cf-application-manager-adr-validate-all-then-dispatch.md)), the HTTP status of outcomes ([ADR 0006](./0006-cpt-cf-application-manager-adr-outcome-http-mapping.md)), or what the registry stores ([ADR 0005](./0005-cpt-cf-application-manager-adr-minimal-application-registry.md)).

### Consequences

* Good, because the audit plugin's availability never changes a request's outcome.
* Good, because a durable record exists before dispatch, and the finished record commits with the change it describes.
* Good, because the mechanism matches Quota Enforcement and License Enforcement.
* Bad, because manifests and caller subjects wait in gear storage until delivered.
* Bad, because the plugin must handle duplicate deliveries.
* Bad, because delivery lag is possible while the plugin is down.
* Bad, because events that exceed the attempt limit are rejected, and an operator signal reports them.

### Confirmation

* A test that no dispatch happens without a committed `accepted` event.
* A test that the `finished` event commits together with the registry write or delete, and rolls back with it.
* A test that redelivery of an event is harmless to an idempotent plugin.
* A test that seeds succeed while the audit plugin is down, and that events are delivered after it returns.
* Inspection that the public surface offers no operation that returns audit data.

## Pros and Cons of the Options

### In-process audit plugin trait, same-transaction outbox, at-least-once delivery

The gear writes audit events to the transactional outbox and a dispatcher delivers them to the audit plugin.

* Good, because the audit outage never blocks a request.
* Good, because the event and the change commit atomically.
* Good, because it reuses the delivery mechanism of other gears.
* Good, because the audit backend is replaceable.
* Neutral, because delivery is at least once, so the plugin deduplicates by event id.
* Bad, because sensitive data waits in gear storage until delivered.

### Two synchronous hand-offs, each attempted once, with the plugin retrying

The gear calls the audit plugin before dispatch and after the outcome, each attempted once.

* Good, because no gear-side storage of audit data is needed.
* Bad, because an audit outage blocks seeding and uninstall.
* Bad, because a failed second hand-off or a process stop leaves an event without its final result.
* Bad, because a timed-out first hand-off can leave an orphan event.
* Bad, because it is a mechanism no other gear uses.

### A gear-local append-only audit ledger

The gear keeps its own append-only ledger, as License Enforcement does.

* Good, because writes are atomic with the registry and never depend on a plugin.
* Bad, because the gear would own retention, purge, erasure and exposure of full manifests.
* Bad, because environments could not choose their own audit backend.
* Bad, because a ledger invites an audit read API on the gear.

### An audit event schema on the platform event broker

Audit events are published to the platform event broker and consumers record them.

* Good, because producers and consumers are decoupled.
* Bad, because the broker is not available yet, so the option is deferred.
* Bad, because publication still needs an outbox for atomicity with the change.

## More Information

* Review this decision when the platform event broker is available, or when an environment needs more than one audit sink.
* Related decisions: [ADR 0003](./0003-cpt-cf-application-manager-adr-validate-all-then-dispatch.md), [ADR 0005](./0005-cpt-cf-application-manager-adr-minimal-application-registry.md), [ADR 0006](./0006-cpt-cf-application-manager-adr-outcome-http-mapping.md).
* Platform plugin model background: [ClientHub and plugins](../../../../../docs/toolkit_unified_system/03_clienthub_and_plugins.md).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-application-manager-fr-audit-event` — defines the event catalog and the content each event carries.
* `cpt-cf-application-manager-fr-audit-ordering` — `accepted` is committed before dispatch; `finished` commits with the registry change.
* `cpt-cf-application-manager-nfr-audit-completeness` — every request yields durable events, and an audit outage does not block seeding.
* `cpt-cf-application-manager-component-audit-dispatcher` — delivers events at least once, in order per operation ([DESIGN](../DESIGN.md)).
* `cpt-cf-application-manager-interface-audit-plugin-api` — the plugin trait that receives events and owns retention, purge, erasure and exposure ([DESIGN](../DESIGN.md)).
