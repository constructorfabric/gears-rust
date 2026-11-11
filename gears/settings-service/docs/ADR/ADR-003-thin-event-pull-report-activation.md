---
description: "ADR of the Settings Service: activation signals to consumers carry no value — a thin event, a pull of the current value, and a reported outcome."
status: accepted
date: 2026-10-23
decision-makers: Constructor Fabric Steering Committee
---

# ADR-003: Activation Signals Carry No Value — Thin Event, Pull, Report

**ID**: `cpt-cf-settings-service-adr-thin-event-pull-report-activation`

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Thin event + pull + report](#thin-event--pull--report)
  - [Fat event](#fat-event)
  - [Fat event with version](#fat-event-with-version)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

When a setting value changes, consuming gears must learn of it, apply it, and the platform must be able to tell whether they did (`cpt-cf-settings-service-fr-consumer-activation`). The transport is the platform Event Broker, whose delivery is at-least-once and unordered across tenants, whose topics are readable by every principal holding a `consume` grant, and whose log can be replayed.

An effective value is not a property of the setting alone. It is resolved per reader: through the tenant ancestor chain for `cascading` settings, under the reader's access decision, with `hidden` and `read_only` restrictions, and with `secret`-trait values masked on every path but the machine-only one. The same key therefore yields different values for different consumers and tenants.

How should a change notification be shaped so that every consumer applies the value it is entitled to, so that no value or secret is exposed through the broker, and so that a repeated, late or reordered delivery cannot apply a stale value?

## Decision Drivers

* The effective value depends on the reader; the resolution and authorization logic exists once, on the read path (DESIGN.md §4.2 *Value Resolver*, §4.8).
* `secret`-trait plaintext must never enter the settings database, cache, audit or an event (DESIGN.md §4.2 *Secret Manager*; PRD §5.7).
* Delivery is at-least-once and a consumer may receive a notification more than once; a repeat must not change the outcome (PRD §5.5, §7.2).
* A cascading change at an ancestor affects many descendant tenants with one write.
* Values are arbitrary GTS-typed JSON, including file references; the event schema must not vary with the value type.
* The platform must know, per setting and consumer, whether the value took effect (the activation outcome), not only that a message was delivered.
* A consumer that was down when the change happened must still converge on return.

## Considered Options

* **Thin event + pull + report** — the notification names the tenant, the subscriber and the changed keys; the consumer re-reads the effective value through the Settings Reader SDK under its own identity, applies it, and reports the applied value (a hash for secrets) through the SDK.
* **Fat event** — the notification carries the resolved value per consumer and tenant; the consumer applies what it received.
* **Fat event with version** — as above, plus a monotonic version so a consumer rejects an older value.

## Decision Outcome

Chosen option: **"Thin event + pull + report"**, because it is the only option under which the read path stays the single place that resolves and authorizes a value, no value or secret travels through the broker, and a repeated or reordered delivery is harmless by construction.

Concretely:

1. `change_notification` carries identifiers only: `tenant`, `subscriber`, the subscriber's changed keys with the `change_ref` of each change, and the value-scope subject when present (DESIGN-activation §4.4). It never carries a value.
2. On receipt the consumer reads each changed key through `SettingsReaderClient` under its own identity (DESIGN.md §4.5) and applies the result. A repeated notification leads to the same read and the same result.
3. The consumer reports the outcome per key through the SDK (`report_outcome`), echoing the value it applied — a hash for a `secret`-trait value. The service compares it with the expected value recorded when the change committed and records the outcome in the setting's history; a later change to the same key and tenant replaces the expectation (DESIGN-activation §4.2 *Activation Outcome Recorder*).
4. The replica broadcast `cache_invalidate` follows the same rule: identifiers only, eviction, re-resolution on the next read.

### Consequences

* Good: one resolution and authorization path; no second implementation of the effective-value logic in a publisher.
* Good: the broker never holds a value or a secret; replaying the topic reveals which keys changed, not what they became.
* Good: at-least-once delivery needs no deduplication on either side; several changes in a row collapse into one read.
* Good: one notification per subscriber and tenant names every subscribed key whose value changed there; tenants nobody subscribed for cost nothing.
* Bad: one extra round-trip per notification, and a read burst after a change — bounded by per-key subscriptions, at one store read per notified key (the re-read bypasses the effective-value cache).
* Bad: a consumer cannot apply a value while the Settings Service is unreachable; the service is a platform component expected to be available, and the activation state keeps the expectation open until the consumer reports.
* Bad: the consumer may read a value newer than the one the notification was about; a newer change replaces the expectation, so the report of the newer value counts as success and an older report is ignored rather than failed.

### Confirmation

* Contract test: the `change_notification` and `cache_invalidate` schemas contain no value-typed attribute.
* Integration test: delivering the same notification twice yields one applied value and two identical reports; delivering two notifications out of order yields the latest value applied, the expectation settled once, and the second report ignored.
* Review: any proposal to add a value attribute to a broker event amends this ADR.

## Pros and Cons of the Options

### Thin event + pull + report

* Good: see Consequences.
* Bad: extra round-trip; depends on service availability at apply time.

### Fat event

* Good: one hop; a consumer can apply without calling back.
* Bad: the publisher must resolve one value per (consumer, tenant) and repeat the reader's authorization — a second implementation of the read path.
* Bad: a `secret` value, or any `pii`-classified value, would sit in the broker log.
* Bad: a late or repeated delivery applies a stale value unless every consumer implements version checks.
* Bad: the event schema varies with the value type.

### Fat event with version

* Good: closes the stale-apply hole of the fat event.
* Bad: keeps every other cost of the fat event, and adds a version protocol every consumer must honour — the deduplication burden PRD §7.2 rules out.

## More Information

The activation outcome report is independent of this choice: even a fat event would need the consumer's report to tell an administrator that the value took effect. The report and the activation state are the cost of knowing the outcome, not of pulling the value.

A possible later refinement is to carry a value version so a consumer that already applied it may skip the read; the `change_ref` already serves as one. It changes nothing in this decision.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-settings-service-fr-consumer-activation` — the activation protocol shape.
* `cpt-cf-settings-service-fr-replica-coherence` — the replica broadcast carries identifiers only.
* DESIGN.md §4.4 *Activation signals on the broker* and [DESIGN-activation.md](../DESIGN-activation.md) §4.1, §4.2, §4.4, §4.5 — where the decision is specified.
