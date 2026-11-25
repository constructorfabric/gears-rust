---
description: "Pricing stores service bindings on plan revisions and freezes the selected bindings in the acceptance receipt; the Subscriptions posture names them."
status: proposed
date: 2026-11-25
---
# Pricing freezes service bindings in the acceptance receipt


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Entitlements keeps a local copy](#entitlements-keeps-a-local-copy)
  - [Subscriptions freezes the binding](#subscriptions-freezes-the-binding)
  - [Pricing stores and freezes](#pricing-stores-and-freezes)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-service-bindings-adr-binding-freeze`

## Context and Problem Statement

A purchase must deliver the service the buyer accepted, even after the plan, the catalog or a default changes. Entitlements needs the frozen binding at every revision of a purchase, and it must not read live terms. Someone must author the binding, freeze it with the prices and name it for each purchased component.

## Decision Drivers

- One immutable purchase evidence: the acceptance receipt.
- Unchanged prices must not hide a changed service.
- A component without a service must be distinguishable from missing evidence.
- Compatible increases must prove that they keep the same service, also from a later plan revision.
- Reuse the quota rule freeze (pricing D-547, D-548).

## Considered Options

- Entitlements keeps a local copy of the binding, filled from a live plan read.
- Subscriptions freezes the binding when it commits the purchase.
- Pricing stores bindings on plan revisions and freezes the selected ones in the receipt; the posture names them.

## Decision Outcome

**Pricing stores and freezes; the posture names.** Bindings are part of a plan revision, validated by Products at each boundary. Acceptance compares the buyer's expected service digest, validates the selected bindings at the quantity and writes the `service` member of the receipt in the same transaction as the prices. Each binding has a digest that excludes sale facts, and an execution digest that also excludes the plan revision. Subscriptions copies the binding IDs and digests into `serviceBindingRefs` on the posture grant and keeps them for the life of the component. Entitlements reads and verifies the receipt.

### Consequences

- `pricing-sdk` carries the binding shape and the digest functions that Pricing, Subscriptions and Entitlements share.
- The receipt grows in place; its schema stays version 1.
- Subscriptions refuses increases and additions of components with bindings before any effect until Pricing's change-delta acceptance exists (SVB-Q04, owner, 2026-11-25), checks them by execution digest and at the resulting quantity once it does, and refuses unsupported changes before commitment.
- Entitlements verifies a snapshot instead of reconstructing terms.

### Confirmation

Feature 2 criteria SVB-C15 to SVB-C20, feature 3 criteria SVB-C21 to SVB-C23 and feature 4 criteria SVB-C24 to SVB-C27 confirm this decision. This ADR changes no runtime code.

## Pros and Cons of the Options

### Entitlements keeps a local copy

- Benefit: no Pricing work.
- Cost: the copy is not purchase evidence; a live read after a plan change returns other terms.
- Cost: the owner ruled against it on 2026-11-25.

### Subscriptions freezes the binding

- Benefit: the posture owner holds what it names.
- Cost: a second freeze beside the receipt; prices and services could diverge.
- Cost: Subscriptions would need the plan and catalog evidence that Pricing already holds.

### Pricing stores and freezes

- Benefit: one receipt, one transaction, one digest for the accepted terms.
- Benefit: the same pattern as the quota rules.
- Cost: Pricing adds authoring, checks and a receipt member.

## More Information

[Feature 2](../features/0002-plan-bindings.md) defines authoring and the freeze. [Feature 3](../features/0003-posture-reference.md) defines the posture reference. [Feature 4](../features/0004-consumption.md) defines consumption.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision addresses:

- `cpt-cf-bss-service-bindings-fr-plan-binding`: Pricing stores bindings on plan revisions.
- `cpt-cf-bss-service-bindings-fr-acceptance`: Acceptance freezes the selected bindings with the prices.
- `cpt-cf-bss-service-bindings-fr-posture-reference`: The posture names the accepted bindings.
- `cpt-cf-bss-service-bindings-principle-frozen-evidence`: Fulfillment reads frozen evidence only.
