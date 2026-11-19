---
description: "Registry-owned RG type publication, caller-scoped snapshots and admission verification."
---

<!-- Created: 2026-04-07 by Constructor Tech -->
<!-- Updated: 2026-11-12 by Constructor Tech -->

# Feature: Registry-Owned Resource Group Types


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [RG Type Identifier Validation](#rg-type-identifier-validation)
  - [Registry Snapshot Consumption](#registry-snapshot-consumption)
  - [Schema-Validated Group and Membership Types](#schema-validated-group-and-membership-types)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Managed Owner Publication](#managed-owner-publication)
  - [Caller-Scoped Registry Catalog](#caller-scoped-registry-catalog)
  - [Consumer-Owned Admission Coverage](#consumer-owned-admission-coverage)
  - [Error Conversion Chain Tests](#error-conversion-chain-tests)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Verification](#7-verification)

<!-- /toc -->

- [x] `p1` - **ID**: `cpt-cf-resource-group-featstatus-type-management`

- [x] `p1` - `cpt-cf-resource-group-feature-type-management`
## 1. Feature Context

### 1.1 Overview

Resource Group consumes GTS type definitions owned by Types Registry. RG publishes
its base schemas through managed GTS publication; each consuming gear publishes
its own derived types. Group and membership rows reference their type by a
key into RG's local identity dictionary (`gts_type`), which maps keys to GTS
identifiers and holds no definitions; a dictionary row never admits a type.

The former local type CRUD routes and SDK methods, type bootstrap interface and
configuration-driven type/group/membership seed services are removed. These
retired operations are not part of the current implementation contract. Their
replacement is owner publication followed by ordinary group and membership APIs.

### 1.2 Purpose

Keep schema admission, inheritance and type lifecycle in one registry while RG
retains responsibility for graph placement and tenant ownership. Type snapshot
reads use the caller context and occur before RG database transactions.

**Requirements**: `cpt-cf-resource-group-fr-manage-types`, `cpt-cf-resource-group-fr-validate-type-code`, `cpt-cf-resource-group-fr-reject-duplicate-type`, `cpt-cf-resource-group-fr-seed-types`, `cpt-cf-resource-group-fr-validate-type-update-hierarchy`, `cpt-cf-resource-group-fr-delete-type-only-if-empty`

**Principles**: `cpt-cf-resource-group-principle-dynamic-types`

**Constraints**: `cpt-cf-resource-group-constraint-gts-identifiers`

### 1.3 Actors

| Actor | Responsibility |
| --- | --- |
| `cpt-cf-resource-group-actor-apps` | Publish consumer-owned declarations and use admitted types for group and membership operations |
| `cpt-cf-resource-group-actor-instance-administrator` | Publish deployment-owned declarations through the registry platform API and export local custom definitions before upgrading |

### 1.4 References

- [PRD](../PRD.md)
- [Design](../DESIGN.md)
- [Decomposition](../DECOMPOSITION.md)
- [SDK foundation](0001-sdk-gear-foundation.md)
- [Entity and hierarchy operations](0003-entity-hierarchy.md)
- [Membership operations](0004-membership.md)

## 2. Actor Flows (CDSL)

Managed publication is orchestrated by the host runtime and Types Registry. RG
contributes its schema collector through `gts_publication` and performs no local
create/update/delete type operation. The registry's platform API is the deployment
interface for authored definitions. Registry admission, version preconditions and
receipts follow the Types Registry contract.

Group creation and membership addition remain RG actor flows in features 0003
and 0004. RG reads the admitted schema definitions from the registry and applies the
contracts below.

## 3. Processes / Business Logic (CDSL)

### RG Type Identifier Validation

- [x] `p1` - **ID**: `cpt-cf-resource-group-algo-type-mgmt-validate-type-input`

**Input**: A group type identifier, normalized by trimming and lowercasing.

**Output**: Success or a validation error before lookup or persistence.

**Steps**:
1. [x] - `p1` - Reject an empty identifier - `inst-val-input-1`
2. [x] - `p1` - Check the identifier begins with the RG type family prefix - `inst-val-input-2`
   1. [x] - `p1` - Return a validation error if the prefix differs - `inst-val-input-2a`
3. [x] - `p1` - Reject identifiers longer than 1024 characters - `inst-val-input-3`

Full GTS shape validation belongs to the SDK value object. Definition admission,
parent/member references, inheritance and schema validation belong to
Types Registry; RG does not duplicate them in this identifier check.

### Registry Snapshot Consumption

`RegistryTypeCatalog` implements `RgTypeCatalog` using `TypesRegistryApi` with the
caller's SecurityContext. Group type reads request authored content and the
resolved schema. `RgType` compiles the group validator and extracts the
membership binding. New links load the bound membership schema and verify that
the resource type is active. Registry reads run before the RG write transaction.

A new group or membership requires active definitions. Existing group updates
that need type validation may use the exact retained definition of a deleted
type. Row-only reads, rename/metadata-clear operations with an unchanged parent,
and cleanup do not look up type definitions. Registry caller refusals and
availability failures retain their intended error classifications.

### Schema-Validated Group and Membership Types

The SDK publishes the abstract group base `gts.cf.core.rg.type.v1~` with required
`parent_type` and `membership_type`, and the abstract membership base
`gts.cf.core.rg.membership.v1~`.
The base `parent_type` accepts `null` or a string matching the fixed
`x-gts-type-ref` for the RG type family.
A concrete group schema constrains the actual `parent_type` and declares an unconditional
`properties.membership_type.const` naming a concrete membership schema.
Publish both concrete definitions through Types Registry before creating
groups that use them.

RG reads the effective group schema and the active membership definition before
creation. The group write transaction validates the actual parent's type
(or `null` at the root), the fixed binding and metadata, and stores the
binding as a key in `gts_type`. A group update that needs type validation
uses the same binding and checks the resulting payload in the write
transaction. The parent group still has to pass RG's tenant and hierarchy checks.

A membership addition verifies the group type, the bound membership schema and
the resource type are active. The membership schema validates only
`resource_type`. RG validates that `resource_id` is nonempty and checks the
group type and binding again before inserting the link. For rows created before
the binding column existed, RG uses the binding from the published group schema.

Types Registry checks same-ID revisions against the previous schema. The
placement and membership schemas can widen through ordinary publication,
while incompatible revisions are refused. The admission tests verify
widening an inherited placement `anyOf` with GTS 0.17.2. Indirect changes
from mutable references remain subject to the existing dependent refresh
behavior.

## 4. States (CDSL)

RG defines no separate lifecycle for type definitions. Types Registry owns active
and deleted states and retains definition content. Registry lifecycle changes do
not scan or rewrite existing RG group, membership or hierarchy rows.

## 5. Definitions of Done

### Managed Owner Publication

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-type-mgmt-managed-publication`

RG declares managed publication of its SDK collector. The host completes
publication before application initialization; consuming gears publish their own
RG-derived types. RG initialization constructs adapters and services without
performing registration or waiting on remote registry admission itself.

**Touches**: `resource-group/src/gear.rs`, `resource-group-sdk/src/gts.rs`.

### Caller-Scoped Registry Catalog

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-type-mgmt-registry-catalog`

Type lookups forward the caller context, select the artifacts required by the
operation and distinguish absent/deleted definitions, access refusal,
unavailability and malformed registry responses. Placement and payload validation
use the materialized snapshot. Groups and memberships store GTS identifiers.

**Touches**: `resource-group/src/infra/types_registry.rs`, `resource-group/src/domain/type_catalog.rs`.

### Consumer-Owned Admission Coverage

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-testing-managed-types`

Rust tests use the registry SDK mock to check inherited payload validation,
placement constraints and membership bindings. E2E tests publish schemas to
Types Registry, check that it refuses incompatible revisions and exercise RG
writes against the accepted revisions. Adapter tests cover caller context,
requested projections, lifecycle policies and error classification.

**Touches**: `managed_types_test.rs`, `registry_types_test.rs`, `infra/types_registry_tests.rs`, `testing/e2e/suites/resource_group/test_type_revisions.py`.

### Error Conversion Chain Tests

- [x] `p2` - **ID**: `cpt-cf-resource-group-dod-testing-error-conversions`

Domain tests cover the conversion of enforcer and database failures into RG
errors. REST error mapping preserves caller-facing classifications and keeps
internal database details out of response bodies.

**Touches**: `domain_unit_test.rs`, `resource-group/src/domain/error.rs`.

## 6. Acceptance Criteria

- [x] The RG base schema is contributed through managed GTS publication.
- [x] Consumers can publish derived RG types through Types Registry admission.
- [x] New group/membership writes use active group and membership schemas.
- [x] Group payload validation uses the unmodified materialized schema through the registry SDK.
- [x] Type reads forward the caller context and precede database write transactions.
- [x] Cleanup and ordinary reads resolve stored type keys through the identity dictionary, without registry lookups and without creating keys.
- [x] Local type CRUD and bootstrap interfaces are absent from SDK, REST routes and OpenAPI.
- [x] Database migration preserves dictionary keys and identifiers, groups, memberships and hierarchy while removing the local definitions.
- [x] Repeating an identical E2E declaration uses the registry's current version and leaves its resource version unchanged.

## 7. Verification

- `managed_types_test.rs`: inherited schemas and stored membership bindings with the registry SDK mock.
- `registry_types_test.rs`: registry definition policies exercised by group and membership services.
- `infra/types_registry_tests.rs`: context/projection forwarding and error classification.
- SQLite/PostgreSQL type-identifier migration tests: preserved rows, indexes, constraints and rollback on refusal.
- `testing/e2e/suites/resource_group`: publication followed by group/membership HTTP calls, including repeat publication, compatible revisions applied to existing groups, and rejection of incompatible revisions.
