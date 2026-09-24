# Feature: Draft Capture and the Line Model

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-featstatus-capture-implemented`

- [ ] `p1` - `cpt-cf-bss-orders-lifecycle-feature-capture`

## Table of Contents

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Create a draft](#21-create-a-draft)
  - [2.2 Add a line](#22-add-a-line)
  - [2.3 Edit a header or edit/remove a line](#23-edit-a-header-or-editremove-a-line)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Classify authored fields and select a trigger](#31-classify-authored-fields-and-select-a-trigger)
  - [3.2 Prepare the date cascade for admission](#32-prepare-the-date-cascade-for-admission)
  - [3.3 Preserve draft shape and abandonment evidence](#33-preserve-draft-shape-and-abandonment-evidence)
- [4. States (CDSL)](#4-states-cdsl)
  - [4.1 Draft authoring and administrative edits](#41-draft-authoring-and-administrative-edits)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Authoring operations and stable identity](#51-authoring-operations-and-stable-identity)
  - [5.2 Shared field classification](#52-shared-field-classification)
  - [5.3 Date policy and admission contribution](#53-date-policy-and-admission-contribution)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Detailed Behavior Contracts](#7-detailed-behavior-contracts)
  - [Capture: Interactions and Sequences](#capture-interactions-and-sequences)
  - [Capture: What a draft may and may not hold (normative)](#capture-what-a-draft-may-and-may-not-hold-normative)
  - [Capture: The date cascade (normative)](#capture-the-date-cascade-normative)
  - [Capture: Draft abandonment](#capture-draft-abandonment)
  - [Capture: Traceability](#capture-traceability)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Create unvalidated drafts and author their header and lines through the transition engine. Classify fields once so commercial changes, frozen tenant axes and administrative edits follow the correct transition and concurrency rules.

### 1.2 Purpose

Capture the commercial shape of an order without catalog or pricing round trips during basket assembly. Retain stable line identity, authored dates, term and cycle for admission and later versioning.

**Requirements**: `cpt-cf-bss-orders-lifecycle-fr-order-create`, `cpt-cf-bss-orders-lifecycle-fr-order-line-dates`, `cpt-cf-bss-orders-lifecycle-fr-order-tenant-axes`, `cpt-cf-bss-orders-lifecycle-fr-order-amendment`, `cpt-cf-bss-orders-lifecycle-fr-order-submit`, `cpt-cf-bss-orders-lifecycle-nfr-order-transition-latency`, `cpt-cf-bss-orders-lifecycle-nfr-order-retention`, `cpt-cf-bss-orders-lifecycle-nfr-order-audit-completeness`.

**Principles**: `cpt-cf-bss-orders-lifecycle-principle-draft-is-unvalidated`, `cpt-cf-bss-orders-lifecycle-principle-line-identity-stable`, `cpt-cf-bss-orders-lifecycle-principle-field-class-declared`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-bss-orders-lifecycle-actor-orders-partner-admin` | Creates and authors permitted partner orders |
| `cpt-cf-bss-orders-lifecycle-actor-orders-direct-customer` | Creates and authors permitted self-service orders |
| `cpt-cf-bss-orders-lifecycle-actor-orders-seller-operator` | Seller role alone grants no creation, editing or submission; any permitted authoring requires an independently complete Partner Admin or Direct Customer path |

Actor labels do not grant access; every operation uses the shared authorization contract.

### 1.4 References

- **PRD**: [PRD.md](../PRD.md), §6.1, §6.2, §12.
- **Architecture**: [DESIGN.md](../DESIGN.md), including [02 models](../DESIGN.md#contract-02-3-1), [02 interfaces](../DESIGN.md#contract-02-3-3), and [02 persistence](../DESIGN.md#contract-02-3-7). Complete behavior is in [§7](#7-detailed-behavior-contracts).
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md).
- **Dependencies**: [Foundation](01-foundation.md), `cpt-cf-bss-orders-lifecycle-feature-foundation`.
- **Consumers**: [Gate and Pin](03-gate-and-pin.md) resolves dates and validates at admission; [Versioning](04-versioning.md) consumes field classification; [Hold and Expiry](07-hold-and-expiry.md) owns abandonment scheduling.

**UI applicability**: UI layout, keyboard navigation, screen-reader behavior and visual accessibility are not applicable because this feature specifies backend contracts, not a user interface. API usability, actionable errors and non-disclosing diagnostics remain applicable; consuming consoles own their UI requirements.

## 2. Actor Flows (CDSL)

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-new-acquisition`, `cpt-cf-bss-orders-lifecycle-usecase-order-amendment`.

### 2.1 Create a draft

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-flow-capture-create-draft`

**Actor**: Partner Admin or Direct Customer with a complete authoring grant. Seller-role membership alone does not authorize this operation.

**Success Scenarios**: Create an empty draft with an identity, seller-unique number, three tenant axes and trusted initiating actor; retry returns the original result.

**Error Scenarios**: Authorization denial, unsupported category, idempotency conflict or infrastructure failure.

**Steps**:
1. Receive `POST /bss-orders-lifecycle/v1/orders` with category, tenant arrangement, optional contract reference and idempotency key.
2. Register the category-admission guard and contribute the authored values. Derive actor identity from trusted context; never accept an actor override.
3. Delegate authorization, key ownership, identity/number allocation and aggregate initialization to the engine's dedicated create branch.
4. Commit `draft`, empty commercial version 1 and `draft_revision=0` with audit and replay outcome. Return the committed identity/number unchanged on retry.

The contract reference is recorded without resolution. An empty draft is valid. No catalog predicate, price pin, market or total is produced; shared platform authorization still applies and fails closed on outage.

### 2.2 Add a line

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-flow-capture-add-line`

**Actor**: Authorized order author.

**Success Scenarios**: Add one current working-set member with stable server-assigned identity and authored commercial values.

**Error Scenarios**: `not-admissible`, `version-conflict`, `currency-mixed`, `line-cap-exceeded`, or invalid structural input.

**Steps**:
1. Receive `POST /bss-orders-lifecycle/v1/orders/{orderId}/lines` with references, positive quantity, currency, authored dates, term/cycle, expected version/revision and key.
2. Select `draft-mutate`; register currency consistency and line-cap guards after the engine's authorization/idempotency/admissibility/revision checks.
3. Assign stable `line_id` and contribute working-set membership with the authored values, retaining dates exactly as authored without cascading them.
4. The engine inserts identity and membership atomically and increments `draft_revision` once, retaining commercial version 1. Return identity and committed revision.
5. Set a line external reference separately through administrative `PATCH`; mixing it into a commercial insert would violate one-request/one-trigger classification.

Bundle plans remain one line; one-time plans represent setup fees, hardware and prepaid items as ordinary lines. Neither shape expands into components or a new line kind. No add-on selection is expressible this phase.

### 2.3 Edit a header or edit/remove a line

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-flow-capture-edit-content`

**Actor**: Authorized order author.

**Success Scenarios**: Commercial draft mutation advances only draft revision; administrative edits route to the audited, non-versioning path.

**Error Scenarios**: `mixed-field-classes`, `tenant-axis-immutable`, `category-not-admitted`, `line-not-found`, `currency-mixed`, or the engine's earlier state/version/authorization refusal.

**Steps**:
1. For header/line `PATCH`, classify named fields using §3.1 before reading order state; fields alone select the trigger. Line `DELETE` always selects `draft-mutate`.
2. Route administrative-only edits to [Versioning](04-versioning.md)'s administrative edit process, including line membership checking. No draft revision is required or advanced there.
3. A header request naming commercial content selects `draft-mutate`. Register guards in order: mixed field classes, fixed seller, category admission. Changes to resource/payer additionally require authorization of the proposed arrangement before commercial fact resolution.
4. A line commercial edit/removal selects `draft-mutate`. Register guards in order: current draft membership, mixed classes for edit, currency consistency if currency changes. Edit/removal does not consume extra line-cap capacity.
5. The engine first applies authorization, replay, admissibility and revisions. Outside `draft`, commercial edits refuse `not-admissible`, ahead of slice guards. Within `draft`, missing or mismatched `expected_draft_revision` refuses `version-conflict`; the optional boundary field is not an early boundary rejection.
6. Apply the commercial contribution under the aggregate lock and increment revision once. Removal deletes working membership only, preserving the reserved identity. Return committed revision; a removed identity cannot be re-admitted or edited.

## 3. Processes / Business Logic (CDSL)

### 3.1 Classify authored fields and select a trigger

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-algo-capture-field-classification`

**Input**: Named header/line fields and operation.

**Output**: Exactly one trigger, plus registered guards.

| Class | Fields / behavior |
|-------|-------------------|
| Commercial | Membership, quantity, SKU/plan/price references, payer, category, line currency, three dates, term duration, billing cycle and contract reference; direct edits select `draft-mutate` |
| Commercial-frozen | Resource and seller tenant axes; seller is immutable from creation, resource from submit; direct edits still select `draft-mutate` |
| Administrative | Order/line external references, display labels and internal notes; administrative-only edits select `administrative-edit` |
| Never authored | Contract renewal election, term windows and notice ladder; surfaced by read-through, absent from line authoring |

1. Read the single declaration shared with Versioning; fail startup if any authored field lacks exactly one class.
2. If any commercial/frozen field is named, select `draft-mutate`; otherwise select `administrative-edit`. Do not read state to select a trigger.
3. The engine handles state admissibility first. In draft, a mixed-class request refuses `mixed-field-classes`; never split a request into two transitions under one key.
4. A draft edit naming seller refuses `tenant-axis-immutable`, even if it supplies the existing value. Resource/payer changes remain permitted only within their authorization and lifecycle rules.

### 3.2 Prepare the date cascade for admission

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-algo-capture-date-cascade`

**Input**: Authored line dates, the resource tenant's effective date-policy snapshot, proposed UTC date basis.

**Output**: Resolved date triple and policy provenance, or `date-cascade-invalid` contributed to the gate.

1. At submit/amendment/Preview, read the effective resource-tenant policy row, falling back to the validated platform default. Snapshot switches, source scope and revision once; missing/invalid policy fails the date guard.
2. Preserve authored contract-effective date; otherwise propose the server's UTC date before any date-dependent external calls.
3. For service-activation and acceptance-due dates, retain authored values. A required unauthored field refuses; defaults never satisfy a requirement. An optional unauthored field defaults to contract-effective date.
4. Use exactly these dates for dependent predicate/evaluation inputs. Before admitting the version, the engine samples its single pre-write timestamp and verifies all transition-date defaults match its UTC day.
5. If the day changed, settle `date-cascade-invalid` with no admitted version or stale assessment; a new attempt must resolve all dependent inputs again. A same-key retry replays the settled refusal.
6. Persist all three resolved dates and the identical policy snapshot on admitted lines. Later reads/fulfillment never reapply current policy; amendment snapshots policy again but carries existing resolved dates as values unless changed.

The acceptance due date is never recorded assent. Quoted service dates remain on the order even when activation is deferred; no billing/start backdating follows from them. Preview's indicative evaluation date binds no later submit.

### 3.3 Preserve draft shape and abandonment evidence

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-algo-capture-draft-invariants`

**Input**: Proposed draft working set and header.

**Output**: Structurally admitted engine contribution or registered refusal.

Enforce one currency and one payer/tenant arrangement per order, positive quantity, the configured 200-line baseline cap and `new_sale` admission. Reject `change` on create, commercial edits, submit and amendment until the change path ships. Preserve opaque references even when retired/unresolvable; catalog validation is admission's responsibility. Keep order numbers seller-unique and structurally opaque. Preserve the creation instant for abandonment; the expiry feature auto-voids rather than deletes the order.

## 4. States (CDSL)

### 4.1 Draft authoring and administrative edits

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-state-capture-authoring`

**Initial State**: `draft` after create, with version 1 and draft revision 0.

| From | Trigger | Result |
|------|---------|--------|
| No order | `create` | `draft`, empty version 1, trusted identity and audit |
| `draft` | `draft-mutate` | Remain `draft`; advance draft revision once, preserve version 1 |
| Any non-terminal state | `administrative-edit` | Remain in the same state; no version/draft-revision bump; changed fields audited through Versioning |
| Any other state | `draft-mutate` | Refuse `not-admissible` |

Submit and abandonment exits are owned by Gate and Pin and Hold and Expiry. There is no added date-waiting state or independent line state machine.

## 5. Definitions of Done

### 5.1 Authoring operations and stable identity

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-dod-capture-authoring`

The system **MUST** implement the five authoring endpoints declared in [Capture API contract](../DESIGN.md#contract-02-3-3) through engine contributions, enforce structural guards and both concurrency tokens, and preserve reserved line identities after removal. Draft authoring must not invoke catalog/evaluation/contract-resolution ports.

**Implements**: `cpt-cf-bss-orders-lifecycle-flow-capture-create-draft`, `cpt-cf-bss-orders-lifecycle-flow-capture-add-line`, `cpt-cf-bss-orders-lifecycle-flow-capture-edit-content`, `cpt-cf-bss-orders-lifecycle-algo-capture-draft-invariants`.

**Constraints**: `cpt-cf-bss-orders-lifecycle-constraint-single-currency-basket`, `cpt-cf-bss-orders-lifecycle-constraint-single-payer`, `cpt-cf-bss-orders-lifecycle-constraint-change-category-refused`, `cpt-cf-bss-orders-lifecycle-constraint-no-addon-selection`.

**Touches**: create/header and line authoring API; `cpt-cf-bss-orders-lifecycle-dbtable-order`, `cpt-cf-bss-orders-lifecycle-dbtable-order-line-identity`, `cpt-cf-bss-orders-lifecycle-dbtable-draft-content`.

### 5.2 Shared field classification

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-dod-capture-field-classes`

The system **MUST** expose one complete declaration consumed by capture and amendment, choose triggers without pre-authorization state reads, and route administrative edits to their separately audited working tables.

**Implements**: `cpt-cf-bss-orders-lifecycle-algo-capture-field-classification`, `cpt-cf-bss-orders-lifecycle-flow-capture-edit-content`.

**Constraints**: `cpt-cf-bss-orders-lifecycle-constraint-single-writer`.

**Touches**: header/line `PATCH`; `cpt-cf-bss-orders-lifecycle-dbtable-administrative-content`; shared field classifier.

### 5.3 Date policy and admission contribution

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-dod-capture-date-policy`

The system **MUST** install the validated platform default and tenant override policy shape, preserve authored dates in draft, and supply the gate/engine with the resolved triple, date basis and exact policy provenance. Startup must fail without the required default; deployment promotion is the policy channel and no Orders endpoint edits it.

**Implements**: `cpt-cf-bss-orders-lifecycle-algo-capture-date-cascade`.

**Constraints**: `cpt-cf-bss-orders-lifecycle-constraint-single-writer`.

**Touches**: `cpt-cf-bss-orders-lifecycle-dbtable-date-policy`, `cpt-cf-bss-orders-lifecycle-dbtable-order-line`, `cpt-cf-bss-orders-lifecycle-entity-line-date-set`.

Capture observability MUST implement the signals and alerts in [design §3.8](../DESIGN.md#contract-02-3-8): draft age and near-auto-void counts, per-order line-mutation rate, classification refusals by field, and missing required dates at submit. Alert on classification-refusal thresholds and successful draft creation followed by failing line insertion; these diagnose incomplete client flows without classifying every such flow as a Gear fault.

## 6. Acceptance Criteria

- [ ] Create an empty draft and retry: identity/number are stable, version stays 1 and actor cannot be overridden by the body; no catalog, pin or total is produced.
- [ ] Assemble/edit a basket during catalog outage while authorization remains available; authoring succeeds. PDP outage still fails closed.
- [ ] Mixed currency, unsupported category, zero/negative quantity and over-cap insertion refuse with the declared boundary/guard behavior; differing billing cycles are accepted.
- [ ] Two concurrent edits at the same draft revision cannot both mutate it. Commercial edits advance revision exactly once without appending a commercial version.
- [ ] Classify a mixed header/line request in draft: it refuses `mixed-field-classes`. Repeat outside draft, including omitted draft revision: the engine returns `not-admissible` first.
- [ ] Seller edits refuse in draft. Authorized resource/payer draft changes succeed under proposed-arrangement checks; administrative-only edits preserve both revision counters and audit actual changed values.
- [ ] Remove a line and attempt to edit it: receive `line-not-found`, retain reserved identity and omit the removed member from submit. Bundle and one-time lines remain one line each.
- [ ] Required unauthored service/acceptance dates fail the gate despite available defaults. Optional omitted dates resolve to contract-effective date, preserving authored values.
- [ ] Change date policy after snapshot: admitted lines retain the original snapshot. Cross UTC midnight during default-dependent resolution: refuse without stale admission and re-resolve on a fresh attempt.
- [ ] Startup rejects an unclassified authored field or missing platform date policy. Reads do not recalculate dates or treat due dates as assent.
- [ ] Draft auto-void integration preserves the order and audit; capture itself schedules no worker. Measure draft creation/line mutation under the declared 200-line baseline separately from external authorization latency.

- [ ] Seller-role-only principals cannot create, edit or submit orders. A principal holding multiple roles must satisfy one complete permitted authoring path; role fragments cannot be combined to gain access.

- [ ] Integration scenarios verify each capture metric and the classification/partial-client-flow alerts, including missing required dates at submit and drafts approaching a configured auto-void TTL.

## 7. Detailed Behavior Contracts

**Contract namespace 02.** Section numbers inside the detailed contracts below resolve
through the [contract address index](../DECOMPOSITION.md#4-contract-address-index),
not the overview section numbers above.

The following sections retain the complete algorithms, guards, state transitions and
feature-local rules. The preceding flows and acceptance criteria summarize these contracts;
shared schemas and interfaces are defined in [DESIGN.md](../DESIGN.md).

<a id="contract-02-3-6"></a>

<!-- contract:02-capture:3.6 -->
### Capture: Interactions and Sequences

<a id="contract-02-create-draft-order"></a>

#### Create draft order

**ID**: `cpt-cf-bss-orders-lifecycle-seq-create-draft`

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-new-acquisition`

**Actors**: `cpt-cf-bss-orders-lifecycle-actor-orders-partner-admin`, `cpt-cf-bss-orders-lifecycle-actor-orders-direct-customer`. Seller-role membership grants no authoring permission; an independently complete buyer path is required.

**Algorithm: Create Draft Order**

Input: category, tenant_axes, contract_id, security_context, idempotency_key
Output: order identity and number, or a registered refusal

1. [ ] - `p1` - Declare the category-admissibility guard so the engine evaluates it and audits its refusal - `inst-cd-declare-category-guard`
2. [ ] - `p1` - Delegate new identity assignment to the engine's dedicated create branch, after idempotency ownership and guard acceptance; never mint another returned identity on replay - `inst-cd-assign-identity`
3. [ ] - `p1` - Delegate seller-unique order-number assignment to the same branch and return its committed number on replay - `inst-cd-assign-number`
4. [ ] - `p1` - Prepare the three tenant axes for engine authorization and derive the initiating actor from trusted SecurityContext, never from a request-body override; persist nothing here. Foundation §3.6's creation initialization table defines the engine's aggregate and version-1 writes - `inst-cd-record-axes`
5. [ ] - `p1` - **IF** a contract reference was supplied: record it without resolving it - `inst-cd-record-contract-ref`
6. [ ] - `p1` - Request the dedicated Create Transition branch of [01 §3.6](01-foundation.md#contract-01-3-6) with the contribution and scoped idempotency key - `inst-cd-request-transition`
7. [ ] - `p1` - **RETURN** the engine's committed identity and number or registered refusal, unchanged on replay - `inst-cd-return-identity`

**Description**: Nothing here reaches outside the gear. The contract reference is recorded, not
resolved — resolution is a submit-gate predicate, so a contract that goes inactive between
capture and submit is caught where it matters.

<a id="contract-02-author-a-line-and-resolve-its-dates"></a>

#### Author a line and resolve its dates

**ID**: `cpt-cf-bss-orders-lifecycle-seq-author-line`

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-new-acquisition`

**Actors**: `cpt-cf-bss-orders-lifecycle-actor-orders-partner-admin`

**Algorithm: Author Line**

Input: order_id, sku_id, plan_id, price_id, qty, currency, dates, term_duration, billing_cycle, expected_version, expected_draft_revision, security_context, idempotency_key
Output: line_id, or a registered refusal

1. [ ] - `p1` - Declare currency-consistency against the order's other lines and line-cap slice guards. The engine first applies authorization (`order × write`), idempotency resolution, state-table admissibility and the expected_version and expected_draft_revision checks ([01 §4.1](../DESIGN.md#contract-01-4-1)); line authoring is admissible only in draft - `inst-al-declare-guards`
2. [ ] - `p1` - Assign a stable line_id - `inst-al-assign-line-id`
3. [ ] - `p1` - Retain any explicitly authored calendar field as authored; resolution of the cascade belongs to the gate per §4.2 and is **not** performed here - `inst-al-retain-authored-dates`
4. [ ] - `p1` - Record term_duration and billing_cycle as authored - `inst-al-record-term-cycle`
5. [ ] - `p1` - Request the line-authoring transition with expected_draft_revision; the engine checks it against the locked draft revision and increments draft_revision atomically with the commercial edit, without advancing current_version. A mismatch returns version-conflict - `inst-al-request-transition`
6. [ ] - `p1` - **RETURN** line_id and the committed draft_revision - `inst-al-return-line-id`

**Description**: Explicit values are retained in draft; the cascade is resolved once at submit
and stored with the admitted version. A later reader never re-derives it, which is what stops two
surfaces disagreeing about an effective date after a policy switch changes. The line's external
reference is administrative (§4.3), so it is not accepted here: a line insert is a `draft-mutate`
request and one request maps to one trigger (D-118). It is set afterwards through line `PATCH`,
which routes an administrative-only edit to the `administrative-edit` path (*Edit or Remove Line*,
D-117).

<a id="contract-02-edit-the-order-header"></a>

#### Edit the order header

**ID**: `cpt-cf-bss-orders-lifecycle-seq-edit-order`

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-new-acquisition`, `cpt-cf-bss-orders-lifecycle-usecase-order-amendment`

**Actors**: `cpt-cf-bss-orders-lifecycle-actor-orders-partner-admin`, `cpt-cf-bss-orders-lifecycle-actor-orders-direct-customer`

**Algorithm: Edit Order**

Input: order_id, the named header fields with their new values, expected_version, expected_draft_revision (commercial draft edits only; optional at the boundary, D-147), security_context, idempotency_key
Output: the committed draft_revision (commercial draft edit) or applied (administrative edit), or a registered refusal

1. [ ] - `p1` - Classify every named field through the §4.3 declaration; the classes alone select the trigger, and **no state is read** to choose it, because authorization precedes any state read ([01 §4.1](../DESIGN.md#contract-01-4-1), D-145). The engine's state-table admissibility then decides whether the selected trigger applies in the current state - `inst-eo-classify-fields`
2. [ ] - `p1` - **IF** every named field is administrative: hand the fields to [04-versioning — Interactions and Sequences](04-versioning.md#contract-04-3-6) *Apply Administrative Edit*, which requests the `administrative-edit` transition ([01 §4.3](01-foundation.md#contract-01-4-3) row 3) in any non-terminal state and neither requires nor increments `draft_revision`; skip steps 3-5 - `inst-eo-route-administrative`
3. [ ] - `p1` - Otherwise the request names at least one commercial field and is a commercial draft edit under `draft-mutate` ([01 §4.3](01-foundation.md#contract-01-4-3) row 2); outside `draft` no row exists and the engine returns `not-admissible` naming the state and trigger (D-145). Declare its slice guards in this registration order: one trigger per request (`mixed-field-classes`, refusing a request that also names an administrative field; §4.3, D-118), the fixed seller (`tenant-axis-immutable`, refusing any `sellerTenantId` value; §4.1, D-119) and category admissibility (`category-not-admitted`). The engine first applies authorization — `order × write`, plus [08 §4.3](../DESIGN.md#contract-08-4-3)'s check of the proposed tenant arrangement when `resourceTenantId` or `payerTenantId` changes — then idempotency resolution, state-table admissibility and the expected_version and expected_draft_revision checks ([01 §4.1](../DESIGN.md#contract-01-4-1)) - `inst-eo-declare-guards`
4. [ ] - `p1` - Request the `draft-mutate` transition with expected_draft_revision, contributing the changed header values; the engine writes them to the aggregate under its lock and increments `draft_revision` atomically without advancing `current_version`. The contract reference is recorded, not resolved - `inst-eo-request-transition`
5. [ ] - `p1` - **RETURN** the committed draft_revision, or the engine's registered refusal - `inst-eo-return`

**Description**: The field class chooses the trigger; the state, checked by the engine after
authorization, only decides whether the commercial trigger is admissible (D-145). In `draft` a commercial header edit is authoring and carries
the draft revision; an administrative edit in any non-terminal state bumps nothing and is
audited per field. A request that would need both triggers is refused rather than split, because
splitting would commit two transitions under one idempotency key.

<a id="contract-02-edit-or-remove-a-line"></a>

#### Edit or remove a line

**ID**: `cpt-cf-bss-orders-lifecycle-seq-edit-or-remove-line`

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-new-acquisition`, `cpt-cf-bss-orders-lifecycle-usecase-order-amendment`

**Actors**: `cpt-cf-bss-orders-lifecycle-actor-orders-partner-admin`, `cpt-cf-bss-orders-lifecycle-actor-orders-direct-customer`

**Algorithm: Edit or Remove Line**

Input: order_id, line_id, operation (`edit` for `PATCH`, `remove` for `DELETE`), the named line fields with their new values (edit only), expected_version, expected_draft_revision (draft commercial edit or removal only; optional at the boundary, D-147), security_context, idempotency_key
Output: the committed draft_revision (draft commercial edit or removal) or applied (administrative edit), or a registered refusal

1. [ ] - `p1` - Classify every named line field through the §4.3 declaration; as in *Edit Order* step 1, the classes alone select the trigger and no state is read to choose it (D-145) - `inst-el-classify-fields`
2. [ ] - `p1` - **IF** the operation is an edit and every named field is administrative: hand line_id and the fields to [04-versioning — Interactions and Sequences](04-versioning.md#contract-04-3-6) *Apply Administrative Edit*, which resolves line_id against current membership (`line-not-found`) and writes `orders_order_line_admin` (D-117); skip steps 3-5 - `inst-el-route-administrative`
3. [ ] - `p1` - Otherwise the request is a removal or an edit naming at least one commercial field, under `draft-mutate` ([01 §4.3](01-foundation.md#contract-01-4-3) row 2); outside `draft` no row exists and the engine returns `not-admissible` naming the state and trigger (D-145). Declare its slice guards in this registration order: line membership (`line-not-found`, refusing a line_id that is not in the draft working set — never inserted, or removed), one trigger per request (`mixed-field-classes`, for an edit also naming an administrative field; D-118) and, for an edit changing the line currency, currency consistency against the order's other lines (`currency-mixed`). The line cap does not apply, because neither an edit nor a removal adds a line. The engine first applies authorization (`order × write`), idempotency resolution, state-table admissibility and the expected_version and expected_draft_revision checks ([01 §4.1](../DESIGN.md#contract-01-4-1)) - `inst-el-declare-guards`
4. [ ] - `p1` - Request the `draft-mutate` transition with expected_draft_revision, contributing the changed authored values or the removal; authored dates are retained as authored (§4.2). On removal the engine deletes only the line's working-set membership, leaving its identity reserved ([01 §3.7](../DESIGN.md#contract-01-3-7)); either way it increments `draft_revision` atomically without advancing `current_version` - `inst-el-request-transition`
5. [ ] - `p1` - **RETURN** the committed draft_revision, or the engine's registered refusal - `inst-el-return`

**Description**: A removed `line_id` is never re-admitted: it answers `line-not-found` exactly
like an identifier that never existed, so a stale client cannot edit a line the working set no
longer holds. After submit the line's commercial content belongs to the version chain and
changes only by amendment ([`04-versioning`](../DESIGN.md#contract-04-1-1)); its administrative fields stay
editable in place through the same endpoint.


<!-- /contract -->

<a id="contract-02-4-1"></a>

<!-- contract:02-capture:4.1 -->
### Capture: What a draft may and may not hold (normative)

A `draft` order **MUST** be modifiable freely: lines added, amended and removed, administrative
and commercial content alike edited in place — with one exception, the seller, below. This is possible because draft content lives in the
**mutable** `orders_draft_content` table rather than in the append-only version chain, and the
submit transition materialises it into version 2 ([01-foundation — Database Schemas and Tables](../DESIGN.md#contract-01-3-7)).
Without that separation, "a draft is freely modifiable" and "the version chain is append-only"
would contradict each other. Authoring **MUST NOT** resolve a catalog
reference, evaluate a sellability predicate, capture a price pin or produce a resolved total. A
`draft` **MAY** therefore hold references that no longer resolve, and that state **MUST NOT** be
treated as an error until submit.

**The seller is fixed at creation.** The order number is allocated unique per `sellerTenantId`
at creation ([01-foundation — Database Schemas and Tables](../DESIGN.md#contract-01-3-7)), so a later seller change would either
break that uniqueness or silently renumber an addressable order. `sellerTenantId` **MUST
NOT** change after creation by any path, and a draft edit naming it **MUST** be refused with
`tenant-axis-immutable` (§3.6 *Edit Order*; [`../DECISIONS.md`](../DECISIONS.md) D-119). The
resource and payer axes stay editable in `draft`; a seller change is a new order.

Every commercial draft edit (including line insertion/removal and resource or payer axis
changes) requires `expected_draft_revision`, included in the idempotency request fingerprint. On
`draft-mutate` it is **optional at the boundary** (D-147): omitting it is never a boundary
rejection, since a client of an order past `draft` has never been shown a `draftRevision`. The
engine compares it only at foundation §3.6 *Attempt Transition* step 12, after step 11's
admissibility check, so a commercial `PATCH` outside `draft` refuses `not-admissible` (D-145), and
in `draft` an omitted value refuses `version-conflict` naming the current draft revision. An
omitted value enters the fingerprint as the not-applicable sentinel. The engine checks
and increments the aggregate's monotonic `draft_revision` in the same transaction as the edit;
`current_version` stays 1. Reads and successful draft writes return the revision. Submit binds
its prepared inputs to the server-read revision and checks both revisions under the aggregate
lock before using gate results, as specified by foundation §3.6.

An order **MUST** carry at least one line at submit; the gate enforces it. A `draft` with zero
lines is valid and expected — it is the state an order exists in between creation and the first
line.


<!-- /contract -->

<a id="contract-02-4-2"></a>

<!-- contract:02-capture:4.2 -->
### Capture: The date cascade (normative)

Each line carries three calendar fields. Their relationship is fixed:

| Field | Requirement | Default |
|-------|-------------|---------|
| Contract-effective date | Mandatory at submit | UTC calendar date of the engine's transition timestamp `t`, if unauthored |
| Service-activation date | Optional unless the tenant policy switch requires it; a required value **MUST** be authored | The contract-effective date, only when the switch does not require the field |
| Customer-acceptance due date | Optional unless the tenant policy switch requires it; a required value **MUST** be authored | The contract-effective date, only when the switch does not require the field |

**A default never satisfies a requirement.** Where the policy snapshot requires a service-activation
or acceptance-due date, only an authored value satisfies it; the cascade **MUST NOT** fill a
required field from the contract-effective date. Where the field is not required and is left
unauthored, the cascade fills it with its default and that resolved value is stored — an
admitted line never stores NULL for any of the three dates (D-60).

An explicitly authored date is retained in draft. At submit, the gate **MUST** resolve the
cascade: where the contract-effective date remains unauthored, it **MUST** use the UTC date of
the engine-selected transition timestamp `t`, then derive any unauthored dependent dates from
that resolved value. `t` is sampled once before writes under the engine's shared `committed_at`
convention; it is not the unknowable future physical database commit instant. The resolved
contract-effective, service-activation and acceptance-due dates **MUST** be stored respectively in
`orders_order_line.contract_effective_date`, `.service_activation_date` and
`.acceptance_due_date`; only the tenant policy-switch state that governed the cascade belongs in
`orders_order_line.date_policy_switch_state`. Resolution **MUST NOT** be deferred to read time or
to fulfillment; it is a submit contribution materialised with the admitted version.

**Policy provenance precedes version creation.** Date requirements are Orders-owned policy rows
in `orders_date_policy` (§3.7): the row for the order's `resourceTenantId` if present, else the
platform default row. The gate reads the effective row once per submit/amendment/Preview run and
snapshots its switches (`service_activation_required`, `acceptance_due_required`), the source
row's scope (tenant row or platform default) and its `revision`. Deployment must provide a
validated platform default; a missing or invalid policy fails the date guard rather than
inventing permissive switches. This snapshot is a resolved input, never authored draft content
and never a read from a version row that has not been created. The admitted line stores that
exact snapshot in `date_policy_switch_state` for later explanation even if the policy row is
subsequently promoted to a new revision ([`../DECISIONS.md`](../DECISIONS.md) D-121).

Before date-dependent external resolution, the gate chooses a proposed UTC effective date from
the server clock and resolves unauthored cascade fields against it. All predicates and
evaluations depending on these dates receive that exact proposed date; it is carried with the
resolved inputs. Inside the transaction, after engine prechecks and before accepting gate
results or writing an admitted version, the engine samples its single timestamp `t` and checks
that every transition-date default still equals `UTC-date(t)`. If the UTC day changed during
resolution, it settles a `date-cascade-invalid` refusal identifying the stale date basis;
it must not replace the date while retaining predicates/totals computed for the previous date.
A fresh attempt resolves all date-dependent inputs again outside the transaction. An idempotent
replay retains the settled refusal. When the dates match, the engine materialises the validated
dates and policy snapshot in the admission transaction, using the same `t` for its transition
timestamps. Preview uses its evaluation instant for indicative defaults and does not bind a
later submit to that date. An amendment snapshots policy again and
resolves its proposed content; carried-forward resolved dates remain values unless changed by
the amendment. No later read or fulfillment re-applies current policy.

Three rules constrain it. The acceptance **due date is a calendar field** and **MUST NOT** be
treated as recorded assent — it never satisfies the acceptance instant owned by
[`05-preconditions`](../DESIGN.md#contract-05-1-1), and the cascade that fills it **MUST NOT** be read
as defaulting that instant. "When access begins" and "when billing begins" are **independent
axes**, so a service-activation date carries no billing implication of its own. And the quoted
service-activation date **MUST** be retained even when the activation barrier defers the line
past it, because the requested date is what the buyer agreed to and the actual instant is what
the subscription starts on.

**A missing required date is refused at the gate.** Where the policy switch requires a calendar
field and the line does not author it, submit **MUST** be refused with the registered
`date-cascade-invalid` reason, carried in the gate's all-failures report alongside every other
predicate failure ([03-gate-and-pin — Interactions and Sequences](03-gate-and-pin.md#contract-03-3-6) *Run Gate and Submit*) rather
than returned ahead of the engine. The
rejected alternative was a twelfth order state holding such an order until the date arrives;
it was rejected because a data-entry omission would then acquire its own TTL, its own guards and
its own event, and because the PRD deliberately adds no waiting state for the analogous
mixed-date barrier. The order stays in `draft`, where it is already freely editable.


<!-- /contract -->

<a id="contract-02-4-5"></a>

<!-- contract:02-capture:4.5 -->
### Capture: Draft abandonment

An abandoned `draft` is governed by the auto-void TTL owned by
[`07-hold-and-expiry`](../DESIGN.md#contract-07-1-1). This slice supplies the only input that sweep
needs — the creation instant on the aggregate — and states the outcome contract: an abandoned
draft is **auto-voided, never deleted**, so its audit trail survives. Nothing in this slice
schedules or performs the sweep.


<!-- /contract -->

<a id="contract-02-5"></a>

<!-- contract:02-capture:5 -->
### Capture: Traceability

- **PRD**: [`../PRD.md`](../PRD.md) — §6.1 order creation, line dates, term and cycle
- **Gear design**: [`../DESIGN.md`](../DESIGN.md) — realises `cpt-cf-bss-orders-lifecycle-component-capture`
- **Engine**: [`01-foundation`](../DESIGN.md#contract-01-1-1) — transition contract, schema, reason registry
- **Consumers**: [`03-gate-and-pin`](../DESIGN.md#contract-03-1-1) snapshots tenant date policy and reads authored lines; [`04-versioning`](../DESIGN.md#contract-04-1-1) reads the field classification; [`07-hold-and-expiry`](../DESIGN.md#contract-07-1-1) reads the creation instant
- **Design set**: [`./README.md`](../DECOMPOSITION.md)
- **ADRs**: [`ADR/0001`](../ADR/0001-cpt-cf-bss-orders-lifecycle-adr-transition-through-engine.md) transition through the engine; [`ADR/0002`](../ADR/0002-cpt-cf-bss-orders-lifecycle-adr-slice-decomposition.md) the foundation-plus-seven-slices decomposition; [`ADR/0004`](../ADR/0004-cpt-cf-bss-orders-lifecycle-adr-closed-enumerations.md) the closed state and event enumerations

<!-- /contract -->
