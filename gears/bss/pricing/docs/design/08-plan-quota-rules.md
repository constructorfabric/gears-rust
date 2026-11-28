<!-- CONFLUENCE_TITLE: [BSS]: Pricing — Plan Quota Rules (Design, Slice 8) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../DECISIONS.md | Owners: BSS Pricing team -->

# DESIGN — Plan Quota Rules (Slice 8)

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-design-slice-08`

<!-- toc -->

- [1. Context](#1-context)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Author, check and publish quota rules](#author-check-and-publish-quota-rules)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [replace](#replace)
  - [govern](#govern)
  - [accept](#accept)
- [4. States (CDSL)](#4-states-cdsl)
- [5. API Surface](#5-api-surface)
- [6. Data Model](#6-data-model)
- [7. Events & Alarms](#7-events--alarms)
- [8. Definitions of Done](#8-definitions-of-done)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Non-Functional Considerations](#10-non-functional-considerations)

<!-- /toc -->

## 1. Context

**Delivery:** the quotas programme, after plans (slice 04) and the read contract (slice 07). Every checkbox is an
implementation obligation, not an assertion about the code.

A draft plan revision carries allocation quota rules. Each rule selects one exact Products quota definition version and
states an amount in an allowed unit, a basis, a logical binding and a scope selection, for one item of the revision. The
author replaces the complete set at once, through REST or the SDK's `PricingQuotaAuthoringClientV1`, which answer alike;
Products validates it outside the transaction; the checks, submit, approval, copy and clone carry and recheck it; the
preview shows the selected quota terms with a digest, and an acceptance freezes them with the price terms. This slice is
Pricing's part of the quotas package's feature 2 (`cpt-cf-bss-quotas-feature-plan-rules`) and of its DESIGN
§3.3.1.1–§3.3.1.3. No quota is provisioned and nothing is sellable before qualification: every Products definition stays
`disabled` until the package's feature 5 (products P-D-271).

Requirements: `cpt-cf-bss-pricing-fr-plan-quota-rules`, `cpt-cf-bss-pricing-nfr-authz`,
`cpt-cf-bss-pricing-nfr-tenant-isolation`, `cpt-cf-bss-pricing-nfr-two-backends`,
`cpt-cf-bss-pricing-nfr-idempotency-concurrency`. Architecture: `cpt-cf-bss-pricing-component-plans`,
`cpt-cf-bss-pricing-component-approvals`, `cpt-cf-bss-pricing-component-read-contract`,
`cpt-cf-bss-pricing-constraint-two-backends`, `cpt-cf-bss-pricing-constraint-no-row-locks`,
`cpt-cf-bss-pricing-constraint-one-replay-store`, `cpt-cf-bss-pricing-seq-blocked-revision`.
[FEATURE](../features/plan-quota-rules.md) owns the executable flow, algorithm and DoD identifiers; this slice
defines no duplicate DoDs.
Dependencies: `cpt-cf-bss-pricing-feature-plans`, `cpt-cf-bss-pricing-feature-read-contract-events`; Products'
`ProductsQuotaCatalogClientV1` (products P-D-272, P-D-275).
Source: [DECISIONS](../DECISIONS.md) D-540 … D-548; the quotas package's
[feature 2](../../../docs/quotas/features/0002-plan-rules.md) and [DESIGN](../../../docs/quotas/DESIGN.md)
§3.3.1.1–§3.3.1.3.

Pricing calls Products for exactly two things: each rule item's SKU version on the revision's sale date
(`ReferenceRegistryV1::sku_version_as_of`, one read per distinct SKU) and `validate_quota_set`, whose answer carries
each resolved definition. Both run as the caller, outside any Pricing transaction and on a task of their own, as
D-503 reads meters. Products answers facts; Pricing decides what each boundary refuses (D-542).

## 2. Actor Flows (CDSL)

### Author, check and publish quota rules

Actors: `cpt-cf-bss-pricing-actor-product-manager`, `cpt-cf-bss-pricing-actor-products`,
`cpt-cf-bss-pricing-actor-subscriptions`. Feature flow: `cpt-cf-bss-pricing-flow-plan-quota-rules`.

1. [ ] - `p1` - Product Manager reads a draft revision with its items and its quota rules, with the revision's ETag; ask `read` on `plan` - `inst-quota-read`
2. [ ] - `p1` - Product Manager selects exact definitions from Products' catalog and puts the complete rule set under the revision's ETag and an idempotency key, by algorithm replace - `inst-quota-replace`
3. [ ] - `p1` - Product Manager reads the revision's checks: one red row per failing quota code, each naming the item, the rule and the field, or the item, the sellable cell and the metric that no rule covers, with the evaluated revision version and digest (D-544) - `inst-quota-check`
4. [ ] - `p1` - Submit and every vote recheck the rules by algorithm govern; a red quota row refuses, and an approved revision publishes its rules unchanged - `inst-quota-submit`
5. [ ] - `p1` - Copying a published revision or cloning its plan copies its rules onto the copied items by algorithm govern - `inst-quota-copy`
6. [ ] - `p1` - A buyer's preview (resolve and `GET /resolve`) carries the revision's quota terms; the buyer computes the selected quota digest for its selection with the SDK's `selected_quota_digest`, the selection of algorithm accept - `inst-quota-preview`
7. [ ] - `p1` - Acceptance checks the expected quota digest and freezes the selected rules in the receipt by algorithm accept - `inst-quota-accept`

## 3. Processes / Business Logic (CDSL)

### replace

Feature algorithm: `cpt-cf-bss-pricing-algo-plan-quota-rules-replace`.

1. [ ] - `p1` - Authenticate and ask `author` on `plan` for the caller's tenant before any header or body is read; when no stored answer replays (step 2), the revision must be the caller's own unlocked draft (`NOT_DRAFT_AUTHOR`, `REVISION_NOT_DRAFT`) - `inst-quota-authorize`
2. [ ] - `p1` - Look up the key's stored answer with the request digest of the `If-Match` value and the rules as they parse (the body itself when it does not parse); the same content answers its stored body and ETag even when the ETag moved since, other content is 409 IDEMPOTENCY_CONFLICT - `inst-quota-replay`
3. [ ] - `p1` - Parse the complete set: no unknown member or null collection, at most 200 rules, unique rule ids, canonical integer amounts, the binding-key and scope-key rule, the text bounds, the basis and subject tokens; each failure names `quota_rules[i].<field>` - `inst-quota-parse`
4. [ ] - `p1` - Outside any transaction, read each rule item's SKU version on the sale date and validate the set through Products' `validate_quota_set`; add the plan's own checks (item, dimension value, SKU version, duplicates); refuse every known failure but availability at once, changing nothing - `inst-quota-validate`
5. [ ] - `p1` - In the transaction, claim the key, then require the unlocked draft at the `If-Match` version (else 409 STALE_REVISION) and the items and sale date the validation saw, each rule's item among them (else capture again) - `inst-quota-fence`
6. [ ] - `p1` - Write the rules with their evidence and availability observation, the set row with its digest, the next revision version, the audit row and the key's answer together; answer 200 with the saved set and the new ETag - `inst-quota-commit`

### govern

Feature algorithm: `cpt-cf-bss-pricing-algo-plan-quota-rules-govern`.

1. [ ] - `p1` - The revision's approval content carries its rules and their digest; a different set is a different content (D-545) - `inst-quota-fingerprint`
2. [ ] - `p1` - Submit and every vote capture the quota validation before their transaction and re-check the captured selection inside it; a red quota row refuses submit and apply; a scheduled switch makes no catalog call - `inst-quota-recheck`
3. [ ] - `p1` - Deleting an item that a rule names is 409 ITEM_HAS_QUOTA_RULES; deleting a draft removes its rules first - `inst-quota-item-delete`
4. [ ] - `p1` - A copy or a clone writes each rule with its copied item, its rule id, definition reference, evidence and observation, and the digest computed again - `inst-quota-remap`

### accept

Feature algorithm: `cpt-cf-bss-pricing-algo-plan-quota-rules-accept`.

1. [ ] - `p1` - Resolve answers every rule of the revision with its pinned evidence and the set digest, read from the stored rules with no catalog call, or none for a revision without rules - `inst-quota-terms`
2. [ ] - `p1` - Before the selection, a revision with rules needs the sale's expected quota digest and a revision without rules takes none, else RESOLUTION_CHANGED; after the selection (step 3), the expected digest must equal the selected digest, else RESOLUTION_CHANGED - `inst-quota-expected`
3. [ ] - `p1` - A quota is a metric: for each metric of a selected item's rules, select every rule of the item for the requested value and that metric, else every default rule of the item for that metric; a metric with neither is QUOTA_RULE_MISSING, whatever the expected digest and before any catalog call; the price's default chain plays no part - `inst-quota-select`
4. [ ] - `p1` - A sale that selects rules needs a whole quantity from 1 (QUANTITY_INVALID); outside the transaction, validate the selected rules through Products for acceptance at that quantity, the first refusal that applies answering (D-548); freeze each rule with its evidence, final limit and availability revision, the original quantity and the selected and snapshot digests, and check the snapshot with `verify_quota_snapshot`; the acceptance transaction writes the snapshot in the receipt, or rolls back and captures again when the rule set moved - `inst-quota-freeze`

## 4. States (CDSL)

No new state. A rule set follows its revision: it is edited only while the revision is an unlocked draft, it is
locked with the revision at submit, and it is published, scheduled or superseded with it (slice 04).

## 5. API Surface

Under /bss-pricing/v1:

- `GET /plan-revisions/{id}/quota-rules` (plan read) answers `{ revision_id, revision_version, quota_rules,
  quota_rules_digest }` with the revision's ETag. Each rule carries the definition's availability that it saw at its
  save; the checks judge the current one. An unknown or foreign revision is 404. A stored read asks Products nothing,
  and it reads the set row and the rules in one serializable read transaction.
- `PUT /plan-revisions/{id}/quota-rules` (plan author, the draft's author, `If-Match`, `Idempotency-Key`) takes
  `{ quota_rules: [...] }` with at most 200 rules and replaces the complete set; it answers 200 with the saved set
  and the new ETag. The order of its answers and refusals is D-543's: 400 without `If-Match` or `Idempotency-Key`,
  or for an empty or unreadable body; the stored answer of the key, which is bound to the `If-Match` version and the
  rules as they parse, whatever their spelling (other content under the key is 409 `IDEMPOTENCY_CONFLICT`, a key
  still in flight 409 `IDEMPOTENCY_KEY_IN_FLIGHT`); then the revision; then one refusal that names every failure as
  `quota_rules[i].<field>` with its code, a failure of the whole set on `quota_rules` (409 when every failure is
  `QUOTA_RULE_DUPLICATE` or `QUOTA_DEFINITION_CHANGED`, else 400; either one an invalid-argument problem whose field
  violations carry the codes, with no `reason`); Products' 400, 403 or 500 as Products gave it; then 409
  `STALE_REVISION` in the transaction, 409 `UNIT_CONTENDED` when the items keep moving and 409 `CONTENDED` when a
  transaction stays contended past its retries; 503 `QUOTA_CATALOG_UNAVAILABLE` (every catalog outage, a 429 and a
  contention conflict included) or `REGISTRY_UNAVAILABLE`. An empty array clears the set and makes no SKU or catalog
  call. `QUOTA_QUALIFICATION_UNAVAILABLE` never refuses a PUT (D-542).
- `GET /plan-revisions/{id}` carries `quota_rules` and `quota_rules_digest` (empty and the empty set's digest for a
  revision without rules).
- `GET /plan-revisions/{id}/checks` and `GET /plan-revisions/checks` carry `revision_version` and
  `quota_rules_digest`, and for a revision with rules the nine quota rows of D-544 after the existing rows; each
  subject of a quota row that names a rule adds `quota_rule_id` and `field` (`""` for the rule as a whole), left out
  when null, and the line of an uncovered sellable cell names only its item. The batch reads each SKU's version once
  per sale date.
- `DELETE /plan-items/{id}` adds 409 `ITEM_HAS_QUOTA_RULES`.
- `bss_pricing_sdk::plan_quota::PricingQuotaAuthoringClientV1`, whose local client the gear registers in the ClientHub
  at init: `get_quota_rules`, `replace_quota_rules` and `check_plan_revision` call the same functions as the GET, the
  PUT and `GET /plan-revisions/{id}/checks`, and answer the same request with the same body, the same refusals and the
  same ETag (D-540). `check_plan_revision` answers the complete saved revision's checks, quota rows included, and
  `replace_quota_rules` shares the PUT's key space.
- On the SDK (D-547, D-548): `ResolvedBindings.quota_terms`, `NewSaleQuery.expected_quota_digest` and
  `AcceptanceReceipt.quota`, each `None` for a plan without rules; `GET /resolve` answers `quota_terms` too, left out
  for a revision without rules. `bss_pricing_sdk::plan_quota` holds the rule's v1 shape (D-541).

Products' `validate_quota_set` asks `read` on `quota_definition` for the caller's tenant. These callers need that grant
(D-542): the authors who save rules, the readers of the checks (single and batch), the submitter, the final approver
(apply re-runs the checks) and the caller of an acceptance (Orders' principal). A non-final vote keeps a refusal as an
observation (D-503) and needs no grant. [DESIGN §3.3](../DESIGN.md#33-api-contracts) fixes canonical errors and route
prefixes. Each mounted route appears in all four censuses with its authz and precondition expectations.

## 6. Data Model

Migration `m20261121_000024_plan_quota_rules` (D-546) adds `pricing_plan_quota_set` (one row per revision that holds
rules: `revision_id`, the digest, `rule_count` 1 to 200, the last writer and time) and `pricing_plan_quota_rule` (key
`(revision_id, rule_id)`; foreign keys to the revision and to its item, with no cascade; the definition's id, version
and digest, the dimension value, the binding key, the subject selector, the scope selection, the amount, the unit, the
basis, the definition's evidence and the availability observation as JSON text; an index on `item_id`).
[DESIGN §3.7](../DESIGN.md#37-database-schemas--tables) gives the DDL. A replace with no rules deletes the set row, so a
revision without rules has none and reads the empty set. The set row and the rules are read in one serializable
transaction, as every Pricing transaction is, so both statements see one snapshot.

The set digest is computed once, at the write, and stored (D-541). The evidence keeps the identity-bearing content of
the definition, without text and availability; the availability observation is stored apart from it (D-542). With the
rule's unit it keeps that unit's grid and maximum, and it keeps the quota kind; the preview's terms carry both (D-547).

## 7. Events & Alarms

None new. `PlanRevisionPublished` gains no member: a consumer reads the revision, which carries its rules (D-545). A
rule write is audited as `plan_revision.quota_rules` in the PUT's transaction; copy and clone keep their own audit.

## 8. Definitions of Done

The sole definitions live in [features/plan-quota-rules.md](../features/plan-quota-rules.md):

- `cpt-cf-bss-pricing-dod-plan-quota-rules-store` — The rule shape, its digest and its storage.
- `cpt-cf-bss-pricing-dod-plan-quota-rules-authoring` — Reading and replacing a revision's rules.
- `cpt-cf-bss-pricing-dod-plan-quota-rules-checks` — Quota rows in the revision checks.
- `cpt-cf-bss-pricing-dod-plan-quota-rules-governance` — Approval content, rechecks, copy and clone.
- `cpt-cf-bss-pricing-dod-plan-quota-rules-acceptance` — Quota terms in the preview and the receipt.

## 9. Acceptance Criteria

1. PRD AC #26 / `cpt-cf-bss-pricing-dod-plan-quota-rules-authoring`: Given a draft with two items, when its author puts a complete set of rules then the set, its digest and a new ETag are saved together; a stale ETag, a replay, a conflicting key, an empty array and an invalid set behave as the criterion states.
2. PRD AC #26 / `cpt-cf-bss-pricing-dod-plan-quota-rules-store`: Given two concurrent replaces on one version, or an item delete racing a replace, on PostgreSQL, when both run then one set is stored and no rule names a missing item.
3. PRD AC #27 / `cpt-cf-bss-pricing-dod-plan-quota-rules-checks`: Given a revision with rules, when its checks run then each failing rule is a red quota row naming the rule and field, and each sellable cell that no rule of a metric covers is a red `QUOTA_RULE_MISSING` line naming its item, with the evaluated revision version and digest.
4. PRD AC #27, #28 / `cpt-cf-bss-pricing-dod-plan-quota-rules-governance`: Given a revision with rules, when it is submitted, approved, copied or cloned then submit and apply refuse red quota rows, a copy keeps its definitions on the copied items, and the published revision reads its rules after a withdrawal.
5. PRD AC #29 / `cpt-cf-bss-pricing-dod-plan-quota-rules-acceptance`: Given a published revision with 10 vCPU per plan unit, when a buyer accepts two units with the preview's quota digest then the receipt freezes the rule, its evidence, quantity 2 and both digests; a stale or missing digest is RESOLUTION_CHANGED.

## 10. Non-Functional Considerations

Both backends hold the same tables, keys, CHECKs and index, and every repository test has its PostgreSQL twin; the
two races of the PUT run on PostgreSQL in both orders. Every write runs in a serializable transaction; the PUT's
fence is the conditional revision write, the rule's foreign key and the in-transaction item comparison, with no row
lock. Products is read before the transaction, on a task of its own, and the transaction re-checks only stored
state; a moved selection rolls back and is captured again within the transaction retry budget. A revision holds at
most 200 rules, a scope selection at most 8 keys, and an amount at most 39 digits. A revision without rules reads,
checks, submits, copies, resolves and accepts as before, except the checks' two new members and the revision read's
empty `quota_rules` and its `quota_rules_digest`; every existing golden, digest vector and seam fixture stays as it
is.
