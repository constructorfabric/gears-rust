# Feature: Sellability Gate, Price Pin and Preview


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Submit a draft](#21-submit-a-draft)
  - [2.2 Preview a basket](#22-preview-a-basket)
  - [2.3 Re-check before first activation](#23-re-check-before-first-activation)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Assess all predicates and pin outcomes](#31-assess-all-predicates-and-pin-outcomes)
  - [3.2 Fix catalog scope and preserve commercial evidence](#32-fix-catalog-scope-and-preserve-commercial-evidence)
  - [3.3 Bound upstream resolution](#33-bound-upstream-resolution)
  - [3.4 Persist diagnostic identity and replay](#34-persist-diagnostic-identity-and-replay)
- [4. States (CDSL)](#4-states-cdsl)
  - [4.1 Admission and assessment outcomes](#41-admission-and-assessment-outcomes)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Shared admission assessment](#51-shared-admission-assessment)
  - [5.2 Preview and durable explanations](#52-preview-and-durable-explanations)
  - [5.3 Activation re-check integration](#53-activation-re-check-integration)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Detailed Behavior Contracts](#7-detailed-behavior-contracts)
  - [Gate and pin: Interactions and Sequences](#gate-and-pin-interactions-and-sequences)
  - [Gate and pin: The Orders delta (normative)](#gate-and-pin-the-orders-delta-normative)
  - [Gate and pin: Preview (normative)](#gate-and-pin-preview-normative)
  - [Gate and pin: Traceability](#gate-and-pin-traceability)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-featstatus-gate-and-pin-implemented`

- [ ] `p1` - `cpt-cf-bss-orders-lifecycle-feature-gate-and-pin`
## 1. Feature Context

### 1.1 Overview

Assess a basket against the adopted Pricing predicates and Orders-specific checks, then atomically submit it with accepted bindings and received totals. Reuse that assessment for amendment and Preview; define the activation re-check executed by Workflow.

### 1.2 Purpose

Ensure admission has complete, explainable evidence and consistent per-line revisions and a common assessment without moving price calculation, approval policy or subscription activation into Lifecycle.

**Requirements**: `cpt-cf-bss-orders-lifecycle-fr-order-submit`, `cpt-cf-bss-orders-lifecycle-fr-order-tenant-axes`, `cpt-cf-bss-orders-lifecycle-fr-order-atomic-fulfillment`, `cpt-cf-bss-orders-lifecycle-fr-orders-boundary-r4-no-price`, `cpt-cf-bss-orders-lifecycle-nfr-order-snapshot-integrity`, `cpt-cf-bss-orders-lifecycle-nfr-order-transition-latency`, `cpt-cf-bss-orders-lifecycle-interface-order-ops`.

**Principles**: `cpt-cf-bss-orders-lifecycle-principle-adopt-not-fork-gate`, `cpt-cf-bss-orders-lifecycle-principle-pin-is-the-commit`, `cpt-cf-bss-orders-lifecycle-principle-resolve-outside-decide-inside`, `cpt-cf-bss-orders-lifecycle-principle-preview-shares-implementation`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-bss-orders-lifecycle-actor-orders-partner-admin` | Submits or previews authorized buyer arrangements |
| `cpt-cf-bss-orders-lifecycle-actor-orders-direct-customer` | Submits or previews authorized self-service arrangements |
| `cpt-cf-bss-orders-lifecycle-actor-orders-seller-operator` | Seller role grants neither submit nor Preview; either action requires an independently complete Partner Admin or Direct Customer authorization path |
| `cpt-cf-bss-orders-lifecycle-actor-orders-catalog-pricing` | Supplies revision, purchase-predicate, price and accepted-binding facts through owning contracts |
| `cpt-cf-bss-orders-lifecycle-actor-orders-idp-ams` | Supplies tenant validity and payer commercial profile |
| `cpt-cf-bss-orders-lifecycle-actor-orders-contracts` | Supplies contract status and party eligibility |
| `cpt-cf-bss-orders-lifecycle-actor-orders-workflow` | Executes the pre-activation re-check and handles its outcomes |
| `cpt-cf-bss-orders-lifecycle-actor-orders-subscriptions` | Supplies occupancy and owns atomic active-state concurrency enforcement |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md), §6.1, §9.1 and §12.
- **Architecture**: [DESIGN.md](../DESIGN.md), including [03 models](../DESIGN.md#contract-03-3-1), [03 interfaces](../DESIGN.md#contract-03-3-3), and [03 persistence](../DESIGN.md#contract-03-3-7). Complete behavior is in [§7](#7-detailed-behavior-contracts).
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md).
- **Dependencies**: [Foundation](01-foundation.md), `cpt-cf-bss-orders-lifecycle-feature-foundation`; [Capture](02-capture.md), `cpt-cf-bss-orders-lifecycle-feature-capture`.
- **Upstream blockers**: [UPSTREAM_REQS.md](../UPSTREAM_REQS.md) retains the unexposed `PricingReadV1` trait, the `bss-orders.system` grant, the residual purchase verdict, Subscriptions' compare-at-activation step and complete item coverage, the SUB-G1 overlap-key answer, Rating evaluation/TCV, payer profile, Contracts, indicative tax, Subscriptions occupancy and atomic activation enforcement. Missing contracts remain unevaluable/unavailable; no local substitute is authorized.
- **Open decisions**: [DECISIONS.md](../DECISIONS.md) retains latency/budget ratification, partner overlap-key dimension, activation-duration policy and quote coverage (Q-29). The new initial-binding deadline is specified in DESIGN §4.3; its producer capability and Product duration policy remain open.

**UI applicability**: UI layout, keyboard navigation, screen-reader behavior and visual accessibility are not applicable because this feature specifies backend contracts, not a user interface. API usability, actionable errors and non-disclosing diagnostics remain applicable; consuming consoles own their UI requirements.

## 2. Actor Flows (CDSL)

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-new-acquisition`, `cpt-cf-bss-orders-lifecycle-usecase-order-amendment`, `cpt-cf-bss-orders-lifecycle-usecase-order-fulfillment-complete`.

### 2.1 Submit a draft

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-flow-gate-and-pin-submit`

**Actor**: An authorized order submitter.

**Success Scenarios**: All required inputs pass; submit commits version 2, pins, totals, dates, market, diagnostics, audit, claims, replay response and `OrderSubmitted` enqueue together.

**Error Scenarios**: Engine authorization/idempotency/state/revision refusal; empty basket; structural or date-basis refusal; failed/unevaluable assessment; authoritative overlap collision; persistence failure.

**Steps**:
1. Receive `POST /bss-orders-lifecycle/v1/orders/{orderId}/submit` with key and expected version/draft revision. Let the engine authorize current/proposed relationships and resolve replay before external guard work.
2. Snapshot the authorized authored basket under its lock with `prepared_draft_revision`; declare the non-empty guard and snapshot effective resource-tenant date policy. Prepare the exact proposed dates before dependent resolution.
3. Resolve identity once, including the payer profile, and derive market from the payer. Fix the seller scope, common assessment date and revision per line; no latest-revision substitution.
4. Resolve each line's catalog product key at that version in one batched call; build the payer/product claim tuple from that result, never from line labels or guessed identifiers.
5. Resolve independent catalog predicates, referenced-contract facts, batched occupancy and accepted-binding composition under §3.3; Rating evaluation follows composition. Independent checks run in parallel, and composition still records its outcome when an independent check fails. Skip only operations whose prerequisite input is missing.
6. Assess the adopted predicates and all nine Orders predicates (§3.1), retaining passed, failed and unevaluable results. Include each line's pin outcome. Keep the failure list separate and avoid duplicate reference/pin or date failures.
7. Pass the complete assessment and admission/refusal contribution to the engine. Locked state/version/client/prepared-revision and pre-gate structural checks retain precedence. Check the date defaults against the engine timestamp before accepting date-dependent evidence.
8. On refusal after assessment, atomically persist complete diagnostic rows, audit and owned idempotency response; write no commercial version, pin, total or event. Replace advisory overlap predicate 9 with any authoritative claim collision before settlement.
9. On admission, atomically materialize the version with exact pins, received totals, dates/policy, payer market and resolved overlap keys. Contribute automatic acceptance only when Preconditions' submit-request rule allows it: no proof reference and submitter tenant equals resource tenant; `sales_path` alone is insufficient.
10. Return the committed response and assessment identity. Same-key replay returns the immutable original response without another run, even if upstream inputs later change.

### 2.2 Preview a basket

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-flow-gate-and-pin-preview`

**Actor**: PDP-authorized Partner Admin or Direct Customer; seller-only permission is insufficient.

**Success Scenarios**: Return complete gate results and received indicative commercial figures without creating or mutating an order; missing term/cycle withholds only TCV.

**Error Scenarios**: Denied assessment scope, rate limit, failed/unevaluable inputs or diagnostic persistence failure.

**Steps**:
1. Receive `POST /bss-orders-lifecycle/v1/orders/preview` with the basket. Enforce axis-specific authorization/delegation before resolving facts about foreign parties; seller choice is a permitted selling relationship, not representation of that seller.
2. Allocate a new run identity and stable run-local line IDs; invoke the same revision-fixed seller-scoped assessment and date preparation as submit, including pin resolvability, without committing pins.
3. Invoke indicative tax only here through its owning port. Return per-line results, received pre-tax totals, indicative tax and expected fulfillment time/per-line deferral; return no approval-requirement verdict.
4. When any line lacks term duration or billing cycle, return success with `tcv` absent and `tcvWithheld: {reason: "preview-term-or-cycle-missing", lineIds: [...]}`. Preserve all other response fields; never assume a term.
5. Commit the complete authorized diagnostic vector before returning its assessment ID. Persist neither order nor Preview totals/TCV/tax. Each repeated Preview is a new run with seven-day diagnostic retention, not an idempotent quote or an offer with validity.

### 2.3 Re-check before first activation

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-flow-gate-and-pin-activation-recheck`

**Actor**: Authenticated Workflow using the owning SDKs after begin-fulfillment commits.

**Success Scenarios**: `proceed` permits Workflow to request the spawn signal and begin dispatch, subject to downstream enforcement.

**Error Scenarios**: `reject` for actual market/occupancy conflict; `not-dispatchable` for hold/terminal/superseded version; `defer` for missing live inputs.

**Steps**:
1. Read the authorized current order/version and stored payer, market and overlap keys. If held, terminal or superseded, return `not-dispatchable` with observed state/version; stop dispatch and do not acknowledge failure just for that result.
2. Read the current payer profile. Unavailability returns `defer(identity-party-unavailable)`; an actual market difference returns per-line `reject(market-divergence)`.
3. Read occupancy in one batch using stored keys, never a new registry lookup. Missing count/limit returns `defer(overlap-presence-unevaluable)`.
4. Compare active count plus pending lines with the effective maximum, returning `reject(overlap-collision)` on excess. Also reject an elapsed stored activation deadline as `order-binding-expired`; otherwise return `proceed`, subject to Subscriptions' pinned comparison at activation (DESIGN §4.3, D-162).
5. Workflow handles `reject` by voiding wave-1 drafts and acknowledging failure with reasons and compensation evidence. It handles `not-dispatchable` by rereading/waiting/following supersession or ending terminal work. On `defer`, retry from step 1 under the design baseline of three attempts over at most 60 seconds; dispatch remains stopped. Exhaustion voids drafts and acknowledges failure with the unavailable-port reason.

This is a Workflow integration algorithm, not a Lifecycle endpoint or begin-fulfillment guard. A passing read is only an early-abort check: Subscriptions must enforce concurrent-active cardinality at its own active commit. A collision after active commit is a fulfillment failure requiring rollback evidence, not a fictitious rejected inactive line. That upstream enforcement remains open: until `…-upreq-overlap-activation-atomicity` is delivered, subscription-side cardinality is advisory at order time and the path is not production-ready (D-180).

## 3. Processes / Business Logic (CDSL)

### 3.1 Assess all predicates and pin outcomes

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-algo-gate-and-pin-assessment`

**Input**: Authorized basket, fixed seller/revision inputs, date/policy basis and resolved port values.

**Output**: Complete ordered outcome vector and separate ordered failures.

Evaluate the nine catalog rows of [DESIGN §4.1](../DESIGN.md#contract-03-4-1): six from the `PricingReadV1` answers (`current_revision`, `resolve`) and the Products `get_sku` flags, recorded locally as `passed`, `failed` with `catalog-predicate-failed`, or `unevaluable` with `catalog-predicate-unevaluable`; the residual rows (market applicability, the owner's further rule) from `SellabilityV1` when it exists, normalizing its `satisfied`/`failed`/`not_evaluable` the same way and preserving its predicate/item/slot identity and `detail`/`owed_to`, and `unevaluable` until then. Transport failure is an operation outcome, not a fabricated predicate answer. A truncated roster or a missing required answer makes the response unusable (`catalog-predicates-unavailable`).

| Orders predicate | Check and refusal behavior |
|------------------|----------------------------|
| 1. Axis validity | All three tenants resolve; `axis-invalid` or owning input unavailability |
| 2. Contract | Only if referenced: active and payer party-eligible through Contracts; `contract-not-active`, `contract-party-ineligible` or `contract-resolution-unavailable` |
| 3. Quantity | Owning per-item quantity floors and selection bounds; `quantity-below-floor`; no local monetary arithmetic |
| 4. Market | Book currency and explicit producer market applicability match payer market; `market-inconsistent`; missing price scope is unevaluable |
| 5. References | Plan/revision/item references resolve without collision/duplication; `reference-unresolvable` / `reference-duplicated` |
| 6. Single currency | All lines share currency; reuse `currency-mixed` |
| 7. Subscription overlap | For each resolved key, `activeCount + proposed <= maxConcurrentActive`, preserving supported unbounded semantics; missing count/limit is `overlap-presence-unevaluable`, never default one; excess is `overlap-cardinality-exceeded` |
| 8. Required dates | Apply Capture's snapshot/cascade; reuse `date-cascade-invalid` |
| 9. In-flight order | No other order holds the key for the same payer and resource tenant across submitted, pending approval, approved, fulfillment or hold; advisory `order-in-flight-for-key`, enforced by Foundation's transactional claim constraint |

Evaluate every independent predicate even after a failure; a single failure blocks the entire order. Pin composition is always part of a completed assessment. Invalid returned pin uses `pin-unresolvable`; unavailable composition uses its port reason. An unresolvable reference records an unevaluable pin result carrying the same reference reason without adding a second failure.

### 3.2 Fix catalog scope and preserve commercial evidence

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-algo-gate-and-pin-fixed-catalog`

**Input**: Seller tenant, payer profile, basket and revision facts per line.

**Output**: Version-consistent catalog facts/pins, received total and stable overlap keys.

1. Read the revision facts through `PricingReadV1` (`current_revision`, `resolve`) as the configured `bss-orders.system` subject in the order's seller tenant (D-160). There is no caller-tenant fallback. PEP denial is unavailable with an operator-only diagnostic.
2. Distinguish a successful missing revision (`pricing-revision-absent`) from unavailable revision reads. Fix one revision per line and a common assessment date. Rating must use the exact composition result; no mid-run latest-price substitution.
3. Distinguish an answered missing product key (`overlap-key-unresolvable`) from operation outage. Use the same resolved key for occupancy, predicates, persisted lines and claim tuples.
4. Store only the catalog-written pin segment: assessment identity, revision per line, selected items and exact price/descriptor bindings, with the activation deadline. Exclude composed snapshot references, overlays, coupon, FX lock and commitment.
5. Persist evaluation's gross/net, explicit discount and promotion, separate recurring/usage/one-time components and computed net pre-tax TCV verbatim. Lifecycle performs no money arithmetic. Usage is flagged/excluded; tax and subscription-context overlays, including brand, are excluded and disclosed on surfaces.
6. Re-pin and re-evaluate on amendment. No refresh worker is introduced. The absolute acceptance deadline is independent of state TTLs; see DESIGN §4.3.

### 3.3 Bound upstream resolution

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-algo-gate-and-pin-port-budgets`

**Input**: One assessment run's required port operations.

**Output**: Resolved values or explicitly attributed unavailable/unevaluable outcomes within operation budgets.

| Port | Deadline per run |
|------|------------------|
| `PricingReadV1::current_revision`, `PricingReadV1::resolve`, `ProductsClient::get_sku`, Subscriptions SUB-G1 key | 250 ms each |
| Identity/profile, contract, overlap occupancy | 250 ms each |
| Price evaluation | 500 ms |
| Indicative tax, Preview only | 250 ms |

Use one logical batched port operation per run; bounded adapter splitting follows DESIGN §4.3. Identity precedes market use; revision facts precede assessment/composition; exact bindings precede Rating; authoritative keys precede occupancy. Retry transient failures at most two attempts inside the deadline, never retry a deadline. Use the design baseline breaker (0.5 failure ratio/30 seconds, open 10 seconds), bulkhead (32 concurrent calls/port), and caller limits (submit 10/minute, Preview 60/minute) through shared platform facilities. Unavailable required shared breaker capability remains a prerequisite.

The conservative resolution ceilings are 2.25 seconds for submit and 2.5 seconds for Preview. These are external-resolution baselines, separate from commit latency and PRD durable-write-plus-publication latency; none proves an end-to-end subsecond submit. Keep unratified budgets and latency reconciliation open.

### 3.4 Persist diagnostic identity and replay

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-algo-gate-and-pin-diagnostics`

**Input**: Completed authorized assessment, trusted caller/scope metadata and provisional run ID.

**Output**: Durable assessment identity/vector bound to the response, or infrastructure failure.

1. Retain every evaluated result, including passed outcomes, identified by run, line, predicate, resolve `item_id` and the slot's `dim_value` (token `default` for the null slot, D-159). Preserve distinct item/slot results without expanding order lines; reject duplicate identities or incomplete coverage.
2. Order by declared predicate order, then binary line ID, binary item ID and slot-key UTF-8 bytes, with NULL first. Persist trusted run metadata atomically with the complete vector.
3. For submit/amendment, use the engine transaction and bind `assessmentId=run_id` to its immutable settled response. Engine-only/state/version/date-basis refusals discard provisional stale results and carry no assessment ID.
4. Choose the first unavailable result as primary Problem if any is unevaluable; otherwise choose the first failed result, preserving the complete authorized report. Canonical reason/domain/code mapping remains Foundation's contract.
5. Preview commits a standalone diagnostic transaction; repeated calls create new runs. Operational lookup requires current diagnostic grants scoped by subject tenant; possession of an assessment ID is never authority. No public diagnostic endpoint is added.

## 4. States (CDSL)

### 4.1 Admission and assessment outcomes

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-state-gate-and-pin-admission`

**Initial State**: `draft` for submit; Preview has no order state.

| From / context | Condition | Result |
|----------------|-----------|--------|
| `draft` | All engine, structural, assessment, date-basis, capacity, binding-deadline and claim checks pass | `submitted`, appended version 2, atomic pins/totals/evidence/event |
| `draft` | Admission refused | Remain `draft`; no commercial admission; reached diagnostics settle as defined above |
| Amendment | Assessment passes | Versioning appends/re-pins and follows its declared transition; this feature adds no edge |
| Preview | Assessment completed | Diagnostic-only run, no order transition or pin persistence |

Each predicate outcome is exactly `passed`, `failed` or `unevaluable`. These are diagnostics, not new order states. Re-check outcomes `proceed`, `reject`, `not-dispatchable`, `defer` are Workflow control results, not persisted lifecycle states or admission guarantees.

## 5. Definitions of Done

### 5.1 Shared admission assessment

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-dod-gate-and-pin-assessment`

The system **MUST** implement one authorized, seller-scoped, revision-fixed assessment for submit, amendment and Preview, preserving tri-state diagnostics, every applicable predicate and independently evaluated pin outcomes under operation budgets. Predicate 7 **MUST** include the interim cross-order addend while the occupancy answer is per payer and **MUST NOT** be documented as an admission guarantee (D-180).

**Implements**: `cpt-cf-bss-orders-lifecycle-flow-gate-and-pin-submit`, `cpt-cf-bss-orders-lifecycle-algo-gate-and-pin-assessment`, `cpt-cf-bss-orders-lifecycle-algo-gate-and-pin-fixed-catalog`, `cpt-cf-bss-orders-lifecycle-algo-gate-and-pin-port-budgets`.

**Constraints**: `cpt-cf-bss-orders-lifecycle-constraint-port-budgets`, `cpt-cf-bss-orders-lifecycle-constraint-partial-predicate-evaluability`, `cpt-cf-bss-orders-lifecycle-constraint-overlap-read-unagreed`, `cpt-cf-bss-orders-lifecycle-constraint-overlap-key-partner-collision`.

**Touches**: submit API; `cpt-cf-bss-orders-lifecycle-interface-gate-ports`; `cpt-cf-bss-orders-lifecycle-dbtable-order-line`, `cpt-cf-bss-orders-lifecycle-dbtable-order-version`, `cpt-cf-bss-orders-lifecycle-dbtable-resolved-total` through engine contributions.

### 5.2 Preview and durable explanations

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-dod-gate-and-pin-preview-diagnostics`

The system **MUST** return authorized Preview fields with explicit exclusions and successful TCV withholding, persist only bounded diagnostic evidence for Preview, and bind every reached submit/amendment assessment to exact idempotent replay.

**Implements**: `cpt-cf-bss-orders-lifecycle-flow-gate-and-pin-preview`, `cpt-cf-bss-orders-lifecycle-algo-gate-and-pin-diagnostics`.

**Constraints**: `cpt-cf-bss-orders-lifecycle-constraint-total-excludes-subscription-overlays`.

**Touches**: Preview API; `cpt-cf-bss-orders-lifecycle-dbtable-gate-outcome`; engine idempotency response settlement; diagnostic retention and operational inspection.

### 5.3 Activation re-check integration

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-dod-gate-and-pin-activation-recheck`

Workflow integration **MUST** execute the four-outcome re-check with the declared retry budget, preserve held/superseded semantics and report actual collisions through the appropriate compensation path. Release evidence must establish Subscriptions' own atomic enforcement; a passing Lifecycle read cannot satisfy that prerequisite. **Release gate (D-180):** the submit/activation path **MUST NOT** be released to production until `cpt-cf-bss-orders-lifecycle-upreq-overlap-activation-atomicity` is agreed by Subscriptions and delivered; until then the gate contract and consumer documents state that subscription-side cardinality is advisory at order time.

**Implements**: `cpt-cf-bss-orders-lifecycle-flow-gate-and-pin-activation-recheck`.

**Constraints**: `cpt-cf-bss-orders-lifecycle-constraint-overlap-read-unagreed`.

**Touches**: Workflow integration, current authorized order read, owning identity/occupancy SDKs and Workflow-seam failure acknowledgement. No new Lifecycle endpoint.

The implementation MUST expose the [design §3.8](../DESIGN.md#contract-03-3-8) signals: per-port latency/timeouts/breaker state; refusals by predicate and origin; unevaluable and unresolvable-pin rates; incomplete occupancy answers; activation outcomes, retries and budget exhaustion; seller-catalog PEP denials; and Preview/submit budget utilization. Alerts cover breaker opening, the unevaluable threshold and any nonzero pin-unresolvable rate. Operational catalog-tenant diagnostics must not expose forbidden seller details to the buyer.

## 6. Acceptance Criteria

- [ ] A fully evaluable authorized basket admits one version with a resolvable pin per line and verbatim totals; a failed/unevaluable line prevents all commercial admission.
- [ ] Mixed failed/unevaluable predicates return all independent failures, persisted passed results and pin outcomes. An unresolvable reference creates no duplicate pin failure; primary Problem follows unavailable-first ordering.
- [ ] Revision absence/outage, overlap-key absence/outage, composition timeout and incomplete bundle component/key coverage produce distinct declared diagnostics and no pin/version admission.
- [ ] Partner callers use seller/revision inputs for every catalog-facing operation; denied catalog access yields unavailable diagnostics, never a buyer-facing 403. Revision or price publication mid-run does not mix the assessed bindings and totals.
- [ ] A 200-line basket invokes each basket-dependent port once per run; validate deadline, bounded retry, breaker, bulkhead and caller-rate behavior through shared facilities.
- [ ] Concurrent draft change, stale UTC basis and in-transaction claim collision preserve engine precedence, exact refusal evidence and zero commercial effects. Same-key replay performs no upstream calls and returns the same assessment/vector.
- [ ] Missing occupancy count/limit refuses unevaluable without assuming one; concurrent distinct submits cannot acquire one claim. One payer ordering one key for two resource tenants holds two claims; predicate 7 applies a per-payer occupancy answer as given and never re-buckets it (D-179), and while the answer is per payer it adds the lines of the payer's other in-flight orders claiming the key under another resource tenant, so a second customer's order submitted while the first is in flight is refused at cardinality one without naming the first (D-180).
- [ ] Preview authorizes every represented axis before resolving facts; unauthorized callers see no out-of-scope party details. Missing term/cycle returns successful `tcvWithheld`; tax/TCV/total never persist on an order or diagnostic row.
- [ ] Preview exposes expected fulfillment time, per-line deferral and total exclusions, with no approval verdict or quote-validity claim. Repeated identical previews have distinct run IDs and seven-day diagnostic retention.
- [ ] Distinct bundle component/key results survive persistence and replay; duplicate identities/incomplete coverage are rejected. Cross-tenant diagnostic lookup is denied and storage failure returns no claimed durable assessment.
- [ ] Re-check covers all four outcomes, holds/supersession during backoff, unavailable-port exhaustion and collision before/after active commit. No activation dispatch occurs during `defer`; no check is presented as atomic subscription enforcement.
- [ ] Report unresolved SDK, occupancy/activation, authorization, quote and staleness prerequisites explicitly. Measure resolution/commit/publication separately; baseline deadlines do not establish full PRD latency compliance.

- [ ] Seller-role-only callers cannot submit or Preview; independently held buyer grants must satisfy one complete authorization path.

- [ ] Fault-injection integration scenarios verify the per-port/predicate signals, missing occupancy fields, activation retry exhaustion and all three alert classes; public refusal responses remain sanitized.

## 7. Detailed Behavior Contracts

**Contract namespace 03.** Section numbers inside the detailed contracts below resolve
through the [contract address index](../DECOMPOSITION.md#4-contract-address-index),
not the overview section numbers above.

The following sections retain the complete algorithms, guards, state transitions and
feature-local rules. The preceding flows and acceptance criteria summarize these contracts;
shared schemas and interfaces are defined in [DESIGN.md](../DESIGN.md).

<a id="contract-03-3-6"></a>

<!-- contract:03-gate-and-pin:3.6 -->
### Gate and pin: Interactions and Sequences

<a id="contract-03-submit-through-the-gate"></a>

#### Submit through the gate

**Contract**: `cpt-cf-bss-orders-lifecycle-seq-gate-submit`, defined in [DESIGN §3.6 Feature sequences](../DESIGN.md#register-sequences).

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-new-acquisition`

**Actors**: `cpt-cf-bss-orders-lifecycle-actor-orders-partner-admin`, `cpt-cf-bss-orders-lifecycle-actor-orders-catalog-pricing`, `cpt-cf-bss-orders-lifecycle-actor-orders-idp-ams`, `cpt-cf-bss-orders-lifecycle-actor-orders-contracts`

**Algorithm: Run Gate and Submit**

Input: order_id, security_context, idempotency_key, expected_version, expected_draft_revision
Output: submitted order with pin and total, or a refusal listing every failure

1. [ ] - `p1` - Declare the at-least-one-line guard; after engine authorization/idempotency preparation, snapshot the authored basket with its server `prepared_draft_revision`, snapshot the resource tenant's effective `orders_date_policy` row (switches, scope, revision) and prepare all proposed dates under capture §4.2 before any date-dependent external call. Bind resolved results to this basket/date basis and carry both revisions into the engine; its locked revision check precedes using any result, including a gate refusal, and returns `version-conflict` for stale input - `inst-gs-declare-lines-guard`
2. [ ] - `p1` - Resolve the identity operation once under its deadline, including tenant-axis validity and the payer's commercial profile; derive the order market from that result before market-dependent calls. Party eligibility is not an identity answer: it belongs to the contract-resolution port alone - `inst-gs-derive-market`
3. [ ] - `p1` - Fix the assessment identity, UTC resolve date and explicitly seller-scoped revision per line through the revision SDK. Require each supplied revision to belong to its plan/seller. Do not choose a later revision implicitly. Missing revision is `pricing-revision-absent`; outage/denial/deadline is `pricing-revision-unavailable`. Retain independent diagnostic results when dependent inputs are unavailable - `inst-gs-fix-catalog-version`
4. [ ] - `p1` - Request each proposed line's SUB-G1 `catalogSubscriptionProductKey` from Subscriptions' key operation for every proposed line/revision under the same assessment in one logical batched request (proposed PriceBook derivation: the SKU of the line's paid `recurring` item(s), D-163); store the key(s) and provenance as answered and never compute them. `plan_id` is not a key. A missing key is `overlap-key-unresolvable`; unavailable owner contract is `overlap-key-unavailable`. All claims and occupancy checks reuse these exact keys - `inst-gs-resolve-overlap-key`
5. [ ] - `p1` - Resolve the following dependency graph under per-port shared deadlines; only independent branches run in parallel. Purchase assessment/composition precedes evaluation, key resolution precedes occupancy, and neither failed independent check suppresses another evaluable result - `inst-gs-parallel-resolve`
   - [ ] - `p1` - `PricingReadV1::current_revision` and `PricingReadV1::resolve` over each fixed revision (the roster, the chain matrix, SKU versions and descriptors), `ProductsClient::get_sku` for each consumed SKU, and, when it exists, `SellabilityV1` for the residual rows - `inst-gs-resolve-catalog`
   - [ ] - `p1` - reuse step 2's tenant-axis validity and payer-profile result without a second identity call - `inst-gs-resolve-identity`
   - [ ] - `p1` - contract status and party eligibility, only where a contract reference is present - `inst-gs-resolve-contract`
   - [ ] - `p1` - overlap occupancy `(activeCount, maxConcurrentActive, provenance)` for each distinct `(payer_tenant_id, resource_tenant_id, overlap_scope_key)` **as resolved in step 4**, applied on the tuple Subscriptions answers (D-179), in one batched call, skipped for a line without one; an answer missing `activeCount` or `maxConcurrentActive` is `overlap-presence-unevaluable` (D-126) - `inst-gs-resolve-overlap`
   - [ ] - `p1` - the resolved total and TCV from Rating over exactly the returned accepted bindings; this depends on composition and never runs against independently resolved prices - `inst-gs-resolve-total`
   - [ ] - `p1` - the accepted order pin for each line under [DESIGN §4.3](../DESIGN.md#contract-03-4-3), complete with descriptor provenance and absolute deadline; record its result even when an independent predicate fails - `inst-gs-compose-pin`
6. [ ] - `p1` - Retain step 1's policy snapshot, resolved cascade and proposed UTC date basis for engine validation under [02-capture — The date cascade (normative)](02-capture.md#contract-02-4-2); every date-dependent predicate/evaluation uses those same proposed dates - `inst-gs-resolve-cascade`
7. [ ] - `p1` - Initialize the complete per-predicate outcome vector and a separate empty failure list - `inst-gs-init-failures`
8. [ ] - `p1` - **FOR EACH** catalog row of §4.1 and each consumed item/slot: evaluate the six read rows from the `PricingReadV1` and `get_sku` answers, take the residual rows from `SellabilityV1` or record them `unevaluable`, and record every passed/failed/unevaluable result in the outcome vector and collect failures separately, preserving upstream detail - `inst-gs-collect-adopted`
9. [ ] - `p1` - **FOR EACH** of the nine delta predicates: evaluate, record its outcome including passed, and add any failure to the separate failure list; predicates 7 and 9 evaluate over step 4's resolved overlap keys - `inst-gs-collect-delta`
10. [ ] - `p1` - **FOR EACH** line: compose its `OrderPin` locally from the resolve answer (the matrix as answered, the consumed slot per item, the locally derived `activation_deadline`) and record the outcome in the vector. A resolve answer missing, truncated or deadline-exhausted contributes `catalog-pin-composition-unavailable`; an unset seller `max_acceptance_interval` contributes `order-binding-policy-missing`; an uncovered or absent consumed slot contributes `pin-unresolvable`. Where the line's reference is unresolvable (predicate 5), record the pin outcome as `unevaluable` carrying that same `reference-unresolvable` reason and add **no second failure**; without a valid revision input it is `unevaluable` with that input's reason - `inst-gs-collect-pin`
11. [ ] - `p1` - **IF** any other port was unresolvable: add its unevaluable reason (absence is a refusal); pin composition's was recorded at step 10 and is not added twice - `inst-gs-collect-unevaluable`
12. [ ] - `p1` - Predicate 8's `date-cascade-invalid` failure was recorded by step 9 and is not added again - `inst-gs-collect-cascade-invalid`
13. [ ] - `p1` - **IF** the failure list is non-empty: - `inst-gs-if-failures`
    1. [ ] - `p1` - Pass the complete outcome vector and failure set as the contribution so the engine persists the gate outcome, audits the refusal and settles the idempotency record in one transaction - `inst-gs-contribute-refusal-outcome`
    2. [ ] - `p1` - **RETURN** the engine's settled response, including every gate failure when gate guards were reached; engine authorization/admissibility/version checks retain precedence - `inst-gs-return-all-failures`
14. [ ] - `p1` - Assemble the resolved total rows and the TCV figure per §4.4 - `inst-gs-assemble-total`
15. [ ] - `p1` - Request the submit transition with both draft revisions, pin, total, market, proposed dates/date basis and policy snapshot, gate outcome, each line's resolved `overlap_scope_key` from step 4 (persisted on `orders_order_line`), the complete resolved `(payer_tenant_id, resource_tenant_id, overlap_scope_key)` claim set built from it and the **submitting principal as the initiating actor**. Before accepting date-dependent results, the engine checks the accepted-binding deadline and aggregate capacity, then checks defaults and assessment date against the UTC date of its single pre-write transition timestamp `t`; a changed date basis refuses with `date-cascade-invalid`. On admission it persists the validated dates and snapshot atomically with the version. The transition carries `05`'s automatic acceptance contribution only where [05-preconditions — Acceptance on the two paths (normative)](05-preconditions.md#contract-05-4-2)'s submit-request rule holds — the allowed submit carried no delegation proof reference and the submitting principal's subject tenant equals the order's `resourceTenantId` (D-146) — never on `orders_order.sales_path` alone; otherwise it carries none - `inst-gs-request-transition`
16. [ ] - `p1` - **RETURN** the submitted order - `inst-gs-return-submitted`

**Description**: Steps 7 through 13 are the all-failures contract, and pin composition is one of
its checks rather than a step reached only on success. Every input is resolved before
*Run Gate and Submit* step 15, so the transaction that commits `submitted` performs no network call and the pin lands
in the same commit as the state.

Verification includes missing revision, revision-read outage, a line with no authoritative overlap key,
overlap-key operation outage, composition timeout after passing
predicates, a failing predicate whose run still persists every line's pin outcome, an unresolvable
reference yielding one failure and an `unevaluable` pin outcome rather than two failures, a
seller-scoped revision read for a caller tenant that is not the seller (the seller's catalog is read, and a PEP
denial refuses `pricing-revision-unavailable`, never 403), mixed failed/unevaluable Pricing answers, and concurrent draft mutation during
resolution. Each refuses through the engine with the specified reason and no admitted pin or
version; a settled idempotent replay performs no upstream calls. Date-policy verification starts
from an authored draft with no versioned line row, changes configuration after the snapshot,
and checks that the admitted dates and stored policy use the same snapshot. A run crossing UTC
midnight with defaulted dates must refuse before consuming the old date-dependent results;
a fresh attempt must re-resolve them for the new day.

<a id="contract-03-preview-a-basket"></a>

#### Preview a basket

**Contract**: `cpt-cf-bss-orders-lifecycle-seq-gate-preview`, defined in [DESIGN §3.6 Feature sequences](../DESIGN.md#register-sequences).

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-new-acquisition`

**Actors**: `cpt-cf-bss-orders-lifecycle-actor-orders-partner-admin`, `cpt-cf-bss-orders-lifecycle-actor-orders-direct-customer`

```mermaid
sequenceDiagram
    participant B as Buyer surface
    participant P as Preview
    participant C as pricing / rating
    participant T as tax owner
    B ->> P: basket lines (term + cycle required for TCV)
    P ->> C: adopted predicates + evaluation
    C -->> P: per-line verdicts, resolved total
    P ->> T: indicative tax
    T -->> P: indicative amount
    P -->> B: verdicts, total, TCV (or tcvWithheld), indicative tax, expected fulfillment time, per-line deferral
```

**Description**: No order is created and no order is mutated; the indicative tax figure is never stored. The gate outcomes are persisted under their own bounded retention. Preview returns no
approval-requirement verdict — that belongs to the policy owner and is obtained by the sibling
gear — and it withholds TCV entirely when a line omits term or cycle, rather than reporting a
figure computed from an assumed term. Withholding is a **successful** response, not a refusal:
`tcv` is absent and `tcvWithheld: {reason: "preview-term-or-cycle-missing", lineIds: […]}` names
the lines that caused it, while every other field is returned (§4.6, D-125).

<a id="contract-03-re-check-before-first-activation"></a>

#### Re-check before first activation

**Contract**: `cpt-cf-bss-orders-lifecycle-seq-gate-fulfillment-recheck`, defined in [DESIGN §3.6 Feature sequences](../DESIGN.md#register-sequences).

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-fulfillment-complete`

**Actors**: `cpt-cf-bss-orders-lifecycle-actor-orders-workflow`, `cpt-cf-bss-orders-lifecycle-actor-orders-subscriptions`

**Algorithm: Re-check Activation Preconditions**

Integration algorithm executed by Workflow after begin-fulfillment commits, through the same
owning upstream SDKs as submit. It is not a Lifecycle endpoint or a begin-fulfillment guard.
Missing upstream SDK operations remain release gates; do not import gear internals.

Input: order_id, current_version, authenticated Workflow SecurityContext
Output: exactly one of `proceed` | `reject` (per-line reasons) | `not-dispatchable` (state or version) | `defer` (port reason) — [`../DECISIONS.md`](../DECISIONS.md) D-127

1. [ ] - `p1` - Read the PDP-authorized current order and its current version through the composed read of [08-read-and-authz — What a read exposes (normative)](../DESIGN.md#contract-08-4-2), which carries the fulfillment inputs steps 5 and 6 use — each line's stored `overlap_scope_key`, the version's `market_currency`, `market_region` and `payer_tenant_id` ([`../DECISIONS.md`](../DECISIONS.md) D-144) - `inst-rc-read-order`
2. [ ] - `p1` - **IF** the order is `on_hold`, is terminal, or its current version differs from `current_version` (superseded): - `inst-rc-if-not-dispatchable`
   1. [ ] - `p1` - **RETURN** `not-dispatchable` carrying the observed state or current version - `inst-rc-return-not-dispatchable`
3. [ ] - `p1` - Re-derive the order market from the payer's **current** commercial profile through the identity port under its §2.2 deadline - `inst-rc-rederive-market`
4. [ ] - `p1` - **IF** the identity port is unavailable or its deadline elapsed: - `inst-rc-if-identity-unavailable`
   1. [ ] - `p1` - **RETURN** `defer` carrying `identity-party-unavailable`; never fabricate a divergence - `inst-rc-return-defer-identity`
5. [ ] - `p1` - **IF** it diverges from the market frozen at submit: - `inst-rc-if-market-diverged`
   1. [ ] - `p1` - **RETURN** `reject` with market-divergence for the affected lines - `inst-rc-return-market-divergence`
6. [ ] - `p1` - Re-read overlap **occupancy** `(activeCount, maxConcurrentActive, provenance)` in one batched call for each line's overlap key **as stored on `orders_order_line.overlap_scope_key`** with the order's payer at submit/amendment and its resource tenant; do not re-resolve the product key from the registry, so the re-check tests the same key the claim holds - `inst-rc-reread-overlap`
7. [ ] - `p1` - **IF** the occupancy port is unavailable, its deadline elapsed, or an answer lacks `activeCount` or `maxConcurrentActive`: - `inst-rc-if-occupancy-unavailable`
   1. [ ] - `p1` - **RETURN** `defer` carrying `overlap-presence-unevaluable`; never default the limit to one and never fabricate a collision - `inst-rc-return-defer-occupancy`
8. [ ] - `p1` - **IF** for any key `activeCount + pending > maxConcurrentActive`, where `pending` is the number of this order's lines carrying the key that are not yet activated (all of them, before the first activation): - `inst-rc-if-overlap-collision`
   1. [ ] - `p1` - **RETURN** `reject` with overlap-collision for the affected lines - `inst-rc-return-overlap-collision`
9. [ ] - `p1` - Check each stored accepted-binding deadline against the live Workflow clock; if elapsed return `reject(order-binding-expired)`. Pass the exact version reference to Subscriptions, which resolves with the accepted consumed slots as pins on the activation date and compares the answer with the accepted `chains[]` before committing `active` under DESIGN §4.3 (D-162). No current-price resolution occurs here. **RETURN** `proceed`, which is an **early-abort pass and not an admission guarantee**: the caller **MUST NOT** treat it as one, and **MUST** be able to handle an `overlap-collision` raised later by Subscriptions on the failure-acknowledgement path of [06-workflow-seam — Acknowledgement (normative)](06-workflow-seam.md#contract-06-4-4) - `inst-rc-return-proceed`

**What Workflow does with each outcome** (normative; the transitions are those of
[06-workflow-seam — Begin fulfillment and the spawn signal (normative)](06-workflow-seam.md#contract-06-4-3) and §4.4):

| Outcome | Workflow's next action | Transition driven |
|---------|------------------------|-------------------|
| `proceed` | Request `report-spawn-signal`, then dispatch the activation wave | `spawn-signal` ([01 §4.3](01-foundation.md#contract-01-4-3) row 12) |
| `reject` | Stop dispatch, void the wave-1 drafts, and acknowledge failure carrying each line's reason with compensation evidence recording the voided drafts | `acknowledge-failed` (row 14) |
| `not-dispatchable` | Stop dispatch and **do not** call acknowledge; re-read the order. `on_hold`: wait for resume (row 22), then re-run the re-check from step 1 against the resumed version. Superseded: abandon this version's attempt and follow the amendment seam for the current version. Terminal: end the attempt — the transition that made it terminal carried its own compensation evidence | none |
| `defer` | Retry the re-check from step 1 with bounded exponential backoff under **`activation-recheck-retry-budget`**; no spawn signal and no dispatch meanwhile. A retry that returns any other outcome is handled by its row. Once the budget is exhausted, void the wave-1 drafts and acknowledge failure carrying the port's unevaluable reason (`identity-party-unavailable` or `overlap-presence-unevaluable`) | none while retrying; `acknowledge-failed` (row 14) on exhaustion |

**`activation-recheck-retry-budget`** is a named deployment value owned by this slice and executed
by Workflow: the maximum number of re-check attempts and the wall-clock bound across them,
**baseline 3 attempts over ≤ 60 s**. Neither this set nor `UPSTREAM_REQS.md §2.6` defines an
existing Workflow retry budget to reuse. Because every retry restarts at step 1, a hold or a
supersession during backoff surfaces as `not-dispatchable` rather than being overtaken by the
exhaustion path. Rejected alternative: aborting on the first port unavailability — it turns a
transient upstream blip after a successful submit into a failed order (D-127).

**Description**: Both predicates read state owned elsewhere that can move between submit and
activation, which is why passing the gate is necessary but not sufficient. The sibling gear
invokes this immediately before dispatching the first activation intent and treats either
rejection as a pre-activation abort rather than a line-execution failure.

**Why this algorithm cannot be made atomic here, and what bounds it instead.** Step 6 is a read and
the activation it guards is a later call into another gear, so the check and the action it guards
are separated by construction — two waves passing step 6 concurrently can both proceed and together
exceed `maxConcurrentActive`. The re-check therefore **MUST NOT** be implemented or relied on as
the enforcement point. What closes each axis, and what merely bounds it, is stated normatively in
`§2.2` *The activation re-check is a bounding device, not the enforcement point*: the **order** axis
is closed in-transaction by [01 §3.7](../DESIGN.md#contract-01-3-7)'s claim index; the **subscription** axis **MUST** be closed by
Subscriptions inside the transaction that commits `active`; and until it is, **this design does not
bound the gap** — §2.2 states why no timed validity window is asserted, and what a closable
server-side form would need. What remains normative here is that a collision surfaces as
`overlap-collision` through failure acknowledgement rather than as an over-provision nobody
refused. A `proceed` is advisory at order time; it is not an admission guarantee, and the
submit/activation path is not production-ready until `…-upreq-overlap-activation-atomicity` is
agreed and delivered (D-180). `pending` in step 8 stays this order's lines: predicate 7's interim
cross-order addend has already refused a sibling submitted after this order, and a concurrently
submitted pair is part of the open axis, not something a second read here could close.


<!-- /contract -->

<a id="contract-03-4-2"></a>

<!-- contract:03-gate-and-pin:4.2 -->
### Gate and pin: The Orders delta (normative)

Nine predicates are this gear's own. Each **MUST** carry its own machine-readable reason:

1. **Axis validity** — all three tenant axes resolve against IdP/Account Management.
2. **Contract active** — where a contract reference is present it resolves to an **active** contract (`contract-not-active`) under which the payer is party-eligible (`contract-party-ineligible`). Party-eligibility policy is owned by Contracts and consulted only when a contract is referenced, through the contract-resolution port alone — never the identity port; its unevaluability there is a refusal (`contract-resolution-unavailable`). This is a predicate of its own because it carries its own registered reason, and folding it into axis validity was what made the predicate count and the reason count disagree.
3. **Purchase-quantity floor** — each selected item quantity satisfies the owning item's `qty_min` and applicable selection rule; since Pricing D-467 resolve carries no `qty_min`, so the floor reduces to a positive exact-decimal quantity (D-170). No implicit line multiplier or legacy one-time-plan bound is assumed. Resource quota ceilings remain a fulfillment concern.
4. **Order-market consistency** — compare the book currency and producer-declared market applicability with the payer profile. An arbitrary selected dimension is not inherently region. Market applicability is a residual owner row (DESIGN §4.1): while its owner is missing the predicate is `unevaluable` with `catalog-predicate-unevaluable`; preserve the producer mapping and policy identity once answered (D-156, D-161).
5. **Reference resolution** — every plan/revision/item reference and selected binding resolves, with no collision or duplication across lines.
6. **Single currency** — all lines share one currency. Authoring already refuses a mixed basket; this predicate is the backstop for a basket assembled before the rule existed.
7. **Overlap uniqueness** — projected activation does not violate the configured concurrent-active cardinality per `overlapScopeKey`, evaluated **within the basket** and **against existing subscriptions**.

   The input is the **occupancy read** of `§3.3` (`SUB-O5`, amended): per
   `(payer_tenant_id, resource_tenant_id, overlap_scope_key)` the batched owning Subscriptions contract returns
   `(activeCount, maxConcurrentActive, provenance)`, the limit resolved using its Catalog/Contract
   policy. The tuple matches predicate 9's claim (D-179). Until Subscriptions enforces the resource
   dimension it answers on the tuple it enforces and says so in `provenance`; this predicate applies
   that answer as given and never re-buckets a per-payer count locally. For each key, `proposed` is
   the number of basket lines carrying it **plus**, while `provenance` states an answer coarser than
   the claim tuple (per payer), the lines carrying the same key on the current version of every
   **other** in-flight order of this payer holding a live `orders_inflight_overlap_claim` on
   `(payer_tenant_id, overlap_scope_key)` under a different resource tenant (D-180). The addend is
   Orders-owned claim data, not a re-bucketing of Subscriptions' count; it is dropped once the
   answer is per the full tuple, and the refusal evidence never names those orders or their count
   to the caller (D-179). It is read outside the transition transaction, so two concurrent submits
   can both miss each other — it narrows the race, it does not close it. The predicate
   passes iff `activeCount + proposed ≤ maxConcurrentActive`, preserving declared unbounded
   semantics if supported. A boolean presence read is insufficient, which is why the port is not
   one (D-126). A missing `activeCount` or `maxConcurrentActive` refuses
   `overlap-presence-unevaluable` — the name is kept for stability — and never silently defaults
   the limit to one. Subscription-side cardinality is **advisory at order time** (D-180): a pass is
   a pre-check, **not** an admission guarantee, and this read is not a reservation. The owning
   activation commit is the enforcement point, owed by `…-upreq-overlap-activation-atomicity`, and
   Workflow **MUST** handle `overlap-collision` on the failure-acknowledgement path (D-89).
8. **Required line dates resolvable** — evaluate authored values and the run's snapshot of the resource tenant's effective `orders_date_policy` row, never a future version row. A policy-required service-activation or acceptance-due date must be authored — a cascade default never satisfies the requirement (capture §4.2, D-60); every other field resolves to its authored value or its default, including proposed transition-date defaults prepared before dependent external calls under capture §4.2. The engine checks that those defaults equal the UTC date of its pre-write transition timestamp `t`, then stores the validated dates and identical snapshot with the admitted version. An invalid cascade or stale date basis refuses with `date-cascade-invalid`; an amendment takes a new policy snapshot while retaining carried-forward date values unless its delta changes them.
9. **One in-flight order per overlap key and resource tenant** — **no other order** in the in-flight set holds the same key for the same payer and resource tenant (D-179); a partner's orders for different customers never collide here. The exclusion of the requesting order itself is load-bearing: an amendment is issued by an order that is *already* in-flight and already holds its key, so a predicate counting all holders without excluding the subject refuses every amendment against itself. Predicate 7 bounds concurrent **subscriptions** and is configurable via `maxConcurrentActive`; this one bounds concurrent **orders** and PRD §6.1(g) fixes it at one with no configurability clause. The two are deliberately separate rules and **MUST NOT** be given a shared cardinality (D-83). **The in-flight set is `submitted`, `pending_approval`, `approved`, `in_fulfillment` and `on_hold`** — `on_hold` is included because a held order resumes onto its pre-hold state and still holds its key, so excluding it would admit a second order that collides at the activation re-check, the expensive path §2.2 refuses to defer failures into. The partial unique index in [01-foundation — Database Schemas and Tables](../DESIGN.md#contract-01-3-7) covers exactly those five states. Idempotency keys protect against a repeated call; this protects against **more distinct orders on one key than the key permits**. The key resolved at `§3.6` *Run Gate and Submit* step 4 is **persisted** on the line and the rule is enforced by an `orders_inflight_overlap_claim`
**partial unique index over `(payer_tenant_id, resource_tenant_id, overlap_scope_key)` inside the transition
transaction**; this predicate is the friendly pre-check,
not the enforcement, because a predicate resolved outside the transaction would let two concurrent
identical submits both pass. The transaction creates claims on submit or amendment and releases
them only on a terminal transition ([`../DECISIONS.md`](../DECISIONS.md) D-26).

**All failures are reported together.** The gate **MUST** evaluate every predicate it can and
return every failure in one response, rather than short-circuiting on the first. The rejected
alternative was short-circuit evaluation; it was rejected because a five-line basket with three
independent problems would otherwise require three round trips to discover, and because the
predicates are resolved in parallel anyway, so the marginal cost of completing evaluation is
near zero. A single failure still refuses the whole order — reporting is exhaustive, admission
is not partial.


<!-- /contract -->

<a id="contract-03-4-6"></a>

<!-- contract:03-gate-and-pin:4.6 -->
### Gate and pin: Preview (normative)

Preview **MUST** create no order and mutate no order. It **does** persist its per-predicate gate
outcomes to `orders_gate_outcome` with a **7-day retention** and a rate limit, because "why was
this basket refused last Tuesday" is a support question worth answering and an unbounded
write path open to every PDP-authorized buyer is not ([`../DECISIONS.md`](../DECISIONS.md) D-52). It **MUST** return
per-line gate results, the
resolved total including the named TCV figure, an **indicative** tax amount per line and in
total sourced from the tax owner, and — when line service-activation dates differ — expected
fulfillment time plus a per-line deferral where a quoted date is earlier.

**Preview uses the same axis-specific authorization as purchase.** Acting for a foreign
`payerTenantId` or `resourceTenantId` requires PDP-validated authority/delegation for that axis
before upstream facts are resolved. `sellerTenantId` selects the permitted seller/catalog
relationship; selecting a different seller does not mean representing that seller. Preview
reads that seller's revision and purchase facts exactly as submit does, through operations
called as the `bss-orders.system` subject in that tenant (D-160); a PEP denial on those reads is `pricing-revision-unavailable`
(or the port's own unavailable reason) with an operator diagnostic, never a 403 to the buyer
([`../DECISIONS.md`](../DECISIONS.md) D-122). A direct
customer T may preview `resource=T, payer=T, seller=S` when that selling relationship is
authorized, without S delegating its identity to T. `resource=T, payer=U, seller=S` requires
authority to act for U and refuses without it. A seller-only role grants no Preview permission;
any independent buyer permission must satisfy the same complete authorization path in
[08-read-and-authz — The permission model (normative)](../DESIGN.md#contract-08-4-3).

Four prohibitions are absolute. The indicative tax **MUST NOT** be stored on any order. Preview
**MUST NOT** return an approval-requirement verdict. Preview **MUST NOT** return a TCV figure
when a basket line omits term duration or billing cycle — the figure is undefined without them,
and returning one computed from an assumed term would be a worse answer than none. Withholding it
is **not a refusal**: the response is successful, carries every other field, omits `tcv`, and
carries `tcvWithheld: {reason: "preview-term-or-cycle-missing", lineIds: […]}` listing every line
that omits either input. The name is a response annotation registered in
[01-foundation — GTS types for the cross-gear contract surface (normative)](../DESIGN.md#contract-01-4-7)'s non-refusal list, never an error variant
([`../DECISIONS.md`](../DECISIONS.md) D-125). And Preview
**MUST NOT** return, in a gate result, reason detail or total, any fact about a party or
relationship outside the caller's PDP-authorized assessment scope.

**Preview is not a quote, and this gear has no quote.** The distinction is worth stating plainly,
because PRD §1.1 says the order "does double duty as **quote and order**" with "validity/expiry
[as] the per-state TTL", and no state in this design delivers what a quote is commercially — a
**priced, non-binding, time-bounded offer**. Three facts make that so, and each is a rule stated
elsewhere in this set rather than an oversight here:

* A `draft` carries **no price**. It "resolves no catalog reference, captures no pin, computes no total" ([02-capture — Component Model](../DESIGN.md#contract-02-3-2)), so the pre-commitment state is unpriced.
* Submit is where the price appears — and on the self-service path **submit *is* the commitment** ([05-preconditions — Acceptance on the two paths (normative)](05-preconditions.md#contract-05-4-2)), so the priced state is not an offer.
* Preview prices a basket but **persists only its per-predicate verdicts** to `orders_gate_outcome` (§3.7). The resolved total, the TCV and the indicative tax are returned and **not stored**, so the number Preview quoted is not recoverable afterwards, is bound for no period, and no order references it.

The consequence is operational rather than theoretical: a partner-led sale that needs "here is your
price, valid for thirty days" must hold that figure **outside** this system of record, with its
validity unenforced — which is the outcome PRD §1.1 gives as the reason a separate quote artifact
is unnecessary. This slice **MUST NOT** close the gap locally by storing Preview's total and
calling it an offer: an offer needs a validity rule, an expiry actor, a re-price rule on expiry and
a binding-on-acceptance rule, none of which any document in this set carries. The reconciliation —
amend §1.1 to stop claiming quote coverage, or specify a priced offer artifact with a validity
bound — is routed as [`../DECISIONS.md`](../DECISIONS.md) **Q-29**.


<!-- /contract -->

<a id="contract-03-5"></a>

<!-- contract:03-gate-and-pin:5 -->
### Gate and pin: Traceability

- **PRD**: [`../PRD.md`](../PRD.md) — §6.1 submit gate and pin, §9.1 Preview
- **Gear design**: [`../DESIGN.md`](../DESIGN.md) — realises `cpt-cf-bss-orders-lifecycle-component-gate-and-pin`
- **Engine**: [`01-foundation`](../DESIGN.md#contract-01-1-1) — transition contract, reason registry, resolved-total schema
- **Producers**: [`02-capture`](../DESIGN.md#contract-02-1-1) authors the lines and the policy-switch state this gate reads
- **Consumers**: [`04-versioning`](../DESIGN.md#contract-04-1-1) re-runs this gate on amendment; [`05-preconditions`](../DESIGN.md#contract-05-1-1) contributes the self-service acceptance instant to the submit transition this slice owns; [`06-workflow-seam`](../DESIGN.md#contract-06-1-1) consumes the activation re-check outcome (specified by 03, executed by Workflow)
- **Upstream asks**: `SUB-O5` overlap occupancy (amended from presence, D-126); `cpt-cf-bss-orders-lifecycle-upreq-catalog-subscription-product-key` (legacy requirement ID for authoritative overlap derivation, D-153); `cpt-cf-bss-orders-lifecycle-upreq-pricing-read-sdk` (`PricingReadV1` over the existing reads, D-161); `cpt-cf-bss-orders-lifecycle-upreq-pricing-catalog-tenant-reads` (the `bss-orders.system` grant, D-160); `cpt-cf-bss-orders-lifecycle-upreq-pricing-purchase-assessment` (the residual market-applicability verdict); `cpt-cf-bss-orders-lifecycle-upreq-rating-evaluation`; `cpt-cf-bss-orders-lifecycle-upreq-payer-commercial-profile`; `cpt-cf-bss-orders-lifecycle-upreq-contract-party-eligibility`; `cpt-cf-bss-orders-lifecycle-upreq-contract-acceptance-declaration` (the port's `acceptance_required` output, read by `05`, D-132); the overlap **dimension** binding for the partner path
- **ADRs**: [`ADR/0001`](../ADR/0001-cpt-cf-bss-orders-lifecycle-adr-transition-through-engine.md) transition through the engine; [`ADR/0002`](../ADR/0002-cpt-cf-bss-orders-lifecycle-adr-slice-decomposition.md) the foundation-plus-seven-slices decomposition; [`ADR/0003`](../ADR/0003-cpt-cf-bss-orders-lifecycle-adr-fail-closed-gate.md) fail closed on an unevaluable gate input; [`ADR/0007`](../ADR/0007-cpt-cf-bss-orders-lifecycle-adr-in-transaction-concurrency.md) concurrency enforced by the in-transaction overlap constraint behind predicate 9

<!-- /contract -->
