<!-- CONFLUENCE_TITLE: [BSS]: Products — Derived Usage Types (Feature) -->
<!-- Related: ../DECOMPOSITION.md, ../DESIGN.md, ../PRD.md, ../DECISIONS.md | Owners: BSS Product Catalog team -->

# Feature: Derived Usage Types

- [ ] `p1` - **ID**: `cpt-cf-bss-products-featstatus-derived-usage-types-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-bss-products-feature-derived-usage-types`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Author declares a derived usage type](#author-declares-a-derived-usage-type)
  - [Pricing author reads a version](#pricing-author-reads-a-version)
  - [Author sells a derived version through a usage SKU](#author-sells-a-derived-version-through-a-usage-sku)
  - [Pricing resolves a derived meter's semantics](#pricing-resolves-a-derived-meters-semantics)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [declaration-judged](#declaration-judged)
  - [inputs-resolve](#inputs-resolve)
  - [version-append](#version-append)
  - [sku-binding](#sku-binding)
  - [pin-holds](#pin-holds)
  - [meter-semantics](#meter-semantics)
- [4. States (CDSL)](#4-states-cdsl)
  - [Derived usage type states](#derived-usage-type-states)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Append-only store on both backends](#append-only-store-on-both-backends)
  - [Declarations judged by the SDK's rules](#declarations-judged-by-the-sdks-rules)
  - [Five doors with their grants and audit](#five-doors-with-their-grants-and-audit)
  - [A usage SKU pins its derived version](#a-usage-sku-pins-its-derived-version)
  - [Products answers pricing's derived meter semantics](#products-answers-pricings-derived-meter-semantics)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

This feature stores and serves derived usage types: composite meters, such as a cloudlet-hour computed from RAM and CPU usage,
declared as versioned catalog data (P-D-229). The declaration, its grammar, its evaluator and its canonical bytes are the
SDK's (`bss_products_sdk::derived`, P-D-230); this feature adds the storage, the doors, the grants and the audit (P-D-231),
a usage SKU's binding to a version, pinned at its first publish (P-D-232), with one later move onto the identity
wrapper of a raw meter (P-D-251), and the meter semantics Products answers to
pricing for a derived meter, E1b of pricing's E1 (P-D-233). It has no design slice of its own: [DESIGN](../DESIGN.md)
§3.1, §3.3, §3.5 and §3.7 are its design, and [DECOMPOSITION](../DECOMPOSITION.md) entry 2.5 places it.

### 1.2 Purpose

Let a catalog author declare a derived usage type once per formula, keep every version immutable, sell a version through a
usage SKU that keeps it from its first publish, give a pricing author the exact meter reference, unit and accrual policy
version a usage policy names, and answer pricing's gates for that meter.

Requirements: `cpt-cf-bss-products-fr-derived-usage-type`.

### 1.3 Actors

`cpt-cf-bss-products-actor-catalog-admin`, `cpt-cf-bss-products-actor-pricing`, `cpt-cf-bss-products-actor-auditor`. The
writes ask `author` on the resource `derived_usage_type`; the reads ask `sku:read` (O-3).

### 1.4 References

- [PRD](../PRD.md): `fr-derived-usage-type`, AC #30, AC #31 and AC #32.
- [DESIGN](../DESIGN.md): §3.1 (the derived usage declaration, type and pin), §3.3 (the doors and the SKU doors' derived
  codes), §3.5 (the catalog port's derived sibling and pricing's meter semantics), §3.7 (the tables).
- [DECISIONS](../DECISIONS.md): P-D-229, P-D-230, P-D-231, P-D-232, P-D-233, P-D-251, P-D-257, P-D-261, P-D-262; pricing D-503 (amended).
- The plan: `docs/superpowers/plans/2026-10-01-products-derived-usage-types.md` in the main checkout, rev 3, runs 1 to 4.

## 2. Actor Flows (CDSL)

### Author declares a derived usage type

- [ ] `p1` - **ID**: `cpt-cf-bss-products-flow-derived-usage-types-author-declares`

1. [ ] - `p1` - Author posts `{code, name, declaration}`; authenticate, ask `author` on `derived_usage_type` anchored to the caller's tenant, and look up an optional `Idempotency-Key` before anything else is judged - `inst-derived-create-input`
2. [ ] - `p1` - Judge the code and the name, then the declaration (algorithm declaration-judged), then each input (algorithm inputs-resolve) - `inst-derived-create-judge`
3. [ ] - `p1` - In one transaction, insert the type and its version 1 with the stored digest, write the audit row and record the replay answer; a taken code is 409 DERIVED_CODE_TAKEN - `inst-derived-create-commit`
4. [ ] - `p1` - Author posts `{declaration}` to the type's versions; an unknown code is 404 before the catalog is asked; the version is appended by algorithm version-append - `inst-derived-version-input`

### Pricing author reads a version

- [ ] `p1` - **ID**: `cpt-cf-bss-products-flow-derived-usage-types-pricing-reads`

1. [ ] - `p1` - Ask `sku:read` (403 denied, 503 unreachable); the SQL filter is `tenant_only()` of that scope beside the caller's tenant, so a SKU `resource_id` does not select a derived row, and a constraint with no `owner_tenant_id` is deny-all - `inst-derived-read-scope`
2. [ ] - `p1` - List the tenant's types by code, 50 to a page and at most 200, with a cursor. Each item carries `latest_version` and `latest`, the latest version in the version-read shape (the declaration, including its formula, the digest, the meter reference, the canonical unit, the accrual policy version, the creator and the time), from one grouped read of the page (P-D-257); read one type with its versions' headers - `inst-derived-read-list`
3. [ ] - `p1` - Read one version: the declaration, the stored digest, `meter_ref` `{usage_type_id: "products.derived/<code>@<n>", version: "<n>"}`, `canonical_unit` and `accrual_policy_version` `derived-v1:<digest>`; a non-canonical `n`, an unknown code or version, and another tenant's type are 404 - `inst-derived-read-version`

### Author sells a derived version through a usage SKU

- [ ] `p1` - **ID**: `cpt-cf-bss-products-flow-derived-usage-types-sku-pins`

1. [ ] - `p1` - Author creates or edits a usage draft with `usage_type_ref = "products.derived/<code>@<n>"` and the version's output unit; the ref is judged by algorithm sku-binding before any catalog is asked, configured or not - `inst-derived-sku-draft`
2. [ ] - `p1` - A draft that was never published may move to another version by PATCH, judged again - `inst-derived-sku-repin`
3. [ ] - `p1` - Submit and approve resolve the ref from the store, never the catalog; the publish rule judges the binding at submit and at apply, and the first publish pins the version - `inst-derived-sku-publish`
4. [ ] - `p1` - A change of the published SKU is judged by algorithm pin-holds, at submit and again at apply - `inst-derived-sku-change`

### Pricing resolves a derived meter's semantics

- [ ] `p1` - **ID**: `cpt-cf-bss-products-flow-derived-usage-types-meter-semantics`

1. [ ] - `p1` - A pricing author names `MeterRef { usage_type_id: "products.derived/<code>@<n>", version: "<n>" }`, the version's output unit and its `derived-v1:<digest>` accrual in a usage entry's policy; pricing asks the ClientHub's one `UsageMeterSemanticsV1` as its door's caller - `inst-derived-meter-ask`
2. [ ] - `p1` - Products' dispatcher answers by algorithm meter-semantics; pricing compares the answer with the policy and the SKU's ref and unit, at entry create, price and plan-revision submit and apply, and at a sale's check - `inst-derived-meter-gates`
3. [ ] - `p1` - A refusal stays the dispatcher's: 400 METER_POLICY_MISMATCH or METER_VERSION_UNKNOWN, 403, and 503 for an outage, which pricing answers 503 - `inst-derived-meter-refusals`

## 3. Processes / Business Logic (CDSL)

### declaration-judged

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-derived-usage-types-declaration-judged`

1. [ ] - `p1` - Parse the string-typed wire declaration into the SDK's types; an unknown granularity, fold, round mode or operator, a decimal that does not parse, or a node whose fields are not its operator's is 400 DERIVED_DECLARATION_INVALID naming the shape's rule - `inst-derived-shape`
2. [ ] - `p1` - Run the SDK's `validate`; its refusal is 400 DERIVED_DECLARATION_INVALID, the detail led by the rule of its variant - `inst-derived-rules`
3. [ ] - `p1` - Store the declaration as the doors serve it, and its digest: the SHA-256 of the SDK's canonical bytes through `aws-lc-rs`, taken once - `inst-derived-digest`

### inputs-resolve

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-derived-usage-types-inputs-resolve`

1. [ ] - `p1` - Ask the `UsageTypeCatalog` port once per input, as the caller, in input order, outside any transaction - `inst-derived-inputs-ask`
2. [ ] - `p1` - A refusal of the caller is 403 USAGE_TYPE_FORBIDDEN, an outage or an unconfigured catalog 503 USAGE_TYPE_UNAVAILABLE, and otherwise every unresolved input one 400 USAGE_TYPE_UNRESOLVED on its ref - `inst-derived-inputs-answer`

### version-append

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-derived-usage-types-version-append`

1. [ ] - `p1` - In the write's transaction, find the type, take its latest version and insert n + 1; a lost race on the number is 409 CONTENDED - `inst-derived-append-number`
2. [ ] - `p1` - Write the audit row `derived_usage_type.version` with the type's id and n in the same transaction; an audit failure rolls the version back - `inst-derived-append-audit`

### sku-binding

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-derived-usage-types-sku-binding`

1. [ ] - `p1` - A ref that starts with the reserved prefix `products.derived/` is derived, whatever follows; it is judged first, before the unconfigured catalog's early answer and before any catalog call - `inst-derived-binding-first`
2. [ ] - `p1` - Parse the meter id; read the type by code and the version under the caller's tenant; a non-canonical id, an unknown code or version, and another tenant's type are one 400 DERIVED_USAGE_TYPE_UNKNOWN on `usage_type_ref` - `inst-derived-binding-version`
3. [ ] - `p1` - A unit the SKU names that is not the version's output unit is 400 DERIVED_UNIT_MISMATCH on `unit`; a draft may leave its unit for later, and its publish then needs one (USAGE_NEEDS_METER) - `inst-derived-binding-unit`

### pin-holds

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-derived-usage-types-pin-holds`

1. [ ] - `p1` - Compare the head's ref and unit with the proposed ref and unit. When the head is a raw GTS id and the proposed ref is the identity wrapper of that meter (one input, that ref whole-string, the same unit, which the change does not move), allow it (P-D-251). Otherwise refuse METERING_IMMUTABLE when the ref moves, the unit moves, or the type leaves usage (P-D-258): on `usage_type_ref` when the ref moves and on `unit` when only the unit moves - `inst-derived-pin-compare`
2. [ ] - `p1` - At submit the change door refuses it before resolving the proposal (400), and the subject again in the transaction; at apply the subject judges the head it finds (409) - `inst-derived-pin-when`

### meter-semantics

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-derived-usage-types-meter-semantics`

1. [ ] - `p1` - A meter whose id does not start with `products.derived/` is raw (E1a): answer exactly `UnconfiguredMeterSemantics`, asking neither the PDP nor the store - `inst-derived-meter-dispatch`
2. [ ] - `p1` - Refuse a nil tenant or subject 403; ask `sku:read` (403 denied, 503 unreachable) - `inst-derived-meter-caller`
3. [ ] - `p1` - Parse the meter id; a `version` that is not canonical or disagrees with `@<n>` is 400 METER_POLICY_MISMATCH on `meter.version` - `inst-derived-meter-version`
4. [ ] - `p1` - Read the type and the version in the caller's tenant, the store's key, beside `tenant_only()` of the `sku:read` scope; an unknown code or version, another tenant's type and an id that names no meter are one 400 METER_VERSION_UNKNOWN on `meter`; a store failure is 503, a row that does not read is a data-loss 500 with detail `a stored derived meter row does not read` (pricing forwards that 500; every other provider 5xx is 503) - `inst-derived-meter-read`
5. [ ] - `p1` - Answer the meter as asked, the version's output unit, `Sum`, `derived-v1:<stored digest>`, source integrated, and the stored digest, never one recomputed - `inst-derived-meter-answer`

## 4. States (CDSL)

### Derived usage type states

- [ ] `p1` - **ID**: `cpt-cf-bss-products-state-derived-usage-types`

1. [ ] - `p1` - A type has no lifecycle: create gives it version 1, and no door renames, retires or deletes it (O-1).
2. [ ] - `p1` - A version is immutable from its insert: the storage refuses every update and delete; a new formula is a new version.
3. [ ] - `p1` - A usage SKU's metering moves only while the SKU is a draft that was never published, except a published raw meter moving onto the identity wrapper of that meter in the same unit (P-D-251); its first publish otherwise fixes the ref and the unit (P-D-258), and a new formula is sold through a new usage SKU (M1).

## 5. Definitions of Done

These definitions own this feature's 5 DoDs. Design constraints: `cpt-cf-bss-products-constraint-two-backends`.

### Append-only store on both backends

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-derived-usage-type-store`

Verified by `m20261001_000012_derived_usage_type_tests::a_version_refuses_update_and_delete`,
`m20261001_000012_derived_usage_type_tests::a_version_cannot_name_another_tenants_type`,
`m20261001_000012_derived_usage_type_tests::the_checks_and_the_code_index_hold`,
`postgres_derived_usage_type::a_version_refuses_update_and_delete`,
`postgres_derived_usage_type::a_version_cannot_name_another_tenants_type`,
`derived_usage_type_repo_tests::a_type_and_its_versions_read_back_as_written`,
`derived_usage_type_repo_tests::the_code_is_unique_per_tenant` and
`derived_usage_type_repo_tests::another_tenant_reads_nothing`. Implementation markers in
`products/src/infra/storage/migrations/m20261001_000012_derived_usage_type.rs` and
`products/src/infra/storage/repo/derived_usage_type_repo.rs`.

Migration 000012 creates the type and version tables on SQLite and Postgres: the code unique per tenant, a tenant-qualified
foreign key from a version to its type, CHECKs on the code, the version and the digest, and a trigger refusing every update
and delete of a version on both engines. The repository reads and writes under the caller's `AccessScope` and tenant, and
maps the code index and the version key to typed refusals (DESIGN §3.7; P-D-231).

### Declarations judged by the SDK's rules

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-derived-usage-type-rules`

Verified by `bss_products_sdk::derived::tests` (the declaration, the grammar and the evaluator),
`domain::derived_tests::every_declaration_error_names_a_rule_of_its_own`,
`domain::derived_tests::an_sdk_refusal_is_derived_declaration_invalid_naming_its_rule`,
`domain::derived_tests::the_digest_is_the_sha256_of_the_canonical_bytes`,
`derived_usage_types_tests::every_declaration_refusal_is_derived_declaration_invalid_naming_its_rule`
and `derived_usage_types_tests::the_catalog_answers_unresolved_400_unreachable_503_denied_403`.
Implementation marker in `products/src/domain/derived.rs`.

A declaration is refused with 400 DERIVED_DECLARATION_INVALID naming its rule, one per SDK variant plus the wire shape's;
the digest is the SHA-256 of the canonical bytes, stored and never recomputed; the inputs resolve through the usage-type
catalog as the caller (403, 503, 400); the SDK's caps are tied to the gear's by const asserts (DESIGN §3.1; P-D-230, P-D-231).

### Five doors with their grants and audit

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-derived-usage-type-doors`

Verified by `derived_usage_types_tests::create_gives_version_1_and_a_new_version_leaves_version_1_as_it_was`,
`derived_usage_types_tests::writes_ask_derived_author_and_reads_ask_sku_read`,
`derived_usage_types_tests::each_create_and_version_writes_one_audit_row_with_the_uuid_subject`,
`derived_usage_types_tests::an_idempotent_replay_answers_the_first_receipt_and_writes_once`,
`derived_usage_types_tests::a_foreign_tenant_reads_nothing`,
`derived_usage_types_tests::the_list_pages_50_by_default_200_at_most_and_walks_by_cursor` and
`derived_usage_types_tests::the_version_read_carries_the_meter_ref_unit_and_accrual`. Implementation marker in
`products/src/api/rest/derived_usage_types.rs`.

Two writes under `author` on `derived_usage_type` and three reads under `sku:read`, registered through OperationBuilder with
503 declared on each and a served text naming every code; writes take an optional `Idempotency-Key` and write one audit row
each in their transaction (`subject_kind = derived_usage_type`, the type's id, the version); the list pages as the SKU list
does and each item carries the latest version in full, from one grouped read (DESIGN §3.3; P-D-231, P-D-257). The list answers 304
when `If-None-Match` matches a weak `ETag` of its JSON, and sends `Cache-Control: private, no-cache`: it names its creators,
so the browser revalidates it, where raw `GET /usage-types`, which names nobody, keeps its minute (P-D-247, P-D-261).
Every read names each `created_by` (`created_by_name`), resolved once per answer
through Account Management, null when it is not available now; the create answers' is null (P-D-262).

### A usage SKU pins its derived version

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-derived-usage-type-pin`

Verified by `derived_binding_tests::a_usage_sku_on_a_derived_version_is_created_submitted_and_approved_without_the_catalog`,
`derived_binding_tests::an_unknown_derived_version_is_refused_at_draft_save`,
`derived_binding_tests::a_unit_other_than_the_output_unit_is_refused`,
`derived_binding_tests::a_draft_moves_its_derived_pin_until_its_first_publish`,
`derived_binding_tests::after_its_first_publish_a_usage_sku_keeps_its_derived_pin`,
`derived_binding_tests::a_published_gts_usage_sku_cannot_take_a_derived_pin`,
`derived_binding_tests::a_published_raw_usage_sku_moves_onto_the_identity_wrapper_of_its_meter`,
`derived_binding_tests::a_stale_change_is_refused_at_apply_when_a_concurrent_write_pinned_a_derived_type`,
`derived_binding_tests::a_published_usage_sku_keeps_its_metering`,
`domain::derived_tests::a_derived_ref_binds_to_its_version_and_its_output_unit` and
`domain::derived_tests::the_pin_moves_only_between_gts_refs` and
`domain::derived_tests::a_published_usage_sku_keeps_its_ref_and_its_unit` and
`domain::derived_tests::wraps_is_the_identity_of_that_one_raw_meter_in_the_sku_unit`. Implementation markers in `products/src/domain/derived.rs`,
`products/src/domain/approvals/change.rs` and `products/src/api/rest/governance.rs`.

A usage SKU's `products.derived/<code>@<n>` ref is judged from the tenant's store first, at draft save, submit and apply,
and the catalog is never asked for it: 400 DERIVED_USAGE_TYPE_UNKNOWN or DERIVED_UNIT_MISMATCH. A raw ref is 400
DERIVED_USAGE_TYPE_REQUIRED before the catalog is asked (P-D-259). The unit is the version's output unit and is not
stored on the SKU. A published usage SKU keeps its ref and that unit, except a raw meter moving onto the identity
wrapper of that meter in the same unit (P-D-251), and every other change that moves the ref or the unit, or that
changes the type away from usage, is METERING_IMMUTABLE at submit (400) and at apply (409) (P-D-258). The codes name
the SKU (DESIGN §3.1, §3.3, §3.5; P-D-232).

### Products answers pricing's derived meter semantics

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-derived-meter-semantics`

Verified by `meter_semantics_tests::a_derived_meter_answers_its_stored_version`,
`meter_semantics_tests::a_raw_meter_answers_exactly_as_an_absent_provider`,
`meter_semantics_tests::a_version_off_the_id_is_a_policy_mismatch`,
`meter_semantics_tests::an_unknown_code_version_or_tenant_is_one_answer`,
`meter_semantics_tests::a_store_failure_is_503_and_a_corrupt_row_500`,
`meter_semantics_tests::a_caller_without_a_tenant_or_a_subject_is_403`,
`meter_semantics_tests::a_denied_sku_read_is_403_and_an_unreachable_pdp_503`,
`meter_semantics_tests::the_callers_tenant_is_pinned`,
`meter_semantics_tests::the_hubs_meter_semantics_is_products_dispatcher_after_the_gears_init` and
`derived_meter_e2e::a_cloudlet_sells_through_pricing_on_products_meter_semantics`. Implementation marker in
`products/src/infra/meter_semantics.rs`.

The gear's init registers one `dyn UsageMeterSemanticsV1` in the ClientHub, as it registers `PricingReferenceRegistry`. It
answers a derived meter from the store, in the caller's tenant under `sku:read`: the output unit, `Sum`,
`derived-v1:<stored digest>`, source integrated and the stored digest; 400 METER_POLICY_MISMATCH for a version off the id,
one 400 METER_VERSION_UNKNOWN for an unknown code, version or tenant, 503 for a store failure, a data-loss 500 with detail `a stored derived meter row does not read` for a row that does not read, 403 for a denial. Every other
meter is answered as an absent provider would; the raw-meter provider (E1a) is an extension point, not built (DESIGN §3.5;
P-D-233).

## 6. Acceptance Criteria

Each criterion below corresponds to exactly one DoD above and cites [PRD §9](../PRD.md#9-acceptance-criteria).

| DoD | PRD trace | Given / When / Then |
| --- | --- | --- |
| `cpt-cf-bss-products-dod-derived-usage-type-store` | AC #29, #30; `cpt-cf-bss-products-fr-derived-usage-type` | Given a stored version, when anything updates or deletes it on either engine, then the storage refuses; a version naming another tenant's type is refused by its key, and a second type with the tenant's code by its index. |
| `cpt-cf-bss-products-dod-derived-usage-type-rules` | AC #30; `cpt-cf-bss-products-fr-derived-usage-type` | Given a declaration that breaks a rule, when it is created or added as a version, then it is 400 DERIVED_DECLARATION_INVALID naming the rule and nothing is written; given the cloudlet, its stored digest is the SHA-256 of its canonical bytes. |
| `cpt-cf-bss-products-dod-derived-usage-type-doors` | AC #28, #30; `cpt-cf-bss-products-fr-derived-usage-type` | Given an author with `author` on `derived_usage_type`, when the cloudlet is created and a second version added, then both read back with their meter ids and version 1 is unchanged, each write has its audit row, a replayed key answers the first receipt, another tenant reads nothing, and a caller with only `sku:read` reads and cannot write. |
| `cpt-cf-bss-products-dod-derived-usage-type-pin` | AC #31; `cpt-cf-bss-products-fr-derived-usage-type` | Given the tenant's derived type with versions 1 and 2 and a catalog configured or not, when a usage SKU is created on version 1 with its output unit, published and then changed, then the catalog is never asked; an unknown version or another unit is refused at draft save; the draft may move to version 2 before its first publish; after it, a change to version 2, to or from a GTS ref, or one that drops the ref is refused at submit (400) and at apply (409). A published raw meter may move onto the identity wrapper of that meter, in the same unit (P-D-251). |
| `cpt-cf-bss-products-dod-derived-meter-semantics` | AC #32; `cpt-cf-bss-products-fr-derived-usage-type` | Given the cloudlet type, a published usage SKU on its version 1 and pricing beside the registry with no other meter provider, when a usage entry names the meter, its output unit and its accrual, and a price, a plan revision and a sale follow, then the sale is accepted through pricing's gates; another unit or accrual is METER_POLICY_MISMATCH, a raw meter is unconfigured, another tenant's type is unknown, and a store outage is 503 at the entry create. |
