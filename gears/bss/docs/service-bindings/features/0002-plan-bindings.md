---
description: "Bind plan items to qualified services on plan revisions and freeze the selected bindings with the accepted prices."
---

# Feature: Plan service bindings and acceptance


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Bind, publish and accept](#21-bind-publish-and-accept)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Binding set replacement](#31-binding-set-replacement)
  - [3.2 Acceptance freeze](#32-acceptance-freeze)
  - [3.3 Selection and examples](#33-selection-and-examples)
  - [3.4 Governance and history](#34-governance-and-history)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Owner-contract conformance](#51-owner-contract-conformance)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-featstatus-plan-bindings-implemented`
- [ ] `p1` - `cpt-cf-bss-service-bindings-feature-plan-bindings`

## 1. Feature Context

### 1.1 Overview

Attach service bindings to plan revisions and freeze the selected bindings with the accepted charging terms. Owners: Pricing and plan authors.

### 1.2 Purpose

This feature covers draft replacement, checks, approval, publication, copy and clone, the preview, the expected digest, the acceptance freeze, receipt reads and the digest functions that other owners share. Service meaning and qualification are Products'. The posture and delivery are features 3 and 4.

**Requirements**:

- `cpt-cf-bss-service-bindings-fr-plan-binding`
- `cpt-cf-bss-service-bindings-fr-binding-validation`
- `cpt-cf-bss-service-bindings-fr-binding-edit`
- `cpt-cf-bss-service-bindings-fr-binding-edit-conflict`
- `cpt-cf-bss-service-bindings-fr-binding-governance`
- `cpt-cf-bss-service-bindings-fr-binding-revision`
- `cpt-cf-bss-service-bindings-fr-binding-coverage`
- `cpt-cf-bss-service-bindings-fr-acceptance`
- `cpt-cf-bss-service-bindings-fr-acceptance-digest`
- `cpt-cf-bss-service-bindings-fr-acceptance-replay`
- `cpt-cf-bss-service-bindings-fr-units`
- `cpt-cf-bss-service-bindings-fr-parameters-resolved`
- `cpt-cf-bss-service-bindings-fr-release-split`
- `cpt-cf-bss-service-bindings-fr-target-scope`
- `cpt-cf-bss-service-bindings-nfr-isolation`
- `cpt-cf-bss-service-bindings-nfr-determinism`
- `cpt-cf-bss-service-bindings-nfr-retention`
- `cpt-cf-bss-service-bindings-interface-owner-sdks`
- `cpt-cf-bss-service-bindings-usecase-buy`
- `cpt-cf-bss-service-bindings-usecase-catalog-change`

**Principles**: `cpt-cf-bss-service-bindings-principle-single-authority`, `cpt-cf-bss-service-bindings-principle-frozen-evidence`.

### 1.3 Actors

| Actor | Role in feature |
| --- | --- |
| `cpt-cf-bss-service-bindings-actor-plan-author` | Author, check and submit bindings |
| `cpt-cf-bss-service-bindings-actor-buyer` | See the service terms in the preview and accept them |
| `cpt-cf-bss-service-bindings-actor-commerce` | Products validates; Orders calls acceptance; Subscriptions and Entitlements read receipts |

### 1.4 References

- **PRD**: [Product requirements](../PRD.md), §5.2 and §5.3.
- **Design**: [Architecture and contracts](../DESIGN.md), §3.3.3, §3.3.4, §3.3.7 and §3.3.8.
- **Decomposition**: [Feature allocation](../DECOMPOSITION.md).
- **Dependencies**: [Service catalog](0001-service-catalog.md).
- **Patterns**: the quota rules and their freeze (pricing D-540 to D-548, pending in PR #6105).

## 2. Actor Flows (CDSL)

### 2.1 Bind, publish and accept

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-flow-plan-bindings`

**Actor**: `cpt-cf-bss-service-bindings-actor-plan-author`.

**Use cases**: `cpt-cf-bss-service-bindings-usecase-buy`, `cpt-cf-bss-service-bindings-usecase-catalog-change`.

**Success Scenarios**: A saved, checked and published revision carries its bindings; an acceptance freezes the selected ones in its receipt.

**Error Scenarios**: Invalid, stale or conflicting writes change nothing. A sale with uncovered or changed service terms is refused before any receipt.

**Steps**:

1. [ ] - `p1` - Read the draft revision, its items and its bindings. - `inst-read`
2. [ ] - `p1` - List the service definitions of each item's SKU version through Products. - `inst-select`
3. [ ] - `p1` - Enter for each binding a service key, an item and dimension value, a definition, a basis and coefficient, a target rule, parameters and the policy slots. - `inst-edit`
4. [ ] - `p1` - Replace the complete set with the revision version and an operation key. - `inst-replace`
5. [ ] - `p1` - Read the checks of that exact revision and fix every failed binding and field. - `inst-check`
6. [ ] - `p1` - Submit, approve and publish through the existing approval unit; each boundary rechecks the bindings. - `inst-publish`
7. [ ] - `p1` - Show the buyer the service terms beside the prices with `selected_service_digest`. - `inst-preview`
8. [ ] - `p1` - Accept the sale with `expected_service_digest`; freeze the selected bindings in the receipt. - `inst-accept`

## 3. Processes / Business Logic (CDSL)

### 3.1 Binding set replacement

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-algo-binding-replace`

**Input**: An authorized replacement, the expected revision version and an operation key.

**Output**: The normalized saved set with its digest and the new version, or a refusal.

**Steps**:

1. [ ] - `p1` - Authorize the caller and the revision for replay or a new write - `inst-authority`
2. [ ] - `p1` - Look up the scoped operation key and compare the request content - `inst-replay`
3. [ ] - `p1` - **IF** the key has a recorded result for the same content - `inst-known-operation`
   1. [ ] - `p1` - **RETURN** the recorded result, even when its version is old - `inst-replay-result`
4. [ ] - `p1` - **IF** the key has a recorded result for other content - `inst-conflicting-operation`
   1. [ ] - `p1` - **RETURN** `IDEMPOTENCY_CONFLICT` without changes - `inst-conflict-result`
5. [ ] - `p1` - Parse the body and run Pricing's structural checks, including the SKU version in force on the revision's sale date for each bound item; collect every failure - `inst-parse`
6. [ ] - `p1` - Call `validate_service_bindings` with purpose `Authoring` outside the transaction; map each check to its code; a verdict that does not judge a binding against the definition it names is a logged 500 - `inst-validate`
7. [ ] - `p1` - **IF** any failure other than `SERVICE_QUALIFICATION_UNAVAILABLE` exists - `inst-invalid`
   1. [ ] - `p1` - **RETURN** one refusal that lists every failure with its binding and field: 409 when every failure is a duplicate or a changed definition, else 400 - `inst-invalid-result`
8. [ ] - `p1` - Inside the transaction, recheck the draft state, the version and the items; commit the normalized bindings, their pinned evidence, the set digest, the audit row, the new version and the key's result - `inst-commit`
9. [ ] - `p1` - **RETURN** the saved set and the new version - `inst-result`

### 3.2 Acceptance freeze

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-algo-acceptance-freeze`

**Input**: A sale query with selections, quantity and `expected_service_digest`, after the price and quota checks.

**Output**: A receipt with the frozen service snapshot, or a commercial refusal.

**Steps**:

1. [ ] - `p1` - Require the service terms and the expected digest together, else `RESOLUTION_CHANGED` - `inst-pair`
2. [ ] - `p1` - Select the bindings; refuse any uncovered service key with `SERVICE_BINDING_MISSING` - `inst-coverage`
3. [ ] - `p1` - Compare the expected and selected digests, else `RESOLUTION_CHANGED` - `inst-digest`
4. [ ] - `p1` - Require a whole quantity from 1 when a binding is selected, else `QUANTITY_INVALID` - `inst-quantity`
5. [ ] - `p1` - Validate the selected bindings through Products with purpose `Acceptance`, the quantity and each item's accepted SKU version; map the first applicable refusal in the order `NOT_SELLABLE`, `RESOLUTION_CHANGED`, `UNSUPPORTED_TERMS`, `QUANTITY_INVALID` - `inst-recheck`
6. [ ] - `p1` - Check Products' verdict against the stored parameters and Pricing's units; a broken verdict is a logged 500 - `inst-verdict`
7. [ ] - `p1` - Build the snapshot, run `verify_service_snapshot` and write it in the receipt's transaction - `inst-freeze`
8. [ ] - `p1` - **RETURN** the receipt with `service` - `inst-result`

### 3.3 Selection and examples

The selection follows DESIGN §3.3.4. A plan item `Backup` with the dimension `frequency` and two bindings of the service key `backup`:

| Binding | Dimension value | Parameters (as authored) |
| --- | --- | --- |
| B1 | null (default) | `{"frequency": "daily"}` |
| B2 | `hourly` | `{"frequency": "hourly"}` |

A sale of `Backup` at `hourly` selects B2. At `daily`, or without a value, it selects B1. With only B2, a sale at `daily` is refused `SERVICE_BINDING_MISSING`. A second service key `monitoring` on the same item adds its own binding to each sale.

| Basis and coefficient | Accepted quantity | Units |
| --- | --- | --- |
| `per_plan_unit`, 1 | 5 | 5 |
| `per_plan_unit`, 10 | 3 | 30 |
| `fixed`, 1 | 5 | 1 |

With bounds `min 1, step 1, max 100`, `per_plan_unit` 10 at quantity 11 gives 110 and is refused `QUANTITY_INVALID`.

### 3.4 Governance and history

Bindings are revision content. Edits to items and their dimensions use the same revision fence; an item with bindings cannot be deleted (`ITEM_HAS_SERVICE_BINDINGS`) and a changed item cannot silently retarget a binding. Checks report every failure as a row with `service_binding_id` and `field`, in the row order of DESIGN §3.3.3, for single and batch checks; a known invalid set on a readable revision is HTTP 200 with failed rows and `ready = false`. `SERVICE_BINDING_MISSING` is red for each sellable cell that no binding covers on the revision's sale date, so a revision cannot be published with cells that every sale would refuse.

Submit, every vote, immediate publication and acceptance revalidate through Products; a non-final vote keeps a refusal as an observation (pricing D-503). A scheduled revision is checked at its approval; its switch on the sale date makes no catalog call, because the ticker's system actor holds no catalog grant (pricing D-545). Acceptance rechecks availability on every sale, so a definition disabled or withdrawn after the approval cannot be sold. A disabled or withdrawn definition leaves saved drafts, published revisions and accepted receipts as they are. Copy and clone carry the bindings with their exact definition versions, remap items, keep binding IDs and service keys, copy the pinned evidence and recompute the digests; the copy's binding digests differ because they cover the revision.

A receipt read and an acceptance replay answer the stored snapshot after any catalog or plan change. The stored-terms check at fulfilment runs `verify_service_snapshot` and then the terms digest.

## 4. States (CDSL)

This feature uses the existing plan revision and receipt states. It adds no lifecycle state. Bindings follow their revision: editable in draft, immutable after submit.

## 5. Definitions of Done

### 5.1 Owner-contract conformance

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-dod-plan-bindings`

Owners implement this behavior and keep passing evidence for every criterion below. Under `DOCS-ONLY` traceability, checkboxes track implementation; they do not claim that tests have run.

**Implements**: `cpt-cf-bss-service-bindings-flow-plan-bindings`, `cpt-cf-bss-service-bindings-algo-binding-replace`, `cpt-cf-bss-service-bindings-algo-acceptance-freeze`.

**Constraints**: `cpt-cf-bss-service-bindings-constraint-first-release`, `cpt-cf-bss-service-bindings-constraint-platform`.

**Touches**: `PricingServiceBindingAuthoringClientV1`, `PricingReadV1::resolve`, `SellabilityV1::check` and `PricingAcceptanceV1::acceptance` (DESIGN §3.3.3–3.3.4). State follows `cpt-cf-bss-service-bindings-db-owner-state`.

Pricing adds one migration for the binding rows and the set digest, and extends the receipt wire in place. `pricing-sdk` publishes the types, the selection, the digest functions, `service_units` and `verify_service_snapshot`, with one test vector for each digest domain that Subscriptions and Entitlements reuse. Tests cover both database backends, the REST and SDK parity, the receipt wire and the refusal order.

## 6. Acceptance Criteria

These criteria have stable verification IDs. Business criteria stay in [PRD §9](../PRD.md#9-acceptance-criteria).

| Contract criterion | Relevant condition | Required guarantee |
| --- | --- | --- |
| SVB-C15 Draft replacement | Add, edit, remove and clear; a foreign item; a duplicate cell and key; a key on two items; a number as a parameter; a missing policy slot; each Products check | Only an authorized unlocked draft changes. All valid bindings, evidence, digest, audit, version and key result commit together. An invalid set writes nothing and names every failure. Parameters are saved complete. |
| SVB-C16 Concurrency and replay | Two writes on one ETag; a lost answer and its replay; key reuse with other content; deletion of an item with bindings | One write wins. The replay answers the first result. Other content conflicts. The item deletion is refused. |
| SVB-C17 Governance | Checks after an edit; a definition disabled before publication; copy and clone of a published revision | Checks name the revision version, digest, binding and field. Submit, votes and immediate publication recheck; the scheduled switch makes no catalog call and acceptance rechecks the sale. Copies keep exact definitions and evidence, remap items and recompute digests. Published bindings never change. |
| SVB-C18 Preview and selection | An exact value, the default, an uncovered key, two keys on one item, a revision without bindings | The selection takes exactly the rules of DESIGN §3.3.4. `SERVICE_BINDING_MISSING` names the item, value and key. A revision without bindings resolves without `service_terms`. |
| SVB-C19 Acceptance freeze | A missing or stale expected digest with unchanged prices; a fractional quantity; units off the grid; a definition not enabled; a catalog outage; a replay after a catalog change | Each refusal follows the order of DESIGN §3.3.4. A success freezes bindings, evidence, units, SKU versions and availability revisions in the receipt's transaction. A replay answers the frozen snapshot. A sale without bindings keeps its receipt bytes. |
| SVB-C20 Digests and verification | Two sales of one revision; a later revision with the same service; one changed member of each digest input; an edited stored snapshot | Both sales freeze the same binding digests. The later revision keeps the execution digest and changes the binding digest. Every member moves its digest. `verify_service_snapshot` refuses every broken member. Producer and consumer vectors match. |
