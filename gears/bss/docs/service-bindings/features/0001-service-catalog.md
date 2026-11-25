---
description: "Review adapter-registered service types, keep resource capability profiles, qualify SKU versions in service definitions and validate binding sets for Pricing."
---

# Feature: Service catalog


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Service catalog onboarding](#21-service-catalog-onboarding)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Proposal checks](#31-proposal-checks)
  - [3.2 Service type review](#32-service-type-review)
  - [3.3 Profile and definition rules](#33-profile-and-definition-rules)
  - [3.4 Reads and validation](#34-reads-and-validation)
- [4. States (CDSL)](#4-states-cdsl)
  - [4.1 Catalog proposal state](#41-catalog-proposal-state)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Owner-contract conformance](#51-owner-contract-conformance)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-featstatus-service-catalog-implemented`
- [ ] `p1` - `cpt-cf-bss-service-bindings-feature-service-catalog`

## 1. Feature Context

### 1.1 Overview

Turn adapter-registered service types into reviewed, versioned catalog records that a plan author can select, and judge binding sets for Pricing. Owners: Products and catalog administrators. Adapter owners register the instances.

### 1.2 Purpose

This feature covers the two GTS schemas, the proposal and review path, immutable service type, profile and definition versions, availability slots, catalog reads and `validate_service_bindings`. Qualification evidence and enabling sale are [feature 5](0005-qualification.md). Prices, bindings and purchases are out of scope.

**Requirements**:

- `cpt-cf-bss-service-bindings-fr-service-type`
- `cpt-cf-bss-service-bindings-fr-parameter-schema`
- `cpt-cf-bss-service-bindings-fr-profile`
- `cpt-cf-bss-service-bindings-fr-sku-qualification`
- `cpt-cf-bss-service-bindings-fr-catalog-history`
- `cpt-cf-bss-service-bindings-fr-catalog-read`
- `cpt-cf-bss-service-bindings-nfr-isolation`
- `cpt-cf-bss-service-bindings-nfr-retention`
- `cpt-cf-bss-service-bindings-interface-owner-sdks`
- `cpt-cf-bss-service-bindings-contract-resource-adapter`
- `cpt-cf-bss-service-bindings-usecase-buy`
- `cpt-cf-bss-service-bindings-usecase-catalog-change`

**Principles**: `cpt-cf-bss-service-bindings-principle-single-authority`.

### 1.3 Actors

| Actor | Role in feature |
| --- | --- |
| `cpt-cf-bss-service-bindings-actor-adapter-owner` | Register service type instances and parameter schemas |
| `cpt-cf-bss-service-bindings-actor-catalog-owner` | Propose, submit and decide catalog versions |
| `cpt-cf-bss-service-bindings-actor-plan-author` | Read applicable service definitions |
| `cpt-cf-bss-service-bindings-actor-commerce` | Pricing calls the binding set validation |

### 1.4 References

- **PRD**: [Product requirements](../PRD.md), §5.1.
- **Design**: [Architecture and contracts](../DESIGN.md), §3.3.1 and §3.3.2.
- **Decomposition**: [Feature allocation](../DECOMPOSITION.md).
- **Dependencies**: None. [Feature 5](0005-qualification.md) enables sale.
- **Patterns**: the quota catalog (products P-D-267 to P-D-276, pending in PR #6105).

## 2. Actor Flows (CDSL)

### 2.1 Service catalog onboarding

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-flow-service-catalog`

**Actor**: `cpt-cf-bss-service-bindings-actor-catalog-owner`.

**Use cases**: `cpt-cf-bss-service-bindings-usecase-buy`, `cpt-cf-bss-service-bindings-usecase-catalog-change`.

**Success Scenarios**: An approved proposal publishes the next immutable version; a read answers exact content with its availability.

**Error Scenarios**: Invalid, unauthorized or conflicting input is refused without effects. A registry outage leaves the proposal unchanged and retryable.

**Steps**:

1. [ ] - `p1` - The adapter owner registers a service type instance and its final parameter schema under the Products schemas. - `inst-register`
2. [ ] - `p1` - A catalog administrator stages a service type proposal that names the instance, then submits it. - `inst-propose-type`
3. [ ] - `p1` - Products resolves the instance and the schema as the caller, checks them and pins their content and registry digests. - `inst-pin`
4. [ ] - `p1` - Another administrator reviews the meaning, the unit and every parameter, then approves or rejects. - `inst-review-type`
5. [ ] - `p1` - Administrators propose and approve a resource capability profile version the same way. - `inst-profile`
6. [ ] - `p1` - Administrators propose a service definition for a SKU version, a service type version and a profile version, with bounds, bases, target rules, access policy and texts, and approve it. - `inst-definition`
7. [ ] - `p1` - The approval writes availability revision 1, `disabled`, `qualification_pending`. - `inst-availability`
8. [ ] - `p1` - A plan author lists the definitions of an item's SKU version with texts, bounds and availability reasons. - `inst-browse`

## 3. Processes / Business Logic (CDSL)

### 3.1 Proposal checks

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-algo-service-catalog`

**Input**: An authorized proposal and the evidence defined in DESIGN §3.3.1 and §3.3.2.

**Output**: A decided proposal and a published version, or a classified refusal.

**Steps**:

1. [ ] - `p1` - Authorize the caller for the catalog tenant before reading the body - `inst-authorize`
2. [ ] - `p1` - Look up the idempotency key and replay a recorded answer before any other check - `inst-replay`
3. [ ] - `p1` - Check the content against its kind: schemas and subset for a service type; closed members for a profile; references, bounds and target rules for a definition - `inst-check`
4. [ ] - `p1` - **IF** the approved content equals the latest version of the same record - `inst-unchanged`
   1. [ ] - `p1` - **RETURN** that version; publish no new one, as a quota definition reuses its version - `inst-unchanged-result`
5. [ ] - `p1` - **IF** the approver is the submitter or the author - `inst-sod`
   1. [ ] - `p1` - **RETURN** `SOD_VIOLATION` without writes - `inst-sod-result`
6. [ ] - `p1` - Inside one transaction, recheck the proposal version, publish the next version with its digest, write the availability slot for a definition and the audit row - `inst-commit`
7. [ ] - `p1` - **RETURN** the decided proposal and the published version - `inst-result`

### 3.2 Service type review

The submit resolves the instance and its parameter schema in types-registry as the caller. It refuses an instance of another schema (`SERVICE_REFERENCE_FAMILY`), an unregistered reference (`SERVICE_REFERENCE_UNKNOWN`), a minor-bearing ID (`SERVICE_VERSION_UNSUPPORTED`) and a parameter schema outside the subset of DESIGN §3.3.1 (`SERVICE_SCHEMA_UNSUPPORTED`, naming the property and the rule). It pins the instance content, the schema content and both registry digests in the proposal. Approval reads only the pinned content and publishes it. A registry change of either entity after the submit leaves the proposal as it is; a new proposal judges the new content. An outage is 503 `SERVICE_REGISTRY_UNAVAILABLE`.

The reviewer checks what a machine cannot. The checklist:

- The display name, description and unit meaning are understandable to a plan author and a buyer, in English at least.
- No parameter carries a credential, a secret, a script, a template body, a URL with authority data, a native resource identifier or an infrastructure topology.
- Each parameter's values have a stable meaning that the adapter design documents.
- The supported target rules match what the adapter can deliver.

A rejection needs a note.

### 3.3 Profile and definition rules

A profile proposal carries every member of DESIGN §3.3.2.1. Every operation flag is explicit. `capability` is `resource` and `contract_major` is `1`.

A definition proposal names a SKU and SKU version that the tenant holds (else `SERVICE_SKU_VERSION_UNKNOWN`), and exact service type and profile versions of the tenant (else 404 on the reference). It may name an existing definition to version; that definition must have the same SKU (else `SERVICE_RECORD_INVALID` on `definition_id`). The approval refuses:

- bounds that are not canonical decimal integers, `step` of 0, `max` below `min` or an absent `max` (`SERVICE_UNITS_INVALID`);
- an empty or unknown base (`SERVICE_RECORD_INVALID` on `supported_bases`);
- target rules outside both the service type's and the profile's (`SERVICE_TARGET_UNSUPPORTED`);
- an access policy other than `{on_restricted: pause, on_revoked: service_stop}` (`SERVICE_RECORD_INVALID` on `access_policy`);
- a profile that the first release cannot sell: without `provision`, `pause`, `restore`, `service_stop`, `pause_preserves_data` or `change_support.initial_set`; with `preparation` but without `withdraw`; or with nonempty `output_keys` (`SERVICE_PROFILE_UNSUPPORTED`).

A definition applies to its SKU version and every later version of the SKU. A new SKU version that changes what the service delivers needs a new definition version, proposed by the administrator.

### 3.4 Reads and validation

Reads follow DESIGN §3.3.2.3. They authorize every page, filter by the caller's tenant and answer exact stored content and digests; they never compute a digest again. A cursor binds the tenant, the filters and the order; another use is 400. A dependency failure is an error, never an empty list.

`validate_service_bindings` follows DESIGN §3.3.2.4. It resolves definitions under the whole `read` grant with its row constraints, which name service definitions; a definition the grant does not reach is unknown to the set. Service types and resource profiles are read under the grant's tenant-only scope. It normalizes parameters: it validates each value against the pinned schema with the registry's draft-07 validator, adds every absent property's default and answers the complete object in property-name order. It computes units with the same arithmetic as `pricing-sdk`'s `service_units`. It is pure after its reads.

## 4. States (CDSL)

### 4.1 Catalog proposal state

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-state-proposal`

| From | Event / to | Guard |
| --- | --- | --- |
| none | create / `staged` | Authorized `author`; kind and content readable |
| `staged` | submit / `submitted` | Authorized `submit`; content checks pass; evidence pinned |
| `submitted` | approve / `approved` | Authorized `approve`; not the submitter or the author; pinned evidence unchanged |
| `submitted` | reject / `rejected` | Authorized `approve`; not the submitter or the author; a note |

`approved` and `rejected` are final. Published versions have no state. Availability is a separate append-only observation of a definition version.

## 5. Definitions of Done

### 5.1 Owner-contract conformance

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-dod-service-catalog`

Owners implement this behavior and keep passing evidence for every criterion below. Under `DOCS-ONLY` traceability, checkboxes track implementation; they do not claim that tests have run.

**Implements**: `cpt-cf-bss-service-bindings-flow-service-catalog`, `cpt-cf-bss-service-bindings-algo-service-catalog`, `cpt-cf-bss-service-bindings-state-proposal`.

**Constraints**: `cpt-cf-bss-service-bindings-constraint-first-release`, `cpt-cf-bss-service-bindings-constraint-platform`.

**Touches**: `ProductsServiceCatalogClientV1` and the `/bss-products/v1` service endpoints (DESIGN §3.3.1–3.3.2). State follows `cpt-cf-bss-service-bindings-db-owner-state` and `cpt-cf-bss-service-bindings-entity-evidence-chain`.

Products adds one migration for the versions, availability, proposals and qualification slots; storage refuses updates of versions and decided proposals. The digest functions and the canonical rendering live in `products-sdk` with test vectors. Unit tests cover the subset rules, the checks and the arithmetic; integration tests cover both database backends, permissions, idempotency and the registry.

## 6. Acceptance Criteria

These criteria have stable verification IDs. Business criteria stay in [PRD §9](../PRD.md#9-acceptance-criteria).

| Contract criterion | Relevant condition | Required guarantee |
| --- | --- | --- |
| SVB-C10 Service type review | A registered instance; one of another schema; a parameter schema with a number, a nested object or a `$ref`; a property without a default that is not required; a registry change after submit; the submitter approving | Only a schema-valid instance with a subset-valid parameter schema reaches review. Each refusal names its rule. The base schema accepts any object; the example parameter schema accepts the example parameters and refuses an unknown member. Approval publishes the pinned content, whatever the registry holds later. The submitter and the author cannot decide. Approval publishes one immutable version with both registry digests. |
| SVB-C11 Profiles and definitions | A profile without pause, with outputs, or with preparation but no withdraw; bounds without `max`; target rules outside the intersection; a definition for an unknown SKU version | Each is refused with its code. An approved definition starts `disabled`, `qualification_pending`, and applies to its SKU version and later ones. |
| SVB-C12 History and versions | A registry change after approval; a proposal equal to the latest version; a read of a withdrawn definition | Approved versions never change. An unchanged proposal answers the latest version and publishes none. Historical versions stay readable with their digests. |
| SVB-C13 Catalog reads | Another tenant's records; a cursor reused with other filters; a locale without a translation; a dependency outage | Foreign records are absent. The cursor is refused. English is served. An outage is an error, not an empty catalog. |
| SVB-C14 Binding set validation | Each failure of DESIGN §3.3.2.4; parameters with absent defaults; a quantity off the grid; a malformed request | Every check is answered in order without dropping a binding. Normalized parameters are complete. Units are exact. Only a malformed request is 400. |
