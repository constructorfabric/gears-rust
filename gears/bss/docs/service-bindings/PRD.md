---
description: "Product requirements for service types, SKU qualification, plan service bindings, their freeze at purchase and their use in service fulfillment."
---

# PRD: Service Bindings


<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
  - [4.3 Future scope](#43-future-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 Service catalog](#51-service-catalog)
  - [5.2 Plan service bindings](#52-plan-service-bindings)
  - [5.3 Acceptance and evidence](#53-acceptance-and-evidence)
  - [5.4 Posture and fulfillment](#54-posture-and-fulfillment)
  - [5.5 Release split](#55-release-split)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Capability-specific requirements](#61-capability-specific-requirements)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
  - [8.1 Buy a managed service](#81-buy-a-managed-service)
  - [8.2 Increase the quantity](#82-increase-the-quantity)
  - [8.3 Change the catalog after a purchase](#83-change-the-catalog-after-a-purchase)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

**Status: proposed.** The first release delivers provision, withdraw, pause, restore and service-stop. Retention, export, holds, disposal and fulfillment outputs are the second release. The [qualification gates](DESIGN.md#42-integration-qualification-gates) define what must be proven before a sale. Example services and amounts are not platform defaults.

## 1. Overview

### 1.1 Purpose

Define how an execution service that a resource adapter supports becomes a qualified SKU offer, a plan binding, frozen purchase evidence and a posture reference. Entitlements uses that chain to deliver the service through the selected adapter. Customers and operators must be able to trace each delivered service to its purchase and to the exact terms that the buyer accepted.

This PRD defines commercial outcomes and business acceptance criteria. [DESIGN](DESIGN.md) specifies the architecture, the owner contracts and the path from the Products catalog to the adapter request. [DECOMPOSITION](DECOMPOSITION.md) maps the requirements to five features: [service catalog](features/0001-service-catalog.md), [plan service bindings](features/0002-plan-bindings.md), [posture reference](features/0003-posture-reference.md), [consumption](features/0004-consumption.md) and [qualification](features/0005-qualification.md). JSON examples are in `schemas/`.

### 1.2 Background / Problem Statement

A tenant buys a plan that includes a service, for example managed backup for five workloads. BSS keeps the accepted terms and tells the selected adapter which service to provide, with which parameters and how many commercial service units. OSS decides which resources implement the service.

The [Entitlements service fulfillment feature](../../entitlements/docs/features/05-resource-execution.md) needs a frozen service binding: which SKU becomes which service type, under which qualified capability profile, with which quantity rule, target rule and parameters. Today no provider defines it. The Products, Pricing and Subscriptions designs do not mention service types or service bindings.

Without one contract, a service could be inferred from a SKU name. A plan edit could change an old purchase. Unsupported parameters could reach an adapter. Missing evidence could look like a purchase without a service. A local copy of the contract in Entitlements would not bind Pricing, which freezes the terms. The owner ruled on 2026-11-25 that the providers design and build the real contract, as the commercial quotas track did.

### 1.3 Goals (Business Outcomes)

- Every delivered service traces to an exact accepted binding, plan revision, SKU version, service type version and profile version.
- Only reviewed service types and qualified SKU and profile pairs can be sold.
- Catalog, plan and default changes after a purchase do not change the service that the purchase delivers.
- Missing or inconsistent evidence blocks new fulfillment and stays visible. It never means "no service".
- Once Pricing's change-delta acceptance exists, compatible quantity increases change the commercial service units of the same service. They never create a second service.
- Second-release capabilities are refused before any effect.

### 1.4 Glossary

| Term | Definition |
| --- | --- |
| Service type | An execution service that an adapter understands, such as managed backup. The adapter owner registers it as a GTS instance of a Products schema. Products reviews it into immutable versions. |
| Parameter schema | The closed set of parameters that a service type accepts, registered with the service type. |
| Commercial service unit | The unit in which a binding counts a service, as the service type defines it, such as one protected workload. It is not a VM count or a topology. |
| Resource capability profile | A Products version that states which service operations, target rules, preparation and changes an adapter must support. |
| Service definition | A Products version that qualifies one SKU version for one service type version under one profile version, with unit bounds, bases, target rules and an access policy. It is a catalog record, not a Service entity. |
| Service binding | A Pricing rule on a plan revision. It binds one plan item cell to one service definition with a quantity rule, a target rule and resolved parameters. |
| Service key | The stable name of one service slot of a plan item. A sale selects at most one binding for each service key. |
| Accepted service snapshot | The frozen copy of the selected bindings in a Pricing acceptance receipt. |
| Service binding reference | The posture member that names the accepted bindings of one purchased component. |
| Execution-policy slot | One of the fixed slots `access` (from the service definition) and `retention`, `export` and `disposal` (on the binding). In the first release only `access` carries a policy. |
| Qualification | Adapter-owner evidence that an exact adapter version supports a service type version under a profile version. |

## 2. Actors

### 2.1 Human Actors

#### Catalog administrator

**ID**: `cpt-cf-bss-service-bindings-actor-catalog-owner`

- **Role**: Review service types, profiles and service definitions, and control sale availability.
- **Needs**: Publish reviewed service meanings and qualified SKU offers.

#### Adapter owner

**ID**: `cpt-cf-bss-service-bindings-actor-adapter-owner`

- **Role**: Register service types and parameter schemas, register the resource adapter and supply qualification evidence.
- **Needs**: Describe what the adapter delivers, without access to commercial terms.

#### Commercial plan author

**ID**: `cpt-cf-bss-service-bindings-actor-plan-author`

- **Role**: Bind plan items to qualified services with quantity rules and parameters.
- **Needs**: Understandable service definitions, exact checks and stable published terms.

#### Authorized tenant purchaser

**ID**: `cpt-cf-bss-service-bindings-actor-buyer`

- **Role**: Buy and change a service and understand what it delivers.
- **Needs**: See the accepted service terms and the fulfillment outcome.

#### Platform operator

**ID**: `cpt-cf-bss-service-bindings-actor-operator`

- **Role**: Inspect blocked or incomplete fulfillment and its evidence.
- **Needs**: Findings that name the missing or inconsistent evidence.

### 2.2 System Actors

#### Products, Pricing, Orders, Subscriptions

**ID**: `cpt-cf-bss-service-bindings-actor-commerce`

- **Role**: Keep the catalog, the accepted evidence and the commercial lifecycle.

#### Entitlements and resource adapters

**ID**: `cpt-cf-bss-service-bindings-actor-fulfillment`

- **Role**: Derive service intents from committed posture and deliver them through the selected adapter.

## 3. Operational Concept & Environment

The platform follows the [ToolKit architecture](../../../../docs/toolkit_unified_system/README.md) and the [project guidelines](../../../../guidelines/README.md). This package covers BSS responsibilities and the commercial inputs of the resource adapter contract.

A service type describes what an adapter can do. A service definition qualifies a SKU for it. A service binding makes it part of an offer. A receipt keeps the accepted terms. A posture reference selects them for a purchased component. Each step needs the next owner's authorization and checks.

OSS owns the resources that implement a service. Neither BSS nor the adapter must enumerate them. The adapter maps a service type to its native offering. The [Entitlements service fulfillment contract](../../entitlements/docs/features/05-resource-execution.md) defines the work orders, results and recovery.

## 4. Scope

### 4.1 In Scope

- Service types and parameter schemas registered by adapter owners and reviewed in Products.
- Resource capability profiles and service definitions (SKU qualification) in Products.
- Service bindings on plan revisions, their validation, approval, publication, copy and clone.
- The preview, the expected digest and the freeze in the Pricing acceptance receipt.
- The service binding references in the Subscriptions posture, the refusal of increases until Pricing's change-delta acceptance exists, and the checks that apply once it does.
- The verification and derivation inputs that Entitlements uses.
- First-release operations: provision, withdraw, pause, restore and service-stop.

### 4.2 Out of Scope

The first release excludes:

- Retention, export, holds, disposal and fulfillment outputs. Their slots exist and must be `not_applicable`.
- The `member_scope` target rule. It stays refused until the Subscriptions posture carries the member scope.
- Parameters that the buyer chooses at purchase. The plan revision fixes them; dimension values select variants.
- A binding that spans several plan items.
- Quantity increases and additions of components with bindings, until Pricing's change-delta acceptance exists (Subscriptions SUB-D-61, S-U16). The owner ruled on 2026-11-25 that Subscriptions refuses them before any effect until then (SVB-Q04).
- Quantity decreases, a change to another binding or plan revision, and migration of accepted services.
- Discovery of service types through an adapter plugin.
- Multi-tier resale chains whose legs freeze different bindings. Subscriptions refuses such a chain version at admission.

Entitlements defines assignments, work orders, results, recovery and the Subscriptions fulfillment read. OSS and the adapter design own native offerings, resource selection, execution and their tests. Rating and Billing own charges. A binding carries no price and no payment term.

### 4.3 Future scope

Second-release capabilities need their own accepted terms: retention and export policies, holds, disposal, fulfillment outputs, buyer-chosen parameters, member-scoped services, binding transitions between plan revisions, service type discovery, increases and additions through Pricing's change-delta acceptance, and multi-tier chains with per-leg bindings. They cannot change accepted first-release bindings.

## 5. Functional Requirements

Each entry defines a separately verifiable outcome. Protocol fields, schemas and algorithms belong to DESIGN and the feature specifications.

### 5.1 Service catalog

#### Reviewed service types

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-service-type`

Products MUST publish a service type only from an adapter-registered GTS instance of its service type schema, after review, as an immutable version. Registration alone does not make a service type sellable.

#### Closed parameter schemas

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-parameter-schema`

Each service type MUST name one closed parameter schema. Parameters MUST NOT carry credentials, scripts or native resource identifiers.

#### Resource capability profiles

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-profile`

Products MUST keep resource capability profiles as immutable reviewed versions. A profile states the supported operations, target rules, preparation and changes.

#### SKU qualification for a service

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-sku-qualification`

Products MUST record each SKU version that may deliver a service as an immutable service definition for one service type version and one profile version. The definition states the unit bounds, the quantity bases, the target rules and the access policy.

#### Historical meaning

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-catalog-history`

Published service type, profile and definition versions MUST stay readable with their original content for accepted purchases. A changed registry instance cannot reinterpret them.

#### Sale availability

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-availability`

A service definition MUST stay unavailable for sale until qualification evidence is recorded and availability is enabled. Disabled or withdrawn availability blocks new sales and keeps accepted obligations.

#### Applicable service catalog

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-catalog-read`

Plan authors MUST be able to list the service definitions applicable to a SKU version, with reviewed text, unit meaning, bounds, conditions and availability reasons.

### 5.2 Plan service bindings

#### Revision-owned bindings

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-plan-binding`

A plan revision MUST express each service that a plan item delivers as a service binding for one item cell and one service key. Different plans for the same SKU can bind different parameters and quantity rules.

#### Complete validation

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-binding-validation`

Pricing MUST validate the complete binding set through Products before it saves, submits, approves or accepts it. A scheduled publication relies on the check at approval and the recheck at every acceptance.

#### Complete draft editing

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-binding-edit`

Authorized plan authors MUST be able to read, add, edit and remove bindings as one complete draft change. If any part is invalid, the saved content stays unchanged.

#### Safe concurrent authoring

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-binding-edit-conflict`

Binding edits MUST refuse stale or conflicting changes and recover an identical retry without applying it twice.

#### Binding-aware plan governance

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-binding-governance`

Plan checks, copy, clone, approval and publication MUST include the bindings and keep published terms unchanged. Bindings carry no price.

#### Explicit semantic changes

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-binding-revision`

A change to a binding MUST require a new plan revision and a new acceptance, even when the price does not change.

#### Covered selections

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-binding-coverage`

Acceptance MUST refuse a sale in which a selected item has a service key with neither a binding for the requested dimension value nor a default binding.

### 5.3 Acceptance and evidence

#### Frozen service evidence

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-acceptance`

Acceptance MUST freeze the selected bindings, their pinned catalog evidence, the resolved parameters and the service units at the original quantity in the same receipt and transaction as the prices.

#### Accepted service digest

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-acceptance-digest`

Acceptance MUST compare the buyer's expected service digest with the selected bindings. Unchanged prices cannot hide changed bindings. A caller that does not send the digest cannot buy a plan with bindings.

#### Stable purchase replay

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-acceptance-replay`

A receipt read or replay MUST return the frozen service evidence after any catalog or plan change.

#### Exact service units

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-units`

A `Fixed` binding MUST give its coefficient once. A `PerPlanUnit` binding MUST multiply its coefficient by the committed quantity once. The result MUST be an exact integer within the frozen unit bounds.

#### Resolved parameters

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-parameters-resolved`

Every frozen binding MUST carry complete parameter values, with defaults resolved before acceptance. Unknown parameters and invalid values are refused before any effect.

### 5.4 Posture and fulfillment

#### Explicit posture reference

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-posture-reference`

The commercial posture MUST name the accepted bindings of each purchased component explicitly and completely.

#### No inferred service

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-explicit-selection`

Fulfillment MUST NOT infer a service from a SKU name, a display name, a current default or a live catalog or plan read.

#### Complete input evidence

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-complete-evidence`

Missing, unverifiable or inconsistent binding evidence MUST block new fulfillment and stay visible. It cannot mean a purchase without a service. Restriction and stop of a known service use its retained target.

#### Compatible increase

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-compatible-increase`

A committed quantity increase MUST keep the same binding and service. The profile must support an increase and the units at the resulting quantity must be within the frozen bounds. Subscriptions checks both before commitment. Increases come only through the Pricing change-delta acceptance (Subscriptions S-U16); until it exists, Subscriptions refuses an increase of a component with bindings before any effect.

#### Unsupported changes refused

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-change-refusal`

A quantity decrease of a component with a `PerPlanUnit` binding, a change to another binding and a change of plan or plan revision for a component with bindings MUST be refused before commitment in the first release.

#### Narrow adapter input

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-adapter-boundary`

The adapter MUST receive the service type, the profile, the service units, the parameters, the access policy and the logical scope. It MUST NOT receive SKU, plan, price, payer or subscription identifiers.

### 5.5 Release split

#### Deferred capabilities refused

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-release-split`

Every owner MUST refuse a second-release capability before any effect: a retention, export or disposal slot that is not `not_applicable`, and a profile that requires fulfillment outputs.

#### Member-scoped target deferred

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-fr-target-scope`

Authoring and acceptance MUST refuse the `member_scope` target rule until the posture carries the authorized member scope.

## 6. Non-Functional Requirements

### 6.1 Capability-specific requirements

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-nfr-isolation`

Catalog, plan and receipt reads and writes MUST use the caller's tenant scope only. A foreign catalog record MUST be absent to the caller and MUST NOT disclose its content.

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-nfr-determinism`

Every digest of this package MUST come from one SDK function of its owner. Producers and consumers MUST compute equal bytes, proven by shared test vectors.

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-nfr-retention`

Frozen service evidence and the catalog versions it pins MUST stay readable for as long as an accepted obligation or its recovery can need them.

### 6.2 NFR Exclusions

Availability and latency targets depend on the deployment and the selected adapter. This package sets no vendor performance targets. Project security, audit and runtime baselines still apply.

## 7. Public Library Interfaces

### 7.1 Public API Surface

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-interface-owner-sdks`

**Type:** versioned owner SDKs and their authorized REST interfaces.

**Stability:** proposed extensions.

Products owns the service catalog reads, review and validation. Pricing owns binding authoring, the preview and the acceptance freeze. Subscriptions owns the posture reference. Entitlements owns fulfillment.

### 7.2 External Integration Contracts

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-contract-resource-adapter`

**Direction:** the adapter owner registers service types and its resource adapter; Entitlements sends service intents to the selected adapter.

**Protocol:** GTS registration and the versioned Entitlements plugin SDK.

The adapter advertises exact service type and profile versions. It receives the service inputs of §5.4 and returns service-operation results under the Entitlements contract.

## 8. Use Cases

### 8.1 Buy a managed service

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-usecase-buy`

**Actors:** adapter owner, catalog administrator, plan author, buyer and fulfillment actors.

**Precondition:** the adapter owner registered a managed backup service type and its adapter.

1. Products reviews the service type and a profile, and qualifies the Backup SKU for them with units from 1 to 100.
2. The plan author binds the Backup item with `PerPlanUnit(1)` and a backup frequency parameter.
3. The buyer accepts quantity five. The receipt freezes the binding with five service units.
4. Subscriptions commits the purchase and names the binding in the posture.
5. Entitlements verifies the receipt and sends the service type, profile, five units and parameters to the selected adapter.

**Alternative flows:** a definition that is not enabled refuses the sale. A missing posture reference blocks fulfillment and opens a finding.

### 8.2 Increase the quantity

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-usecase-increase`

**Actor:** buyer.

**Precondition:** an active purchase of five units under a profile that supports increases.

Until Pricing's change-delta acceptance exists, Subscriptions refuses the increase before any effect (SVB-Q04). Once it exists, the buyer adds three units. Subscriptions checks that the delta froze the same service, by execution digest, and that eight units are within the bounds. It commits quantity eight. Entitlements sends eight units for the same service. A profile without increase support refuses the change before commitment.

### 8.3 Change the catalog after a purchase

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-usecase-catalog-change`

**Actors:** adapter owner, catalog administrator, plan author.

**Precondition:** an active purchase of an accepted binding.

The adapter owner changes a parameter default. Products publishes a new service type version and qualifies the SKU in a new definition version. The plan author publishes a new revision that binds it. The existing purchase keeps its frozen binding, parameters and service type version. Only a new acceptance can use the new revision.

## 9. Acceptance Criteria

These business criteria verify §5–6. Their `SVB-C` labels identify checks, not more requirements. The features hold the technical criteria. [DESIGN](DESIGN.md) defines the qualification gates.

| Criterion | Business acceptance outcome |
| --- | --- |
| SVB-C01 Accepted evidence | The service type, profile, parameters, quantity rule and units of a purchase stay as accepted after catalog, plan and default changes. Missing history never becomes an unqualified service. |
| SVB-C02 Explicit selection | A service is delivered only for a binding that the posture names and the receipt froze. A SKU name, a display name or a live read never selects a service. |
| SVB-C03 Exact units | `Fixed(1)` gives one unit at any quantity. `PerPlanUnit(1)` gives five units at quantity five. Units outside the bounds are refused before commitment. |
| SVB-C04 Resolved parameters | A purchase freezes every parameter, defaults included. A later default change does not reach the adapter for that purchase. |
| SVB-C05 Compatible increase | Before the change-delta acceptance exists, an increase is refused before any effect. After it exists, an increase from five to eight units keeps the same service, a profile without increase support refuses it before commitment, and the units are judged at eight. A decrease is refused. |
| SVB-C06 Release split | A binding with a retention, export or disposal policy, or a profile that requires outputs, cannot be published or sold in the first release. |
| SVB-C07 Tenant isolation | Catalog, plan and receipt evidence of one catalog tenant is not visible to another. The service is delivered in the resource tenant only. |
| SVB-C08 Qualified sale | Only an enabled, qualified definition can be sold. Withdrawal blocks new sales and keeps accepted purchases. |

## 10. Dependencies

| Dependency | Required contribution | Criticality |
| --- | --- | --- |
| Products | Service type, profile and definition versions, review, reads, validation and availability | p1 |
| Pricing | Binding authoring, preview, acceptance freeze and receipt reads | p1 |
| Types-registry | Schema and instance registration, content digests and validation | p1 |
| Orders / Subscriptions | Acquisition with the receipt, committed quantity, posture references and admission of changes | p1 |
| Entitlements | Verification, derivation, assignments, work orders and recovery under its feature 05 | p1 |
| Resource adapter | Registered service types, plugin instance, qualification evidence and service execution | p1 |
| Commercial quotas package | The patterns that this package reuses: catalog versions, set validation, acceptance freeze and digests | p1 |
| Pricing change-delta acceptance (Subscriptions S-U16) | Not designed yet. Increases and additions of components with bindings wait for it (SVB-Q04) | p2 |

The Pricing and Products changes of the commercial quotas track are pending in [PR #6105](https://github.com/constructorfabric/gears-rust/pull/6105). This package extends the same contracts and cites their decision rows.

## 11. Assumptions

The first release has one service assignment for each accepted binding of a purchased component. The service is delivered to the resource tenant as a whole. Each adapter design maps service types to its native offerings. Deployments name the profiles and routes that Entitlements may select.

## 12. Risks

| Risk | Impact | Required treatment |
| --- | --- | --- |
| A parameter schema carries a credential or native identifier | Secrets or OSS identities enter commercial records | Review rule and checklist before approval; closed schema subset |
| An adapter advertises a service type without real support | A sale cannot be fulfilled | Qualification evidence before availability; registry metadata never qualifies |
| A plan author binds a service to the wrong SKU | The wrong service is delivered | Products qualifies each SKU version; Pricing checks the definition's SKU against the item |
| Pricing and Entitlements compute different digests | Valid purchases block | One SDK function per digest and shared test vectors |

## 13. Open Questions

[DESIGN §4.5](DESIGN.md#45-open-questions) records the owner's rulings of 2026-11-25: qualification is built once for quota, license and service profiles after both catalogs exist, and every service definition stays disabled until then (SVB-Q01); the plan revision fixes the parameters and dimension values select variants (SVB-Q02); one binding names one item cell (SVB-Q03); an increase of a component with bindings is refused before any effect until Pricing's change-delta acceptance exists (SVB-Q04). No question is open.

## 14. Traceability

[DECOMPOSITION](DECOMPOSITION.md) maps every requirement to its feature. The features define the technical criteria. Business criteria stay in §9.
