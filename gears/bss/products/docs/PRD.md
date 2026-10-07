<!-- CONFLUENCE_TITLE: [BSS]: Products — SKU Registry (PRD) -->
<!-- Related: ./DESIGN.md, ./DECISIONS.md, ./ADR/ | Owners: BSS Product Catalog team -->

# PRD — Products: SKU Registry

- [ ] `p1` - **PRD implementation status**

**Supersedes:** the Product & SKU Registry PRD on `bss/products-backup` (3a38f0b28) · **Source:** `docs/superpowers/specs/2026-09-24-pricebook-model-design.md`

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
  - [3.1 Module-Specific Environment Constraints](#31-module-specific-environment-constraints)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

Products provides a tenant-scoped SKU registry with flat categories, billing descriptors, metering declarations,
durable versions and governed lifecycle changes. A SKU is the catalog's unit and has no parent entity.
Pricing reads these definitions and owns books, price book entries, prices and plans (spec §1, §2 decisions 3 and 12, §4).

### 1.2 Background / Problem Statement

Operators need one SKU definition that commercial modeling can read consistently. Changes to descriptors must
reach future billing periods through a dated version while earlier bindings retain their descriptors. Publication,
changes and retirement need the same review process, and a SKU must not retire while another gear holds a live
reference. The prototype model and the explicit dispositions in spec §3 define this scope (spec §2 decisions 2,
8, 14 and 17).

### 1.3 Goals (Business Outcomes)

- Let catalog administrators author typed SKUs and organize them in one category level.
- Give reviewers one approval-unit shape for publication, changes and retirement, with tenant-configured quorum
  and separation of duties.
- Preserve the SKU version in force on a date so pricing can bind reproducible descriptors for each period.
- Prevent retirement and type changes from racing with new references through the Products reference registry.
- Keep Products and Pricing independently owned gears with explicit read, reservation and event contracts.

### 1.4 Glossary

| Term | Meaning |
| --- | --- |
| SKU | The tenant's named and coded commercial definition, with its own type, optional category and lifecycle. |
| Type | `recurring`, `usage`, `one_time` or `bundle`; determines charge kind where the SKU can be priced. |
| Category | An optional flat grouping, at most one per SKU, with code, name, default flag, sort order and active/retired status. |
| Descriptors | `gl_code`, `tax_category` and `invoice_line_template`, bound by pricing from a dated SKU version. |
| Metering | A usage SKU's `usage_type_ref` and `unit`; the reference resolves through the usage-type catalog port. |
| Derived usage type | A meter computed per granule from other usage, such as a cloudlet-hour from RAM and CPU; declared by Products as versioned data and evaluated by Rating (P-D-229). |
| Lifecycle | `draft`, `published`, `deprecated`, `retiring`, `retired`; `retiring` is the transient retirement fence. |
| SKU version | An append-only snapshot identified by SKU and `published_version`, with `effective_from`. |
| Concurrency version | A mutable row's optimistic concurrency token; distinct from the published SKU version. |
| Approval unit | One reviewable proposed action with items, snapshot, fingerprint, quorum, state and decisions. |
| Generation | The unit content revision a reviewer saw; refresh increments it and makes earlier decisions stale. |
| Fence | A durable barrier (`retiring` or `type_change_pending`) preventing new reference reservations. |
| Reservation | A `price_book_entry`, `plan_item` or `sold_as` reference attempt; reserved and confirmed rows both count as live. |
| Binding | Pricing's period-specific choice of SKU version and descriptors, retained by the consumer. |

## 2. Actors

### 2.1 Human Actors

#### Catalog Admin

**ID**: `cpt-cf-bss-products-actor-catalog-admin`

**Role**: Authors SKUs and categories, submits publication and change units, and requests retirement.
**Needs**: Draft editing, reference summaries, version history, approval status and fence recovery.

#### Finance Reviewer

**ID**: `cpt-cf-bss-products-actor-finance-reviewer`

**Role**: Reviews proposed content and effective dates, then approves or rejects units independently of authors.
**Needs**: Stored snapshots, live recomputation, generation-aware decisions and enforced separation of duties.

#### Auditor

**ID**: `cpt-cf-bss-products-actor-auditor`

**Role**: Reads tenant-scoped definitions, version history and approval audit records.
**Needs**: Durable snapshots and decisions, including stale generations and terminal outcomes.

### 2.2 System Actors

#### Pricing

**ID**: `cpt-cf-bss-products-actor-pricing`

**Role**: Reads SKU type, descriptors, metering and dated versions; consumes `SkuChanged` to refresh its read
model. Supplies reference facts by reserving, confirming and releasing its references in Products.
**Needs**: A synchronous reservation contract and grouped reference reads; Products answers the reference query
from its own registry (spec §2 decision 17, §4, §7.3, §13).

## 3. Operational Concept & Environment

### 3.1 Module-Specific Environment Constraints

- **Two gears**: Products owns SKUs, categories, versions and their approval units; Pricing owns commercial
  modeling and reads the SKU contract (spec §2 decision 3).
- **Reference barrier**: Pricing reserves before its write and confirms after commit. Products answers
  `GET /skus/{id}/references` locally. Fence acquisition and the live-reference check are one transaction;
  no cross-gear call occurs while setting a fence (spec §2 decision 17, §13).
- **Eventing**: Pricing consumes `SkuChanged` to refresh type, descriptors, metering and versions. Descriptor
  changes reach period bindings without creating a pricing approval unit (spec §7.3).
- **Storage and security**: Reuse toolkit REST, SecureORM tenant scoping, authz, outbox and the SQLite/Postgres
  infrastructure. The same approval rules apply on both backends (spec §2 decision 10, §3 item 27, §10).
- **Delivery boundary**: Products is implemented in phase 1; Pricing's reservation write path arrives in phase 2.
  These phases are not independently mergeable before the phase 2 integration gate (spec §11).

## 4. Scope

### 4.1 In Scope

Typed SKUs; flat categories; billing descriptors and billing-timing override; usage-type resolution; derived
usage types (P-D-229); lifecycle; durable dated versions; the three SKU approval kinds; policy settings; reference reservations and fences;
concurrency and idempotency; browse/search and reference summaries; audit and transactional events
(spec §3 D, §4, §6, §7.2–§7.3).

### 4.2 Out of Scope

Product entities; attributes, localization and display fields; CatalogVersion snapshots, freeze and diff;
PlanTier; bundle composition signals; bulk import/export; environment promotion; SKU clone; retention and
right-to-erasure. Pricing owns books, price book entries, prices, plans, promotions and migrations. Rating and Subscriptions
adaptation, and Studio API wiring, are separate programmes (spec §3 D, §4, §10–§11).

## 5. Functional Requirements

#### `fr-sku-define`

- [ ] `p1` - **ID**: `cpt-cf-bss-products-fr-sku-define`

The registry shall create and edit a SKU as an independent tenant-scoped definition with code, name, type,
an optional category, description, sellable flag, descriptors, billing timing and type-appropriate metering.

**Rules**

- Code and name are each unique per tenant; collisions return 409 `SKU_CODE_TAKEN` and `SKU_NAME_TAKEN`.
- The category is optional (P-D-196): an omitted `category_id` is null, with no fallback to the default
  category; a draft `PATCH` with `category_id: null` and a `sku_change` can clear it.
- SKU and category records carry tenant identity, creation/update timestamps and a concurrency `version`;
  a SKU also carries `revision` and `published_version`.
- Direct `PATCH` edits drafts only; published or deprecated content changes use `sku_change`.
- A pending unit prevents edits, deletion or participation in another unit (`ROW_LOCKED_PENDING`, 409).

**Rationale**: spec §3 item 32, §4 (SKU fields and uniqueness), §6 (pending lock), §7.2.

#### `fr-sku-type-frozen`

- [ ] `p1` - **ID**: `cpt-cf-bss-products-fr-sku-type-frozen`

A SKU's type shall determine the charge kind of its price book entries; a type change must pass the reference barrier.

**Rules**

- A SKU with a price book entry cannot change type (`SKU_TYPE_FROZEN`, 409).
- The type-change fence is refused while any registry reservation is reserved or confirmed, including
  `plan_item` and `sold_as` references (`SKU_TYPE_FROZEN`, 409).
- The live-reference check and setting `type_change_pending` occur in one transaction; while fenced, new
  reservations fail with 409 `SKU_FENCED`. No remote count participates in this transaction.
- Published or deprecated type changes follow `sku_change`; rejecting or withdrawing the pending unit releases
  its fence and pending lock. A fence without a unit follows the recovery rules in `fr-sku-retire-fenced`.

**Rationale**: spec §2 decision 17, §2.2 (fences and conditional writes), §4 (type and reference rules), §6.

#### `fr-sku-descriptors`

- [ ] `p1` - **ID**: `cpt-cf-bss-products-fr-sku-descriptors`

The SKU shall own its GL code, tax category and invoice-line template. Changes to published or deprecated
content shall carry an effective date through a `sku_change` approval unit.

**Rules**

- `effective_from` defaults to today. Applying the unit appends a dated snapshot and emits
  `SkuChanged { skuId, changed[], effectiveFrom }`.
- Pricing binds descriptors from the version in force at a period's start; earlier bindings retain theirs.
  Descriptor changes reach every book through bindings; no per-book approval or refreeze action is required.
- `billing_timing`, when present, is `advance` or `arrears` and overrides the tenant default.
- A pending unit blocks direct edits (`ROW_LOCKED_PENDING`, 409); a proposed date earlier than the latest
  version's date is refused (`VERSION_ORDER`, 409).

**Rationale**: spec §2 decision 14, §2.2 (version timeline), §4, §6, §7.1–§7.2, §8, §12.

#### `fr-sku-metering`

- [ ] `p1` - **ID**: `cpt-cf-bss-products-fr-sku-metering`

A usage SKU shall declare the meter reference and unit needed by consumers before it can publish.

**Rules**

- Publication requires both `usage_type_ref` and `unit` (`USAGE_NEEDS_METER` when incomplete).
- The reference must resolve through the pluggable usage-type catalog port (`USAGE_TYPE_UNRESOLVED` when it
  cannot resolve); the catalog integration remains in force. The catalog is read as the caller: a catalog
  that refuses the caller is 403 `USAGE_TYPE_FORBIDDEN` at submit and approve, never an outage (P-D-207).
- Metering fields are usage-only; in particular, a bundle rejects them (`BUNDLE_HAS_NO_METER`).
- Submit validates the proposed content and apply revalidates it before publication or change.

**Rationale**: spec §4 (usage rule), §6 (subject validation), §15 (usage-type catalog design retained).

#### `fr-derived-usage-type`

- [ ] `p1` - **ID**: `cpt-cf-bss-products-fr-derived-usage-type`

The registry shall declare a derived usage type, a meter computed per granule from other usage (a cloudlet-hour from RAM and
CPU, for example), as versioned data with one evaluator that Rating calls (P-D-229, P-D-230).

**Rules**

- A version names its output unit, its granularity (an hour), at least one raw input, a formula over them, and the
  output's scale and rounding. Each input is a GTS usage type at its exact version, with its granule fold and, for a
  time-weighted input, its hold bound. A version never changes; a new formula is a new version.
- The formula is data in a closed grammar: inputs, constants, add, subtract, multiply, divide by a non-zero constant, the
  larger and the smaller of two or more, ceil, floor and round. A declaration that breaks a rule is refused with the rule
  named: an unknown, unused or duplicate input, a derived input, a division by zero, a max or min of fewer than two
  operands, a formula deeper than 32 or of more than 256 nodes, a scale above 12, a hold that is missing, misplaced or
  outside 1 to 86,400 seconds, or an empty or over-cap unit.
- The formula applies per granule, to that granule's folded input quantities; a window's quantity is the sum of its granule
  outputs. The arithmetic is exact decimal and checked: an overflow, a missing, extra or negative input and a negative
  result are errors, never a panic. A result is rounded, then normalized (`-0` is `0`).
- A derived meter is named `products.derived/<code>@<n>`, with `<n>` a canonical version from 1. The prefix is reserved;
  a GTS id never starts with it.
- A declaration has one canonical encoding (decimals normalized, a fixed field order), and its digest identifies the
  version.
- A type has a code unique in the tenant and a name; its versions are stored append-only, with no approval of their own,
  each with the digest of its canonical encoding. Each input resolves through the usage-type catalog as the caller. Writes
  need the derived usage type's author grant, reads the SKU read grant, and every create and version is audited (P-D-231).
- A usage SKU names a derived meter by its id as its `usage_type_ref`, and sells its version's output unit. The registry
  judges the ref from its own store, never from the usage-type catalog, configured or not: a version the tenant does not
  hold, and a unit other than its output unit, are refused. A draft may move to another version; the first publish pins
  it. After that, a raw meter may move onto the identity wrapper of that meter, in the same unit (P-D-251). Every other
  change of the ref is refused (to another version, to or from a raw ref, or by dropping it). A new formula is
  sold through a new usage SKU (P-D-232).
- The registry answers pricing's meter semantics for a derived meter (E1b): the version's output unit, a sum over the
  granules, the accrual `derived-v1:<digest>` and the stored digest, read in the caller's tenant under the SKU read grant;
  a version off the meter id, or a meter the tenant does not hold, is refused. Any other meter is a raw one, answered as
  if no provider were registered until the raw-meter provider exists (P-D-233).

**Rationale**: owner decision of 2026-10-01 (P-D-229; rating T-D-39); the declaration, grammar and evaluator (P-D-230);
the storage, doors, grants and audit (P-D-231); the SKU's binding and pin (P-D-232); the meter semantics answered to
pricing (P-D-233).

#### `fr-sku-bundle`

- [ ] `p1` - **ID**: `cpt-cf-bss-products-fr-sku-bundle`

A bundle shall be a SKU that a Pricing plan is sold as, without composition stored in Products.

**Rules**

- A bundle is never priced and cannot be a plan item; its relationship to a plan is `sold_as`.
- Bundles have no meter; attempts to assign usage metering fail with `BUNDLE_HAS_NO_METER`.
- Sold-as references use the same reservation barrier as price book entries and plan items; a fenced bundle rejects a new
  reservation with 409 `SKU_FENCED`.

**Rationale**: spec §2 decision 17, §3 items 13 and 39, §4 (bundle rule), §5 (plan rules), §13.

#### `fr-sku-lifecycle`

- [ ] `p1` - **ID**: `cpt-cf-bss-products-fr-sku-lifecycle`

The registry shall govern `draft → published ↔ deprecated` and retirement, with `retiring` as the transient
retirement fence and `retired` as the retirement result.

**Rules**

- `sku_publish` publishes a draft and increments `published_version`.
- `sku_change` governs content and/or lifecycle changes on published or deprecated SKUs, including deprecation
  and return to published status, with an effective date.
- `sku_retire` applies only with zero live references; a referenced retirement is refused (`SKU_REFERENCED`).
- Pricing refuses a new price book entry or plan item on a retiring SKU (`SKU_RETIRING`); its new-plan-revision check
  refuses a deprecated SKU (`ITEM_SKU_DEPRECATED`).
- A pending unit owns the mutation lock (`ROW_LOCKED_PENDING`, 409); rejection or withdrawal unlocks it.
- A never-published draft is deleted by its author, never retired (P-D-206): `DELETE /skus/{id}` under
  `If-Match` answers 204; a SKU that was ever published is `SKU_NOT_DRAFT` (409).

**Rationale**: spec §3 item 35, §4 (fence and lifecycle), §5 (deprecated guard), §6, §7.2.

#### `fr-sku-versions`

- [ ] `p1` - **ID**: `cpt-cf-bss-products-fr-sku-versions`

Every publish and every applied change appends a `sku_version (sku_id, published_version, effective_from,
snapshot)`. A version's `effective_from` is the `effectiveFrom` of the `sku_change` unit that produced it (the
publish itself is effective at once). `GET /skus/{id}/versions/as-of?date=<date>` returns the version whose
`effective_from` is the latest not after the date, and `GET /skus/{id}/versions` returns every version as an
array (P-D-214). Pricing binds a period's descriptors from the dated read (spec §7.1); nothing in this gear is
frozen per price.

**Rules**

- Versions are append-only; a version is never edited or deleted.
- A change cannot carry `effective_from` earlier than the latest existing version's date (`VERSION_ORDER`,
  409). Equal dates are allowed; the higher `published_version` wins for that date. There is no unique index
  on `(sku_id, effective_from)`.
- A date earlier than the first version answers 404 `NO_VERSION_IN_FORCE`.
- The `sku` row holds the latest applied content, which can be future-effective; consumers use the dated
  version read to determine what is in force.

**Rationale**: spec §2.2 (durable versions and amended timeline), Codex finding 10; §4, §7.1.

#### `fr-sku-retire-fenced`

- [ ] `p1` - **ID**: `cpt-cf-bss-products-fr-sku-retire-fenced`

Retirement shall acquire a durable fence before the retirement unit proceeds, preventing references from
racing with approval and apply.

**Rules**

- In one transaction, refuse the fence if any reserved or confirmed reference exists (`SKU_REFERENCED`),
  otherwise set `retiring`, `fenced_at` and `fence_op_id`. New reservations fail with 409 `SKU_FENCED`.
- Apply revalidates zero references. A failed environment check is `APPLY_REFUSED` with the domain reason
  `SKU_REFERENCED`; the apply transaction rolls back and the SKU stays `retiring` until the unit is withdrawn
  or rejected, restoring its pre-fence lifecycle and releasing the pending lock.
- Retried submit on a fenced SKU with no pending unit resumes the operation by rechecking and submitting.
- A fence older than `fence_ttl_minutes` (a deployment setting, P-D-209), without a unit, is reverted by the next request on that
  SKU or by `POST /skus/{id}/unfence`. Recovery also applies to `type_change_pending` fences.
- A live reservation cannot be ignored because its caller died; release must follow `fr-reference-registry`.

**Rationale**: spec §2 decision 17, §2.2 (resumable fences), §4, §6 (apply refusal and unlock), §13.

#### `fr-category-flat`

- [ ] `p1` - **ID**: `cpt-cf-bss-products-fr-category-flat`

The registry shall maintain a flat category list with at most one category per SKU.

**Rules**

- Categories carry `id`, tenant-unique `code`, `name`, `is_default`, `sort_order` and `active | retired` status.
- Category creation and edits are direct operations and require no approval unit.
- Retirement fails with `CATEGORY_IN_USE` while a SKU in `draft`, `published`, `deprecated` or `retiring`
  points at the category; retired SKUs and a SKU without a category do not block it (P-D-196, P-D-208).
  Retiring a retired category is `CATEGORY_RETIRED` (409).
- Category patches participate in optimistic concurrency (`STALE_REVISION` for a stale revision).

**Rationale**: spec §2 decision 12, §4 (category schema and retire rule), §7.2, §14.

#### `fr-approval-units`

- [ ] `p1` - **ID**: `cpt-cf-bss-products-fr-approval-units`

The registry shall use the shared `bss-approval` unit shape for `sku_publish`, `sku_change` and `sku_retire`.

**Rules**

- Quorum comes from tenant settings with an optional per-kind override and is copied into the unit on submit;
  there is no materiality threshold. Submit validates, records items and snapshot, and conditionally acquires
  `pending_unit_id`; failed acquisition returns 409 `ROW_LOCKED_PENDING` and rolls the submission back.
- States are `pending`, `approved`, `rejected`, `withdrawn`. At quorum zero, submit still records an approved
  unit, with `decided_at = submitted_at`, no decisions, and the ordinary audit and events.
- The submitter and every item's `created_by` are excluded from approval (403 `SOD_VIOLATION`), even when they
  hold both submit and approve permissions. A reviewer need not hold submit permission.
- Approve and reject name the reviewed `generation`; a mismatch returns 400 `GENERATION_MISMATCH` with the
  current generation. An actor can vote only once per generation (409 `DUPLICATE_VOTE`).
- Re-collection compares proposed business content and effective date, excluding lock/version metadata.
  Drift rewrites items, snapshot and hash, increments generation, marks earlier decisions stale, and commits
  the refresh while returning 400 `UNIT_STALE` with the new generation. Reviewers vote again on that content.
- Approval below quorum leaves the unit pending; quorum applies the subject and approves atomically.
  An environment failure at apply returns `APPLY_REFUSED` and rolls back rather than refreshing content.
- One reject closes the unit and requires a note. Only the submitter may withdraw (`NOT_SUBMITTER` otherwise).
  Decisions or withdrawal on a non-pending unit fail with 409 `UNIT_ALREADY_DECIDED`.
- Every unit write is conditional on its `version`; losing a race returns 409 `UNIT_CONTENDED` for client retry.
  No database row locks are used. Terminal transitions clear pending locks; approval retains
  `approved_by_unit_id` as provenance.
- Unit detail returns both its stored snapshot and a live recomputation; the latter does not replace the
  transactional fingerprint check.

**Rationale**: spec §2 decision 8, §2.2 (generations and conditional writes), §6, §14 (policy scope).

#### `fr-events`

- [ ] `p1` - **ID**: `cpt-cf-bss-products-fr-events`

Products shall record domain events and approval decisions in the outbox in the transaction that changes state.

**Rules**

- Domain events are `SkuPublished`, `SkuChanged { skuId, changed[], effectiveFrom }` and `SkuRetired`.
- Every terminal unit transition, including reject, withdraw and quorum-zero approval, records
  `ApprovalUnitDecided { unitId, kind, state, generation, actors[] }` and an audit row. Submission records an
  audit row but no event unless it also applies through quorum zero.
- Operator force-release records `ReferenceForceReleased` with its audit record.
- A rolled-back apply (`APPLY_REFUSED`, including `SKU_REFERENCED`) publishes no success event or terminal
  decision. A `UNIT_STALE` refresh does not emit a successful apply event.
- Pricing consumes `SkuChanged` to refresh its SKU read model; this does not draft changes in any price book.

**Rationale**: spec §4, §6 (terminal transitions and same-transaction events), §7.3, §13.

#### `fr-read-model`

- [ ] `p1` - **ID**: `cpt-cf-bss-products-fr-read-model`

The registry shall expose a simplified tenant-scoped browse/search model and a SKU card with reference summary
and durable version reads.

**Rules**

- List and search support code, name, category, type and lifecycle. A category filter matches only the SKUs
  in that category; an unfiltered list includes the SKUs without a category (P-D-196), and `category_id eq
  null` lists those. The list pages on the toolkit's OData, with a case-insensitive `q` over code, name, unit,
  usage type and GL code, and answers tab counts per lifecycle and in review (P-D-210, P-D-211).
- `GET /skus/{id}/references` reads the local registry, returning reference rows and counts grouped by owner
  and kind; reserved references count alongside confirmed ones.
- The card makes unresolved reservations visible so an operator can inspect and release abandoned attempts.
- The list items and the card carry pricing's usage of each SKU (its entries, their currencies, prices by
  state and distinct plans) through the `SkuUsageV1` port that pricing fills. The usage is `null` when the port
  is absent, refuses the caller or cannot answer; the read never fails for it, and the usage never takes part
  in a fence (P-D-197). The list filters on the same facts (`priced`, `in_plan`) through the port's sets; a
  filter the port cannot answer fails the read rather than widening it (P-D-212).
- Dated reads use the version timeline, not the latest SKU row, which can contain future-effective content.
  A date earlier than the first version returns 404 `NO_VERSION_IN_FORCE`.
- Reads require `products:read` and tenant scope; a caller cannot use search, card, reference or version reads
  to inspect another tenant's data.

**Rationale**: spec §2.2 (dated truth), §3 item 43, §4 (read model and registry), §7.2–§7.3, §12.

#### `fr-reference-registry`

- [ ] `p1` - **ID**: `cpt-cf-bss-products-fr-reference-registry`

Products shall own durable reference reservations so fencing a SKU and accepting a new reference cannot both
succeed in a race.

**Rules**

- Records contain `id`, `sku_id`, `owner_gear`, `ref_kind` (`price_book_entry | plan_item | sold_as`), `ref_id`,
  `state` (`reserved | confirmed | released`), reservation/confirmation/release timestamps, `released_by`
  and `release_reason`.
- Within tenant scope, `(owner_gear, ref_kind, ref_id)` is unique over live rows only. Each new attempt after
  release has a fresh reservation ID; released rows are retained and never reactivated.
- Reserve creates a reservation with 201, or returns 200 and the existing reservation for the same live logical
  reference. A fenced SKU refuses new reservations with 409 `SKU_FENCED`.
- Reserved and confirmed references count until released, with no expiry-based exemption. A live reservation
  prevents a retirement fence (`SKU_REFERENCED`) or type-change fence (`SKU_TYPE_FROZEN`) in the same database
  transaction; Postgres uses serializable isolation.
- Confirming a confirmed row returns 200; confirming a released row returns 409 `REFERENCE_RELEASED`.
- The owner gear releases only after durable cancellation or deletion of its own object. An operator's
  `DELETE /references/{id}` requires `force: true` and a reason, writes audit and emits
  `ReferenceForceReleased` so the owner can verify and re-reserve an object that still exists.
- Pricing's protocol is reserve → re-read SKU → write object, reservation ID and confirmation work in one
  Pricing transaction → confirm with durable retry. If Products is unavailable before the write, Pricing
  returns 503 `REGISTRY_UNAVAILABLE` and writes nothing. If confirmation fails after commit, Pricing retains
  `confirmation_pending = true` and retries; it must not release merely because confirmation timed out.
- Definite rollback leads to durable cancellation then release; deletion leads to removal then release.
  Products cannot itself detect release beneath a live owner object; the owner must obey this protocol.

**Rationale**: spec §2 decision 17, §2.2 (reference barrier), §4 (registry), §7.2–§7.3, §12–§13.

#### `fr-concurrency-idempotency`

- [ ] `p1` - **ID**: `cpt-cf-bss-products-fr-concurrency-idempotency`

Authoring shall preserve optimistic concurrency and support tenant-scoped replay of keyed POST requests.

**Rules**

- Every PATCH requires `If-Match` against the row version; stale revisions fail with `STALE_REVISION`.
- Every POST accepts an optional `Idempotency-Key`. The replay store is keyed by `(tenant, endpoint,
  client_key)` and retained for 24 hours; a replay is checked before any fence or approval-unit work.
- Approval units carry no separate idempotency key. Logical-reference reserve idempotency also applies when
  no client key is supplied.
- Conditional unit updates prevent lost decisions (`UNIT_CONTENDED`, 409); conditional pending-lock acquisition
  prevents multiple units owning one item (`ROW_LOCKED_PENDING`, 409).

**Rationale**: spec §3 item 23, §2.2 (one idempotency contract and conditional writes), §6, §7.2 products row.

## 6. Non-Functional Requirements

#### `nfr-authz`

- [ ] `p1` - **ID**: `cpt-cf-bss-products-nfr-authz`

All operations shall deny access by default and enforce the applicable `products:read`, `products:author`,
`products:submit`, `products:approve` or `products:settings` permission. Submit and approve are separate grants;
possessing both does not bypass separation of duties. Verification includes permission denial and an author
attempting to approve their own unit (spec §3 item 27, §6, §7.3).

#### `nfr-audit`

- [ ] `p1` - **ID**: `cpt-cf-bss-products-nfr-audit`

Every approval submission shall write an audit row. Every terminal transition shall atomically write its audit
row and `ApprovalUnitDecided`, including rejection, withdrawal and quorum-zero approval. Operator force-release
shall also be audited and evented; stale decisions remain recorded for their generation. Verify each path
individually (spec §2.2, §4, §6).

#### `nfr-tenant-isolation`

- [ ] `p1` - **ID**: `cpt-cf-bss-products-nfr-tenant-isolation`

All SKU, category, version, approval, reference, audit and replay operations shall obey SecureORM tenant scope.
Uniqueness and idempotency shall not collide between tenants, and no read or write may escape the caller's
scope. Verification uses two tenants with overlapping codes and client keys (spec §3 item 27, §4, §6, §2.2).

#### `nfr-two-backends`

- [ ] `p1` - **ID**: `cpt-cf-bss-products-nfr-two-backends`

The same registry, approval, version and reservation behavior shall pass storage checks on SQLite and Postgres.
Use conditional writes without `FOR UPDATE`; Postgres fences use serializable isolation and SQLite has one
writer. Both storage tiers must pass at phase gates (spec §2 decision 10, §2.2, §5–§6, §10–§11).

## 7. Public Library Interfaces

### 7.1 Public API Surface

Routes below are relative to the Products API. Errors expose `{ code, field, message }`; stale-generation errors
also report the current generation. PATCH, the draft DELETE and the policy PUT use `If-Match`; POST accepts
an optional `Idempotency-Key` (spec §7.2 products row, with §2.2 amendments).

| Surface | Calls and behavior |
| --- | --- |
| SKU authoring | `POST /skus`; `PATCH /skus/{id}` for drafts; `DELETE /skus/{id}` for a never-published draft (P-D-206); `POST /skus/{id}/changes` for published/deprecated content and/or lifecycle, with `effective_from` defaulting to today. |
| Usage-type picker | `GET /usage-types?q&kind&limit&cursor`, the catalog the publish gate resolves against, read as the caller (P-D-207). |
| SKU reads | `GET /skus` on the toolkit's OData (`$filter`, `$orderby`, `$top`/`limit`, `cursor`) with `q`, `priced` and `in_plan`, `GET /skus/counts`, `GET /skus/{id}`; list/search by code, name, category (or none), type, lifecycle, in-review and pricing's usage (P-D-210, P-D-211, P-D-212); each SKU carries pricing's `usage` or `null` (P-D-197). |
| Lifecycle | `POST /skus/{id}/submit`, `POST /skus/{id}/retire`, `POST /skus/{id}/unfence`; submit, retire and changes take the submitter's optional `note`, kept on the unit (P-D-219). |
| History | `GET /skus/{id}/history`: every act on the SKU and its approval units, oldest first, with who, when, the lifecycle it moved from and to, the unit and the note (P-D-213). |
| Versions | `GET /skus/{id}/versions` reads every version as an array; `GET /skus/{id}/versions/as-of?date=<date>` reads the version in force (P-D-214; the spelling question of §13 is settled). |
| References | `GET /skus/{id}/references` returns `{ owner, kind, ref_id, state }` rows and grouped counts; `POST /skus/{id}/references/reserve { owner, kind, ref_id }` returns `{ reservation_id }`; `POST /references/{id}/confirm`; `DELETE /references/{id}` releases, with `force: true` and reason for an operator. |
| Categories | `GET /categories` (paged, filtered and ordered on the toolkit's OData), `GET /categories/{id}`, `POST /categories`, `PATCH /categories/{id}`, `POST /categories/{id}/retire`; the reads carry `sku_count`, the SKUs that are not retired naming the category (P-D-215). |
| Approvals | `GET /approval-units?state&kind&refId` (newest first with `$orderby=submitted_at desc`) and `GET /approval-units/counts` by state and kind (P-D-227), `GET /approval-units/{id}`, each unit with `caller_can_approve` (P-D-228), `POST /approval-units/{id}/approve`, `/reject`, `/withdraw`; approve/reject carry `generation`, reject requires a note; every unit carries its `submit_note` (P-D-219). |
| Approval policy | `GET /approval-policy` with a content `ETag`, `PUT /approval-policy` under `If-Match`: the tenant default quorum and its optional per-kind overrides (P-D-205); `DELETE /approval-policy/{kind}` under `If-Match` resets a kind's override to the default, and the default is never deleted (P-D-216). The fence TTL is a deployment setting, not a tenant one (P-D-209). |

The `products-sdk` surface exposes `Sku`, `SkuType`, `Lifecycle`, `Category`, `SkuVersion` and
`SkuChangedPayload`. The usage-type catalog port remains available, and the `SkuUsageV1` port, which pricing
fills, carries pricing's usage of SKUs to the SKU reads (P-D-197). `ProductCatalogClientV1` and its browse
transport (`GET /bss-products/v1/browse`) remain until phase 2, as required by the Task 3 interface boundary.

### 7.2 External Integration Contracts

- **Pricing reads**: SKU type, descriptors and metering, plus dated `SkuVersion` snapshots for binding. The
  latest applied SKU row may be future-effective; it is not a substitute for the dated read (spec §2.2, §7.1).
- **Pricing references**: reserve/write/confirm and cancellation/deletion/release, including sold-as references.
  Products answers counts from its own registry; confirmations may be retried durably (spec §13).
- **Pricing events**: `SkuChangedPayload` includes `skuId`, `changed[]` and `effectiveFrom`; Pricing refreshes
  its read model without drafting book changes (spec §6–§7.3).
- **Usage-type catalog**: the pluggable resolution port remains unchanged; usage publication requires a
  resolvable reference and a unit (spec §4, §15).
- **Approval library**: both gears own their approval storage and implement the shared `bss-approval` subject
  contract for collection, validation, locking, snapshot, apply and unlock (spec §6).

## 8. Use Cases

#### Change a GL code

- [ ] `p1` - **ID**: `cpt-cf-bss-products-usecase-change-gl-code`

**Actor**: `cpt-cf-bss-products-actor-catalog-admin`; independent decision by
`cpt-cf-bss-products-actor-finance-reviewer`.

**Preconditions**:

- Storage is published with GL code `4010-STOR`; the author can submit and the reviewer can approve.

**Main Flow**:

1. Author a `sku_change` proposing `4012-STOR` with `effectiveFrom = 2026-10-01`.
2. Submit the unit and review the snapshot showing the before/after GL code and effective date.
3. An independent reviewer approves the generation they saw; once quorum is reached, apply the change,
   append SKU version 7 and record `SkuChanged` and the terminal approval event in the same transaction.
4. Pricing binds version 7 for periods starting on or after October 1.

**Postconditions**:

- Earlier bindings retain `4010-STOR`; periods starting on or after the effective date use `4012-STOR`.

**Alternative Flows**:

- Author approval fails with 403 `SOD_VIOLATION`. Content drift returns 400 `UNIT_STALE` with the refreshed
  generation and requires renewed review; an outdated vote returns `GENERATION_MISMATCH` (spec §2.2, §6, §8).

#### Retire a SKU

- [ ] `p1` - **ID**: `cpt-cf-bss-products-usecase-retire-sku`

**Actor**: `cpt-cf-bss-products-actor-catalog-admin`; independent decision by
`cpt-cf-bss-products-actor-finance-reviewer`.

**Preconditions**:

- The SKU is published or deprecated. Its card exposes all reserved and confirmed references.

**Main Flow**:

1. Inspect the reference summary; the owner removes or durably cancels its objects before releasing references.
2. Request retirement. Products atomically checks zero live references and records the `retiring` fence.
3. Submit the retirement unit; while it is pending, new reference reservations fail with `SKU_FENCED`.
4. Review and approve the unit; apply rechecks zero references and atomically retires the SKU with audit,
   `SkuRetired` and `ApprovalUnitDecided`.

**Postconditions**:

- The SKU is retired and the approval provenance is retained.

**Alternative Flows**:

- A live reservation refuses the initial fence (`SKU_REFERENCED`), leaving the original lifecycle intact.
- If apply finds an invalid reference environment, it refuses with `SKU_REFERENCED` under `APPLY_REFUSED`;
  the SKU stays `retiring`. The submitter withdraws, or a reviewer rejects with a note, to abort and restore
  the pre-fence lifecycle and clear the pending lock.
- A crash between fence and submission is resumed by retry; an expired fence with no unit is reverted by the
  next SKU request or explicit unfence. An abandoned reservation requires owner release or an audited operator
  force-release with a reason (spec §2.2, §4, §6, §13).

## 9. Acceptance Criteria

**AC #1. Unique SKU identity — `fr-sku-define`**

- **Given** a tenant with a SKU using code `STORAGE` and name `Storage`.
- **When** an author creates another SKU with that code or name.
- **Then** creation fails with 409 `SKU_CODE_TAKEN` or 409 `SKU_NAME_TAKEN`, respectively; the same identifiers
  in another tenant do not collide.

**AC #2. Referenced type is frozen — `fr-sku-type-frozen`**

- **Given** a SKU with a reserved or confirmed price book entry, plan-item or sold-as reference.
- **When** an author attempts to change its type.
- **Then** the fence and change are refused with 409 `SKU_TYPE_FROZEN`, leaving the type unchanged.

**AC #3. Dated GL change — `fr-sku-descriptors`**

- **Given** Storage with `4010-STOR` and an independent reviewer.
- **When** a change to `4012-STOR` effective October 1 reaches quorum.
- **Then** a dated version and `SkuChanged` are recorded; earlier period bindings retain the old GL code and
  periods starting on or after October 1 bind the new one without a Pricing unit.

**AC #4. Locked descriptor edit — `fr-sku-descriptors`**

- **Given** a SKU whose descriptor change is pending review.
- **When** an author attempts to edit the locked content directly.
- **Then** the edit fails with 409 `ROW_LOCKED_PENDING` and the reviewed snapshot remains intact.

**AC #5. Required usage metering — `fr-sku-metering`**

- **Given** a usage SKU missing a meter reference or unit, or carrying an unresolvable reference.
- **When** publication is submitted.
- **Then** validation refuses it with `USAGE_NEEDS_METER` or `USAGE_TYPE_UNRESOLVED`, respectively, without
  publishing a version.

**AC #6. Bundle boundary — `fr-sku-bundle`**

- **Given** a bundle SKU.
- **When** an author assigns metering to it.
- **Then** the change fails with `BUNDLE_HAS_NO_METER`; its supported commercial relationship remains a plan's
  sold-as reference, with no composition in Products or price book entry on the bundle.

**AC #7. Lifecycle adoption guards — `fr-sku-lifecycle`**

- **Given** a retiring SKU and a deprecated SKU.
- **When** Pricing tries to add a price book entry or plan item on the retiring SKU, or adds the deprecated SKU to a new
  plan revision.
- **Then** it refuses with `SKU_RETIRING` or `ITEM_SKU_DEPRECATED`, respectively.

**AC #8. Version timeline and equal dates — `fr-sku-versions`**

- **Given** an existing SKU version effective October 1.
- **When** another change proposes September 30, or an approved change uses October 1 again.
- **Then** the earlier date fails with 409 `VERSION_ORDER`; the equal-date change appends a version, and the
  higher `published_version` wins for October 1 while both immutable snapshots remain stored.

**AC #9. Version in force — `fr-sku-versions`**

- **Given** publication on September 24 and an applied descriptor change effective October 1.
- **When** a consumer reads versions as of September 30, October 1 or September 23.
- **Then** it receives the publication snapshot, the changed snapshot or 404 `NO_VERSION_IN_FORCE`,
  respectively; the latest applied SKU row does not make the future change effective early.

**AC #10. Retirement fence refusal — `fr-sku-retire-fenced`**

- **Given** a published SKU with a live reservation, including an unconfirmed reservation left by a dead caller.
- **When** retirement is requested.
- **Then** `SKU_REFERENCED` refuses the fence in the transaction that checks the registry and the SKU remains
  published; the reservation continues counting until released.

**AC #11. Retirement apply refusal and abort — `fr-sku-retire-fenced`**

- **Given** a pending retirement unit and an invalid reference environment detected at apply (a defensive
  revalidation case; the reservation protocol prevents a normal new reservation through the fence).
- **When** quorum attempts to apply retirement.
- **Then** apply is refused as `APPLY_REFUSED` with reason `SKU_REFERENCED`; the SKU stays `retiring` until
  withdrawn or rejected, which restores the pre-fence lifecycle and clears the pending lock.

**AC #12. Interrupted fence recovery — `fr-sku-retire-fenced`**

- **Given** a SKU fenced without a pending unit after an interrupted submit.
- **When** submit is retried before expiry, or the next SKU request/unfence arrives after `fence_ttl_minutes`.
- **Then** the unexpired operation resumes with revalidation, or the expired orphan fence is reverted,
  respectively; a fence belonging to a pending unit is not cleared by orphan recovery.

**AC #13. Referenced category — `fr-category-flat`**

- **Given** a flat category referenced by a SKU that is not retired.
- **When** an administrator requests category retirement.
- **Then** retirement fails with `CATEGORY_IN_USE`; a category no such SKU references can retire directly
  without an approval unit, also when the tenant has SKUs without a category or its SKUs are all retired
  (P-D-208).

**AC #14. Separation of duties — `fr-approval-units`, `nfr-authz`**

- **Given** a pending unit and an actor who is its submitter or the author of any item, even if another actor
  submitted it.
- **When** that actor attempts approval with the correct generation and approve permission.
- **Then** approval fails with 403 `SOD_VIOLATION` and contributes no vote.

**AC #15. Stale content commits a refresh — `fr-approval-units`**

- **Given** a pending unit whose re-collected business content or effective date differs from its fingerprint.
- **When** a reviewer attempts a decision.
- **Then** the operation returns 400 `UNIT_STALE` with the incremented generation, commits refreshed items,
  snapshot and hash, and preserves previous decisions as stale without applying the change.

**AC #16. Generation and duplicate votes — `fr-approval-units`**

- **Given** a pending unit with a current generation and an actor who already voted in it.
- **When** a delayed vote names an earlier generation, or the same actor votes again in the current generation.
- **Then** the request fails with 400 `GENERATION_MISMATCH` and the current generation, or 409 `DUPLICATE_VOTE`,
  respectively; stale decisions never count toward the current quorum.

**AC #17. Terminal decisions and withdrawal — `fr-approval-units`**

- **Given** a pending unit and a terminal unit.
- **When** a non-submitter withdraws the pending unit, or a caller decides/withdraws the terminal unit.
- **Then** the first operation fails with `NOT_SUBMITTER` and the second with 409 `UNIT_ALREADY_DECIDED`;
  neither changes the recorded outcome.

**AC #18. Concurrent unit updates — `fr-approval-units`, `fr-concurrency-idempotency`**

- **Given** two decisions attempting to update the same observed unit version.
- **When** one conditional update wins.
- **Then** the losing update returns 409 `UNIT_CONTENDED` for retry and cannot overwrite the winning decision.

**AC #19. Quorum and locking — `fr-approval-units`**

- **Given** pending units with copied quorum values of one and two.
- **When** eligible distinct reviewers vote for the current generation.
- **Then** one vote applies the quorum-one unit, two votes apply the quorum-two unit, and one vote leaves the
  latter pending; attempts to edit, delete or submit its locked SKU elsewhere fail with `ROW_LOCKED_PENDING`.

**AC #20. Terminal audit and events — `fr-events`, `nfr-audit`**

- **Given** valid approval, rejection, withdrawal and quorum-zero publication paths.
- **When** each reaches its terminal state.
- **Then** its state, audit row and `ApprovalUnitDecided` commit together; successful applies also include the
  relevant SKU event, and quorum zero records an approved unit with no decisions and equal submitted/decided times.

**AC #21. Refused apply has no success event — `fr-events`**

- **Given** a subject whose apply revalidation returns `APPLY_REFUSED` with `SKU_REFERENCED`.
- **When** the approval transaction rolls back.
- **Then** it records no `SkuRetired` or successful terminal `ApprovalUnitDecided`, and no retirement is visible.

**AC #22. Scoped search and card — `fr-read-model`, `nfr-tenant-isolation`**

- **Given** two tenants with matching SKU names and one tenant with reserved and confirmed references.
- **When** a reader filters by code, name, category, type or lifecycle and opens its SKU card.
- **Then** only its scoped SKUs appear, reference counts include both live states grouped by owner and kind,
  and attempts to inspect the other tenant through card, reference or version reads disclose no data.

**AC #23. Fence/reserve race — `fr-reference-registry`**

- **Given** concurrent attempts to reserve a reference and acquire a retirement or type-change fence.
- **When** the database commits one attempt first.
- **Then** a winning reservation blocks the fence (`SKU_REFERENCED` or `SKU_TYPE_FROZEN`), or a winning fence
  blocks the reservation with 409 `SKU_FENCED`; both cannot succeed.

**AC #24. Reference identity and release — `fr-reference-registry`**

- **Given** a live logical reference and then its durably cancelled/deleted owner object.
- **When** reserve is repeated while live, then the owner releases it and later makes a new attempt.
- **Then** replay while live returns 200 with the same reservation; the later attempt has a fresh ID, the
  released history remains, and confirming the released ID returns 409 `REFERENCE_RELEASED`.

**AC #25. Confirmation retry and outage — `fr-reference-registry`**

- **Given** a Pricing write using the reservation protocol.
- **When** Products is unavailable before reserve, or confirmation fails after the Pricing transaction commits.
- **Then** the former returns 503 `REGISTRY_UNAVAILABLE` with no object written; the latter preserves the
  object with `confirmation_pending = true` and retries confirmation durably without releasing the reservation;
  confirming an already confirmed reservation succeeds with 200.

**AC #26. Operator release — `fr-reference-registry`, `nfr-audit`**

- **Given** an abandoned reservation visible on the SKU card.
- **When** an operator releases it with `force: true` and a reason.
- **Then** release records the actor/reason, audit and `ReferenceForceReleased`; without force or a reason,
  operator release is refused, and the reservation remains live.

**AC #27. Stale PATCH and replay — `fr-concurrency-idempotency`**

- **Given** a stale SKU/category concurrency version and a retained keyed POST result.
- **When** a caller patches with stale `If-Match`, or replays that POST with the same tenant, endpoint and key
  inside the 24-hour retention window.
- **Then** the patch fails with `STALE_REVISION`; replay returns the stored outcome before fence/unit work and
  creates no additional unit, version or reservation.

**AC #28. Deny by default — `nfr-authz`**

- **Given** a caller lacking the permission for a read, authoring, submission, approval or settings operation.
- **When** the caller invokes that operation.
- **Then** access is denied and no protected content is exposed or changed; granting approve alone still allows
  an independent reviewer to decide an eligible unit without submit permission.

**AC #29. Two storage backends — `nfr-two-backends`**

- **Given** the same registry and approval scenarios on SQLite and Postgres.
- **When** the storage verification suites run, including concurrent reservation/fence and unit-update cases.
- **Then** both enforce the same uniqueness, timeline, audit, replay and reference invariants without row locks.

**AC #30. Derived usage type versions — `fr-derived-usage-type`**

- **Given** a tenant whose usage-type catalog resolves a RAM and a CPU usage type, and the cloudlet declaration over them.
- **When** an author creates the derived usage type and later adds a version with another formula.
- **Then** versions 1 and 2 both read back, version 1 unchanged, each with its stored digest and its meter id
  `products.derived/<code>@<n>`; a declaration that breaks a rule is refused with the rule named, an input the catalog does
  not know is refused, each create and version is audited, and another tenant reads neither (P-D-231).

**AC #31. A usage SKU pins a derived version — `fr-derived-usage-type`**

- **Given** the tenant's derived usage type with versions 1 and 2, and a usage-type catalog that is configured or not.
- **When** an author creates a usage SKU on version 1 selling its output unit, publishes it, and then proposes changes.
- **Then** the draft saves and publishes without the catalog being asked; an unknown version and another unit are
  refused at draft save; the draft may move to version 2 before its first publish; after it, a change to version 2, to a
  GTS ref, from a GTS ref, or one that drops the ref, is refused at submit and again at apply (P-D-232). A published raw
  meter may move onto the identity wrapper of that meter, in the same unit (P-D-251).

**AC #32. A derived meter sells through pricing — `fr-derived-usage-type`**

- **Given** the cloudlet derived usage type, a published usage SKU on its version 1 with its output unit and its invoice
  fields, and pricing running beside the registry with no other meter provider.
- **When** a pricing author creates a usage entry whose policy names the meter, the output unit and the version's accrual,
  prices it, publishes a plan revision, and a sale is checked.
- **Then** the sale is accepted; a policy with another unit or another accrual is refused `METER_POLICY_MISMATCH`, a raw
  meter is answered as unconfigured, another tenant's type is unknown, and a store outage is 503 (P-D-233).

## 10. Dependencies

| Dependency | Description | Criticality |
| --- | --- | --- |
| Pricing | Reads SKU versions and consumes `SkuChanged`; owns the reserve/write/confirm and release protocol. Products serves reference reads locally, and answers pricing's meter semantics for derived meters (P-D-233). | `p1` |
| `bss-approval` | Shared policy, unit, item and decision model plus subject contract; each gear owns its stored copies. | `p1` |
| Usage-type catalog port | Resolves usage meter references; retained as the pluggable integration in spec §4 and §15. | `p1` |
| Toolkit REST, authz and SecureORM | Existing API infrastructure, deny-by-default permissions, scoped storage and transactional writes. | `p1` |
| Outbox and audit infrastructure | Persists domain events and terminal approval records with their state changes. | `p1` |
| SQLite and Postgres harnesses | Verify the same storage semantics on both supported backends. | `p1` |

## 11. Assumptions

- Categories and settings change directly without approval (spec §4); policy quorum is tenant-wide with an
  optional per-kind override (spec §14).
- Pricing obeys the reservation protocol and owns durable confirmation retries and release after cancellation
  or deletion (spec §13).
- Consumers bind descriptors using the SKU version in force at a period's start and retain prior bindings;
  Rating and Subscriptions implement their adaptations in separate plans (spec §2 decision 9, §7.1).
- The new migration chain starts from zero without migrating deployed data (spec §2 decision 1).

## 12. Risks

| Risk | Consequence and specified response |
| --- | --- |
| Pricing integration arrives after Products | In phase 1, registry counts cannot establish completeness for Pricing writes that do not yet reserve. Phase 2 must implement the write path before the programme's first merge gate; a zero count is not evidence of integrated safety (spec §11, §13). |
| Products unavailable before a Pricing write | Pricing fails with 503 `REGISTRY_UNAVAILABLE` and writes nothing (spec §13). |
| Caller dies or confirmation is unavailable after commit | The live reservation continues blocking fences. Pricing retries confirm durably; the card exposes abandoned reservations for explicit release (spec §12–§13). |
| Owner or operator releases beneath a live object | Products cannot detect that misuse itself. Owner release requires durable cancellation/deletion; force-release is audited and evented so the owner can verify and re-reserve (spec §4, §13). |
| Reviewer acts on changed content | Generation checks and a committed stale refresh prevent unseen content reaching quorum (spec §2.2, §6). |
| Consumer reads latest content instead of the dated version | A future-effective change could reach a period early; bindings must use the version-in-force read (spec §2.2, §7.1). |

## 13. Open Questions

The cross-gear barrier is decided: use reservations, not a remote reference count (spec §13).
The spec uses both `asOf` (§2.2, §4) and `as_of` (§7.2) for the version-date query parameter. Settled: the dated
read is `GET /skus/{id}/versions/as-of?date=<date>`, one version at its own path, and the history is always an
array (P-D-214). No other product decision is open in this task's scope.

## 14. Traceability

The content source is `docs/superpowers/specs/2026-09-24-pricebook-model-design.md`, read from
`/Users/alexey/Projects/diffora/gears-rust/docs/superpowers/specs/2026-09-24-pricebook-model-design.md`.
Task 3 of `docs/superpowers/plans/2026-09-24-pricebook-phase0-docs.md` fixes this artifact's identifiers,
headings, error-code vocabulary and interim SDK/browse interface boundary. All identifiers are defined once
in their actor, requirement or use-case blocks above.

| Requirements | Source |
| --- | --- |
| `fr-sku-define`, `fr-sku-type-frozen`, `fr-category-flat` | Spec §2 decisions 3, 12 and 17; §3 D; §4. |
| `fr-sku-descriptors`, `fr-sku-versions` | Spec §2 decision 14; §2.2; §4; §7.1; §8; §12. |
| `fr-sku-metering`, `fr-sku-bundle` | Spec §3 D; §4; §5; §15. |
| `fr-derived-usage-type` | Owner decision of 2026-10-01 (P-D-229; rating T-D-39); P-D-230; P-D-231; P-D-232; P-D-233. |
| `fr-sku-lifecycle`, `fr-sku-retire-fenced`, `fr-reference-registry` | Spec §2 decision 17; §2.2; §4; §6; §7.2; §13. |
| `fr-approval-units`, `fr-concurrency-idempotency` | Spec §2 decision 8; §2.2; §3 items 23 and 27; §6; §7.2; §14. |
| `fr-events`, `fr-read-model` | Spec §3 item 43; §4; §6; §7.3; §12–§13. |
| `nfr-authz`, `nfr-audit`, `nfr-tenant-isolation`, `nfr-two-backends` | Spec §2 decision 10; §2.2; §3 item 27; §4–§6; §7.3; §10. |
| `usecase-change-gl-code`, `usecase-retire-sku` | Spec §8 and §2.2/§4/§6/§13, respectively. |
