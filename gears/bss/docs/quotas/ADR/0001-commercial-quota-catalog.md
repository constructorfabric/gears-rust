---
description: "Products owns the commercial quota catalog for BSS, using GTS definitions and reviewed adapter capabilities."
status: accepted
date: 2026-11-16
---
# Products owns the commercial quota catalog


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Pricing reads adapter capabilities directly](#pricing-reads-adapter-capabilities-directly)
  - [GTS Registry owns the sellable catalog](#gts-registry-owns-the-sellable-catalog)
  - [Products owns the commercial catalog](#products-owns-the-commercial-catalog)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-quotas-adr-commercial-catalog`

## Context and Problem Statement

Adapters report which allocation limits OSS supports. Plan authors need approved definitions with clear units, conditions and sale availability. One BSS owner must decide what can enter a plan and how capability changes affect new sales.

## Decision Drivers

- Give catalog approval and sale availability one business owner.
- Use registered GTS identities and keep accepted purchase terms unchanged.
- Give Pricing a consistent catalog across adapters.
- Keep OSS integration details behind the adapter contract.
- Report capability changes without silently changing purchased rights.

## Considered Options

- Pricing reads adapter capabilities directly.
- GTS Registry owns the sellable catalog.
- Products owns the commercial catalog.

## Decision Outcome

**Products owns the commercial catalog.** It already defines resource meaning and product mappings. It will also own catalog review, qualification references and sale availability. Pricing uses the approved catalog to author and validate plan rules.

GTS Registry stores schemas and named definitions. Products uses their identities and keeps historical evidence. Discovered and manually authored definitions go through the same review.

We choose Products to keep approval and sale availability with one business owner. All three options can save purchase snapshots. Preserving history alone does not justify this choice.

### Consequences

- Products publishes approved versions of definitions and adapter mappings. It keeps their source, review and qualification evidence. Pricing owns plan amounts and accepted terms.
- Products applies its authorization and audit rules to catalog reads and publication. Discovery cannot approve entries. Discovered and manual input follow the same checks.
- Products supplies reviewed names, units and conditions to the plan editor without per-row adapter or registry calls.
- Products tracks capability changes and sale availability. If a profile loses qualification, it cannot support new publication or purchases. Existing purchase terms stay unchanged. If fulfillment fails, recovery still applies.
- A failed discovery or registry read does not mean a capability was removed. Historical records do not prove current availability.
- Each adapter design defines its OSS components, connections and quota application. The catalog stores no live tenant usage or runtime counters.

### Confirmation

Check that the catalog and plan-authoring contracts follow these responsibilities. Verify approval for both input paths, authorized reads, exact historical content and capability changes. Feature 1 covers Q-C38, Q-C57 and Q-C67. Feature 5 covers qualification withdrawal and Q-C21.

This ADR changes no runtime code or database schema. Implementation must meet the owner contracts and qualification checks in DESIGN.

## Pros and Cons of the Options

### Pricing reads adapter capabilities directly

Pricing and the plan editor read definitions from adapters. They can cache results and save purchase snapshots.

- Benefit: no separate Products publication step or catalog model.
- Cost: Pricing must own review and lifecycle rules, or integrations must repeat them.
- Cost: consumers must handle adapter differences, outages and changes to provide a consistent plan editor.

This can work, but catalog ownership moves into Pricing or is split across integrations.

### GTS Registry owns the sellable catalog

The registry stores definitions, approvals, product mappings, qualification and sale availability.

- Benefit: one source provides definitions and sale availability.
- Benefit: no separate Products catalog API or store.
- Cost: the shared registry must support product lifecycle, authorization and audit rules.
- Cost: registry changes must serve both type management and commercial workflows.

This can work with those extensions. Registering a capability alone does not approve it for sale.

### Products owns the commercial catalog

Products reviews discovered and manually authored entries, then publishes them using registered definitions.

- Benefit: one business owner defines resource meaning, product mappings and approval.
- Benefit: Pricing uses one catalog contract across integrations.
- Cost: Products must publish entries, store history and maintain mappings.
- Risk: catalog availability can lag OSS changes. Products must track qualification and capability changes. Approval does not guarantee live capacity.

## More Information

[Catalog onboarding](../features/0001-catalog.md) defines intake, review and reads. [Adapter qualification](../features/0005-qualification.md) covers availability and capability changes. Platform security and audit rules still apply. This decision adds no compliance exception or performance target.

Revisit this choice if a shared platform catalog can take over the commercial lifecycle. Record that change in a new ADR. Migrate approvals, mappings, qualification and history while keeping accepted references and purchase evidence readable.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision addresses:

- `cpt-cf-bss-quotas-fr-catalog`: Products stages discovery and manual input for review.
- `cpt-cf-bss-quotas-fr-catalog-gts`: Catalog entries use registered metric and unit identities.
- `cpt-cf-bss-quotas-fr-catalog-approval`: Both input paths require the same approval.
- `cpt-cf-bss-quotas-fr-catalog-history`: Published meaning remains readable for accepted purchases.
- `cpt-cf-bss-quotas-fr-plan-catalog`: Plan authors read approved definitions and current sale availability.
- `cpt-cf-bss-quotas-component-commercial-chain`: Products, Pricing and adapters retain distinct responsibilities.
