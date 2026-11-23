---
description: "Turn authorized provider evidence into reviewed, versioned definitions that a plan author can select."
---

# Feature: Quota catalog onboarding


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Quota catalog onboarding](#21-quota-catalog-onboarding)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Quota catalog onboarding checks](#31-quota-catalog-onboarding-checks)
  - [3.2 Catalog intake and publication](#32-catalog-intake-and-publication)
  - [3.3 Catalog picker behavior](#33-catalog-picker-behavior)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Owner-contract conformance](#51-owner-contract-conformance)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-featstatus-catalog-implemented`
- [ ] `p1` - `cpt-cf-bss-quotas-feature-catalog`

## 1. Feature Context

### 1.1 Overview

Turn authorized provider evidence into reviewed, versioned definitions that a plan author can select. Owners: Products and catalog administrators.

### 1.2 Purpose

This feature covers discovery, registered semantics, candidate approval, immutable history and scoped catalog reads. Prices, purchased rights, live customer quotas and physical stock are out of scope.

**Requirements**:

- `cpt-cf-bss-quotas-fr-catalog`
- `cpt-cf-bss-quotas-fr-catalog-gts`
- `cpt-cf-bss-quotas-fr-catalog-presentation`
- `cpt-cf-bss-quotas-fr-catalog-approval`
- `cpt-cf-bss-quotas-fr-catalog-classification`
- `cpt-cf-bss-quotas-fr-catalog-history`
- `cpt-cf-bss-quotas-fr-catalog-policies`
- `cpt-cf-bss-quotas-fr-catalog-compatibility`
- `cpt-cf-bss-quotas-fr-plan-catalog`
- `cpt-cf-bss-quotas-nfr-isolation`
- `cpt-cf-bss-quotas-interface-management`
- `cpt-cf-bss-quotas-contract-provider`
- `cpt-cf-bss-quotas-usecase-buy`

**Principles**: `cpt-cf-bss-quotas-principle-single-authority`.

### 1.3 Actors

| Actor | Role in feature |
| --- | --- |
| `cpt-cf-bss-quotas-actor-catalog-owner` | Start or qualify the operation within the authorized scope |
| `cpt-cf-bss-quotas-actor-commerce` | Provide authoritative results and evidence for recovery |

### 1.4 References

- **PRD**: [Product requirements](../PRD.md).
- **Design**: [Architecture and contracts](../DESIGN.md), §3.3.1, 3.3.1.1 and 3.3.1.3.
- **Decomposition**: [Feature allocation](../DECOMPOSITION.md).
- **Dependencies**: None. Qualification evidence is required before enabling sales.
- **Pending owner contracts**: [Entitlements #5744](https://github.com/constructorfabric/gears-rust/pull/5744). All `ENT-D-*` and `E-U*` references depend on this pending work, as described in DESIGN §1.2.

## 2. Actor Flows (CDSL)

### 2.1 Quota catalog onboarding

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-flow-catalog`

**Actor**: `cpt-cf-bss-quotas-actor-catalog-owner`.

**Use cases**: `cpt-cf-bss-quotas-usecase-buy`.

**Success Scenarios**: An authorized, supported request returns the matching result defined by the owner contract.

**Error Scenarios**: Reject invalid or unauthorized input without effects. Recover uncertain external outcomes using their original identity.

**Steps**:

1. [x] - `p1` - Read one authorized adapter target through the discovery contract. - `inst-discover`
2. [x] - `p1` - Verify one complete snapshot and resolve every registered definition and dependency. - `inst-complete`
3. [x] - `p1` - Save a candidate with its original snapshot and publisher provenance. - `inst-candidate`
4. [x] - `p1` - Review resource meaning, units, counted capacity and scope before approving a definition version. - `inst-review`
5. [ ] - `p1` - Attach matching qualification evidence before enabling the definition for sale. - `inst-qualify`
6. [x] - `p1` - Return applicable approved definitions to an authorized plan author with descriptions and availability reasons. - `inst-browse`

## 3. Processes / Business Logic (CDSL)

### 3.1 Quota catalog onboarding checks

- [x] `p1` - **ID**: `cpt-cf-bss-quotas-algo-catalog`

**Input**: Authorized owner request and the pinned evidence defined in DESIGN §3.3.1, 3.3.1.1 and 3.3.1.3.

**Output**: A result tied to the request, or an explicit refusal or unresolved outcome.

**Steps**:

1. [x] - `p1` - Authorize the catalog and adapter target before resolving input. - `inst-authorize`
2. [x] - `p1` - Check actual registry ancestry, exact units, supported major versions and pinned historical content. - `inst-identity`
3. [x] - `p1` - Recover the existing candidate for the same source revision and content. Reject conflicting content. - `inst-replay`
4. [x] - `p1` - Leave intake unresolved when pages or dependencies are missing. Do not infer capability removal. - `inst-incomplete`
5. [x] - `p1` - **RETURN** the reviewed definition or a classified validation or dependency failure. - `inst-result`

### 3.2 Catalog intake and publication

Products reviews display text with each metric definition. It explains the limit, shared scope and what counts toward it, including stopped resources where relevant. The form shows the selected profile's reviewed conditions. Text changes require a new immutable catalog-definition version and cannot change accepted semantics.

The accounting policy describes the purchased service. Its localized description requires an English fallback. It does not define OSS resource states, release messages or counter algorithms. Products does not receive resource allocation or release events.

The plan form resolves `metric_ref` to a title and description. It gets the canonical unit symbol from `canonical_unit_ref`. The selected adapter capability supplies the supported scope, operations and numeric bounds. Convert bounds from the adapter's `unit_ref` exactly before validating them in the plan's unit.

The author enters the commercial amount and rule basis. The form must not use a technical maximum as the purchased amount. Missing required display text blocks publication for plan authoring.

Products registers its base schemas and discovery plugin specification in types-registry. Authorized metric/unit owners register immutable named definitions. Adapter owners register approved derived schemas and plugin instances. The plugin specification derives from ToolKit's exported `PluginV1` identifier. Products resolves configured adapter instances through its public service and scoped ClientHub. Consumers do not depend on plugin implementations.

Generate SDK schemas with `#[gts_type_schema]` and `gts_schema_with_refs_as_string()`. JSON Schema `$id` and `$ref` use `gts://` URIs. References declare `x-gts-instance-ref` or `x-gts-type-ref` narrowing. Intake must also resolve every referenced record and check its actual registry ancestry. Syntactic narrowing alone is not enough. GTS provides identity and validation, not transport or publication permission.

Products authenticates the call context. It parses, resolves and validates every record and reference, including registry ancestry and unit semantics. It authorizes the publisher, target and catalog scope before saving a candidate. A GTS vendor prefix or subject type does not prove tenant authority.

Unknown types, wrong reference families, unsupported major versions, invalid conversions and conflicting snapshots cannot become sellable entries. Registry outages leave intake retryable. They never permit a fallback that skips validation.

Classify errors as invalid reference/schema/unit, unsupported version/capability, source conflict, forbidden scope or unavailable dependency. HTTP responses use safe RFC 9457 error details.

Products stores each candidate under its authorized catalog and adapter target. The same binding, revision and content recover the same candidate. Different content at that revision conflicts. Retain the complete source snapshot and resolved schema/instance digests.

Indexed GTS references use the platform's deterministic UUID representation. Extension `properties` use JSONB. Resolve reference ancestry through types-registry, not SQL string matching. Do not create a second registry of GTS definitions.

Product engineering reviews the candidate and publishes immutable `MetricSemanticsVersion` and `EnforcementMappingVersion` records. Only the approved catalog version can enter plan authoring. Manual authoring uses the same registered identities, validation and approval path, with recorded author provenance. It cannot introduce unregistered units or skip adapter qualification.

A later complete discovery run may report drift or retirement for review. It never rewrites published terms, automatically unpublishes an accepted purchase or changes live tenant limits.

Classify provider values as countable resource limits, feature grants, financial controls, or operational/retention policies. Only qualified numerical resource mappings enter this quota profile. Provider defaults and configured scope caps are technical evidence, not prices or available stock. Do not import provider customer balances as purchased rights.

### 3.3 Catalog picker behavior

The list includes every field needed by the picker, without a registry call for each row. The author selects an exact definition version and allowed unit, enters an amount and basis, and selects the plan item and logical binding. The editor must show profile conditions and any disabled reason. Pricing resolves and validates the references itself. UI filtering does not replace authorization or publication checks.

## 4. States (CDSL)

This feature uses the owner states defined in DESIGN. It adds no lifecycle state machine. Only the owning contracts can change availability, draft/accepted status or commercial posture.

## 5. Definitions of Done

### 5.1 Owner-contract conformance

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-dod-catalog`

Owners must implement this behavior and retain passing evidence for every criterion below. Under `DOCS-ONLY` traceability, FEATURE checkboxes and evidence references track implementation. They do not claim that code markers exist or tests have run.

Implementation evidence: Products feature [Quota Catalog](../../../products/docs/features/quota-catalog.md) (P-D-266 … P-D-274).

**Implements**: `cpt-cf-bss-quotas-flow-catalog`, `cpt-cf-bss-quotas-algo-catalog`.

**Constraints**: `cpt-cf-bss-quotas-constraint-qualified-profiles`.

**Touches**: ProductsQuotaDiscoveryPluginClientV1 and ProductsQuotaCatalogClientV1 (DESIGN §3.3.1–3.3.1.1). State retention follows `cpt-cf-bss-quotas-db-owner-state` and the evidence entities in `cpt-cf-bss-quotas-entity-evidence-chain`.

Follow the security, error, storage, task-runtime and API rules in DESIGN §2.2 and §3.3.7 without exceptions. Check scope on reads, writes, replay and recovery. Test concurrent calls, foreign scopes, lost responses and restart wherever the flow has external effects.

Unit tests cover pure validation and derivation. Contract and integration tests cover persistence, permissions and actual adapter effects. This feature specifies no new compile-time flag.

Rollback must preserve accepted history, durable identities and unknown effects. Disabling new sales does not erase existing obligations. Follow DESIGN §3.8 and Feature 4 for observability and recovery. Owners supply executable tests and deployment changes.

## 6. Acceptance Criteria

These criteria have stable verification IDs. Implementation is complete only when their required outcomes are verified. This document records no test results. Business criteria remain in [PRD §9](../PRD.md#9-acceptance-criteria).

| Contract criterion | Relevant condition | Required guarantee |
| --- | --- | --- |
| Q-C37 GTS catalog intake | Typed discovery intake, exact replay, changed same-revision content, wrong-family/unknown references, foreign scope, manual authoring, locale fallback and missing display text | Products stages only authorized, schema-validated candidates with pinned metric/unit references. Replay returns the same candidate. Reject conflicting revisions, wrong scopes and invalid references. Review and qualification must precede plan publication. Neither discovery nor manual input creates a purchase or changes tenant state. Both use the same gates. The form shows reviewed title/description with English fallback, the unit and selected profile conditions. Missing required text blocks publication. Convert numeric bounds exactly. They never set the purchased quantity. |
| Q-C38 Units, versions and discovery continuity | Allocation/feature/monetary/retention classification, GB/GiB conversion, unsupported step, zero denominator, same-ID semantic drift, mixed pages, registry outage and retirement | Only qualified allocation records enter this profile. Use exact conversion with registered immutable units. Reject invalid arithmetic, versions and conflicts. Incomplete snapshots or registry outages cannot permit publication or imply removal. Review new definitions. Keep accepted references and limits unchanged. |
| Q-C57 Catalog picker | Authorized SKU/version listing, pagination across changed filters/scope, exact historical reads after withdrawal and catalog/registry outage | Show only matching approved definitions. Include reasons for disabled definitions and exclude them from the default picker. Historical reads return exact content. Reject cursor reuse with changed scope or filters. Unavailable dependencies return errors, not an empty successful list. Supply labels, units, exact bounds and profile conditions without per-row registry calls. |
| Q-C67 Registry evolution | Compatible schema addition, minor-bearing identity after major-only registration, and a breaking semantic version | Compatible changes use validated content revisions under major-only identities. Reject minor-bearing versions within that major. Breaking changes require a new major and approved definition. Accepted history retains resolved content, revisions and dependency digests. Reading the current content of a major-only identity cannot change an old purchase. |
