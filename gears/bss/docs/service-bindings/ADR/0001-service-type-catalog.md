---
description: "Products owns service types, resource capability profiles and SKU qualification; adapter owners register service type instances under Products schemas."
status: proposed
date: 2026-11-25
---
# Products owns service types and SKU qualification


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Entitlements owns the service type schema](#entitlements-owns-the-service-type-schema)
  - [The adapter owner keeps a private catalog](#the-adapter-owner-keeps-a-private-catalog)
  - [Products owns the schemas and the catalog](#products-owns-the-schemas-and-the-catalog)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-service-bindings-adr-service-catalog`

## Context and Problem Statement

Entitlements must send an adapter an exact execution service type and a qualified capability profile for each purchased service. Plan authors need reviewed services with clear units, parameters and conditions. One BSS owner must decide which service types exist, which SKUs may deliver them and when they are sellable. The Entitlements design first named an adapter-owned descriptor schema in the Entitlements namespace. Products and Pricing are built in this repository and cannot depend on an Entitlements SDK. The owner ruled on 2026-11-25 that the providers own the real contract.

## Decision Drivers

- Give service review, SKU qualification and sale availability one business owner (ENT-D-22).
- Let the adapter owner state what a service means without giving it commercial authority.
- Keep accepted purchases tied to immutable, exactly referenced content.
- Reuse the commercial quotas catalog pattern instead of a second one.
- Keep Pricing and Products free of a dependency on the Entitlements SDK.

## Considered Options

- Entitlements owns the service type schema; adapters register descriptors; Products only qualifies.
- The adapter owner keeps a private catalog that Pricing reads.
- Products owns the schemas and the catalog; adapter owners register instances.

## Decision Outcome

**Products owns the schemas and the catalog.** `products-sdk` declares `gts.cf.bss.products.service_type.v1~` and `gts.cf.bss.products.service_parameters.v1~`. The adapter owner registers service type instances and final parameter schemas in its own namespace. Products reviews each into an immutable service type version, keeps resource capability profile versions and records the qualification of each SKU version as a service definition version. Availability is separate and starts disabled.

This is the split that the quotas track uses for metrics (products P-D-267, ADR `cpt-cf-bss-quotas-adr-commercial-catalog`). We choose it to keep review and sale availability with one owner and to keep the contract in a crate that Pricing and Entitlements can both use.

### Consequences

- Products adds three versioned record kinds, one review path, reads and `validate_service_bindings`. It adds no Product parent and no Service entity.
- The Entitlements design drops `gts.cf.bss.entitlements.service_type.v1~` and references the Products schema. Its `ServiceTypeRefV1` names a Products service type version.
- The adapter's resource plugin instance advertises Products service type and profile versions. It can do so only after Products approves them.
- A changed registry instance needs a new reviewed version. Accepted purchases keep the version they froze.
- Registration and discovery never enable a sale. Qualification evidence and an availability decision do.

### Confirmation

Feature 1 criteria SVB-C10 to SVB-C14 and feature 5 criteria SVB-C28 and SVB-C29 confirm this decision. This ADR changes no runtime code.

## Pros and Cons of the Options

### Entitlements owns the service type schema

- Benefit: the consumer defines what it receives.
- Cost: Pricing and Products would need the Entitlements SDK, which is built elsewhere, or a copy of it.
- Cost: review and availability would still be Products', so the contract would be split between three owners.

### The adapter owner keeps a private catalog

- Benefit: no Products work.
- Cost: every adapter repeats review, history and availability; Pricing reads several catalogs with different outages.
- Cost: no single owner decides what is sellable.

### Products owns the schemas and the catalog

- Benefit: one owner and one contract crate for meaning, qualification and availability.
- Benefit: the same pattern as the quota catalog.
- Cost: Products builds and keeps the catalog and its history.
- Risk: the catalog can lag adapter changes; qualification controls what is sold.

## More Information

[Feature 1](../features/0001-service-catalog.md) defines review and reads. [Feature 5](../features/0005-qualification.md) defines qualification and availability. Revisit this choice if a platform catalog takes over the commercial lifecycle; keep every accepted reference readable.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision addresses:

- `cpt-cf-bss-service-bindings-fr-service-type`: Products publishes reviewed versions of registered service types.
- `cpt-cf-bss-service-bindings-fr-sku-qualification`: Products qualifies SKU versions in service definitions.
- `cpt-cf-bss-service-bindings-fr-availability`: Availability is separate and starts disabled.
- `cpt-cf-bss-service-bindings-component-commercial-chain`: The adapter owner, Products and Pricing keep distinct responsibilities.
