<!-- CONFLUENCE_TITLE: [BSS]: Products — Quota Catalog (Feature) -->
<!-- Related: ../DECOMPOSITION.md, ../DESIGN.md, ../PRD.md, ../DECISIONS.md | Owners: BSS Product Catalog team -->

# Feature: Quota Catalog

- [ ] `p1` - **ID**: `cpt-cf-bss-products-featstatus-quota-catalog-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-bss-products-feature-quota-catalog`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Administrator onboards quota capabilities](#administrator-onboards-quota-capabilities)
  - [Catalog owner reviews a candidate](#catalog-owner-reviews-a-candidate)
  - [Plan author browses the quota catalog](#plan-author-browses-the-quota-catalog)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [record](#record)
  - [snapshot](#snapshot)
  - [definition](#definition)
- [4. States (CDSL)](#4-states-cdsl)
  - [Quota candidate states](#quota-candidate-states)
- [5. Definitions of Done](#5-definitions-of-done)
  - [The quota contract in products-sdk](#the-quota-contract-in-products-sdk)
  - [The quota store on both backends](#the-quota-store-on-both-backends)
  - [Discovery intake and manual records](#discovery-intake-and-manual-records)
  - [Review with separation of duties](#review-with-separation-of-duties)
  - [Catalog reads and the SDK client](#catalog-reads-and-the-sdk-client)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

This feature is Products' implementation of the quotas package's feature 1, quota catalog onboarding
([`cpt-cf-bss-quotas-feature-catalog`](../../../docs/quotas/features/0001-catalog.md)), on the contract of the
package's [DESIGN §3.3.1–§3.3.1.3](../../../docs/quotas/DESIGN.md#331-catalog-onboarding-and-plan-publication)
(P-D-266). An administrator binds adapter targets and runs discovery, or authors a capability record by hand.
Products judges every record against types-registry and stages the valid ones as candidates with their snapshot and
registry evidence (P-D-268). A submitter proposes a candidate for one SKU version, and an independent approver
publishes an immutable definition version (P-D-269, P-D-270), unavailable for sale until qualification (P-D-271).
Plan authors and Pricing read the applicable versions through two endpoints and `ProductsQuotaCatalogClientV1`
(P-D-272), under two new resources (P-D-273), on the store of migration 000015 (P-D-274).

Products registers the quota schemas and the discovery plugin specification, and no metric, unit or policy
instance: the adapter owner registers those in its own namespace, and the records reference them (P-D-267). No
discovery adapter exists in this version, so production discovery answers 501 `QUOTA_DISCOVERY_CLOSED` before any
effect. Enabling sale belongs to a later Products feature for the package's feature 5. The feature has no design
slice of its own: [DESIGN](../DESIGN.md) §3.1, §3.2, §3.3, §3.4, §3.5 and §3.7 are its design, and
[DECOMPOSITION](../DECOMPOSITION.md) entry 2.6 places it.

### 1.2 Purpose

Let a catalog administrator turn authorized adapter evidence and manual records into reviewed, versioned quota
definitions; keep every version, its mapping and its profile immutable; record that no version is sellable before
qualification; and give a plan author and Pricing the exact definitions that apply to a SKU version, with their text,
units, bounds, conditions and availability, without a registry call per row.

Requirements: `cpt-cf-bss-products-fr-quota-catalog`. It serves the package's `cpt-cf-bss-quotas-fr-catalog`,
`cpt-cf-bss-quotas-fr-catalog-gts`, `cpt-cf-bss-quotas-fr-catalog-presentation`,
`cpt-cf-bss-quotas-fr-catalog-approval`, `cpt-cf-bss-quotas-fr-catalog-classification`,
`cpt-cf-bss-quotas-fr-catalog-history`, `cpt-cf-bss-quotas-fr-catalog-policies`,
`cpt-cf-bss-quotas-fr-catalog-compatibility` and `cpt-cf-bss-quotas-fr-plan-catalog`.

### 1.3 Actors

`cpt-cf-bss-products-actor-catalog-admin` binds targets, runs discovery, authors records, submits, and approves or
rejects, each under its own grant; `cpt-cf-bss-products-actor-pricing` reads the catalog through the SDK client as its
caller. Targets and discovery ask `author` on `quota_adapter_target`, target reads `read`; a manual record asks
`author` on `quota_definition`, a submit `submit`, an approve or a reject `approve`, and the candidate and definition
reads `read` (P-D-273).

### 1.4 References

- [PRD](../PRD.md): `fr-quota-catalog`, AC #33, AC #34, AC #35, AC #36 and AC #37.
- [DESIGN](../DESIGN.md): §3.1 (the quota catalog: entities, the judgement of a record, digests, approval), §3.2 (the
  quota catalog component), §3.3 (the quota operations, the list query and the SDK methods), §3.4 (products-sdk and
  types-registry), §3.5 (the discovery plugin and types-registry), §3.7 (migration 000015).
- [DECISIONS](../DECISIONS.md): P-D-266, P-D-267, P-D-268, P-D-269, P-D-270, P-D-271, P-D-272, P-D-273, P-D-274;
  P-D-225 (text caps), P-D-265 (the caller's tenant only).
- The quotas package: [feature 1](../../../docs/quotas/features/0001-catalog.md) (flow `cpt-cf-bss-quotas-flow-catalog`,
  algorithm `cpt-cf-bss-quotas-algo-catalog`, DoD `cpt-cf-bss-quotas-dod-catalog`, criteria Q-C37, Q-C38, Q-C57 and
  Q-C67), [DESIGN](../../../docs/quotas/DESIGN.md) §3.3.1–§3.3.1.3 and
  [ADR 0001](../../../docs/quotas/ADR/0001-commercial-quota-catalog.md).

## 2. Actor Flows (CDSL)

### Administrator onboards quota capabilities

- [x] `p1` - **ID**: `cpt-cf-bss-products-flow-quota-catalog-intake`

Implements the package's `cpt-cf-bss-quotas-flow-catalog` steps `inst-discover`, `inst-complete` and
`inst-candidate`, and `cpt-cf-bss-quotas-algo-catalog` step `inst-authorize`.

1. [x] - `p1` - Administrator binds a registered discovery plugin instance and a target key; ask `author` on `quota_adapter_target`; a plugin instance types-registry does not hold under the quota discovery specification is 400 QUOTA_TARGET_PLUGIN_UNKNOWN; the same binding twice is 409 QUOTA_TARGET_TAKEN - `inst-quota-target`
2. [x] - `p1` - Administrator runs discovery on the target; ask `author` on `quota_adapter_target`; resolve the target's plugin client through the scoped ClientHub, and with none registered answer 501 QUOTA_DISCOVERY_CLOSED before any read or write; read the complete snapshot by algorithm snapshot - `inst-quota-discover`
3. [x] - `p1` - Answer a stored revision first by algorithm snapshot's replay, before any record is judged; else judge every record by algorithm record outside any transaction; then, in one transaction, store the snapshot, one candidate per valid record and the review notes, with the audit row, or answer the stored snapshot by algorithm snapshot's replay - `inst-quota-stage`
4. [x] - `p1` - Administrator authors one record by hand for a target; ask `author` on `quota_definition`; judge it by algorithm record; a rejected record is 400 with its code; store it as a manual candidate with its author - `inst-quota-manual`

### Catalog owner reviews a candidate

- [x] `p1` - **ID**: `cpt-cf-bss-products-flow-quota-catalog-review`

Implements the package's `cpt-cf-bss-quotas-flow-catalog` step `inst-review` and `cpt-cf-bss-quotas-algo-catalog`
step `inst-result`.

1. [x] - `p1` - Submitter proposes a staged candidate for one SKU version with plan units, bases and reviewed conditions, under the candidate's `If-Match`; ask `submit` on `quota_definition`; the proposal is judged by algorithm definition; answer the submitted candidate with its new `ETag` - `inst-quota-submit`
2. [x] - `p1` - Approver approves a submitted candidate under its `If-Match`, with a JSON body and an optional note; ask `approve` on `quota_definition`; separation and the build follow algorithm definition, in one transaction with the audit row that reads the named definition's SKU and metric again; answer the candidate and the definition, with no `ETag` - `inst-quota-approve`
3. [x] - `p1` - Approver rejects a submitted candidate with a note under its `If-Match`; a missing note is 400 NOTE_REQUIRED; the submitter and a manual record's author are refused 403 SOD_VIOLATION; answer the rejected candidate with its new `ETag` - `inst-quota-reject`
4. [x] - `p1` - The approved version is recorded unavailable for sale, revision 1 `disabled` with reason `qualification_pending`; no endpoint of this feature enables it - `inst-quota-unavailable`

### Plan author browses the quota catalog

- [x] `p1` - **ID**: `cpt-cf-bss-products-flow-quota-catalog-browse`

Implements the package's `cpt-cf-bss-quotas-flow-catalog` step `inst-browse`.

1. [x] - `p1` - Plan author lists the definitions applicable to one SKU version; ask `read` on `quota_definition`; the filter, availability, locale, order and cursor are DESIGN §3.3's; no registry call is made - `inst-quota-list`
2. [x] - `p1` - Plan author reads one exact version with its current availability; another tenant's or an unknown version is 404; a stored row that does not read is 500 - `inst-quota-get`
3. [x] - `p1` - Pricing reads the same list and version through `ProductsQuotaCatalogClientV1` in the ClientHub, as its caller - `inst-quota-sdk`

## 3. Processes / Business Logic (CDSL)

### record

- [x] `p1` - **ID**: `cpt-cf-bss-products-algo-quota-catalog-record`

Implements the package's `cpt-cf-bss-quotas-algo-catalog` step `inst-identity`.

1. [x] - `p1` - Parse the record into the closed v1 shape: UUID `id`, `type_id`, keys, references, decimal-string limits with a positive step, six explicit operations, explicit change support and an object `properties`; a record that does not parse is rejected QUOTA_RECORD_INVALID - `inst-quota-shape`
2. [x] - `p1` - Check that no segment of `type_id` or of a metric, unit or policy reference carries a minor version, the adapter's segments included, and every `cf.bss.products.quota_*` segment is `v1`; check that `type_id` is the capability schema or a schema derived from it, resolve `type_id` (concrete) and each of its ancestors as registered schemas, resolve the metric, the unit, both policies and the subject and resource types, and check each reference's family; last, validate the record against the live schema; an unknown reference is QUOTA_REFERENCE_UNKNOWN, a wrong family or an abstract type QUOTA_REFERENCE_FAMILY, a version QUOTA_VERSION_UNSUPPORTED, a metric that is not an allocation metric QUOTA_CAPABILITY_UNSUPPORTED, a metric without English text QUOTA_DISPLAY_TEXT_MISSING, a record the schema refuses or a schema that does not compile QUOTA_RECORD_INVALID; a registry outage aborts the intake - `inst-quota-identity`
3. [x] - `p1` - Check the units: each references its canonical unit directly, the canonical unit references itself with 1/1, factors are positive and reduced, and the record's unit and the metric share one canonical unit; else QUOTA_UNIT_INVALID or QUOTA_UNIT_MISMATCH - `inst-quota-units`
4. [x] - `p1` - Check the policies' v1 meaning: allocated capacity summed with English text; the resource owner in one verified scope with required dimensions among `region` and `resource_pool`; else QUOTA_POLICY_INVALID - `inst-quota-policies`
5. [x] - `p1` - Keep the evidence of every resolved entity, its id, UUID, registry resource version and content digest, and the record's digest - `inst-quota-evidence`

### snapshot

- [x] `p1` - **ID**: `cpt-cf-bss-products-algo-quota-catalog-snapshot`

Implements the package's `cpt-cf-bss-quotas-algo-catalog` steps `inst-replay` and `inst-incomplete`.

1. [x] - `p1` - Read pages of the configured size from the target, each bounded by the configured timeout and all by the discovery deadline, following `next_cursor` until it is absent; a page may hold fewer records than asked, every page but the last holds one, and only the record bound limits the read; an adapter error or a late page is 503 QUOTA_DISCOVERY_UNAVAILABLE, a read past the deadline or the bound 503 QUOTA_DISCOVERY_INCOMPLETE - `inst-quota-pages`
2. [x] - `p1` - Verify that every page names the same snapshot id, revision, observation time, count and digest, that no record id repeats, that the records number the count and stay within the configured bound, and that the canonical digest matches; otherwise answer 503 QUOTA_DISCOVERY_INCOMPLETE and store nothing; never read an incomplete snapshot as a removal - `inst-quota-verify`
3. [x] - `p1` - In the intake transaction, find the target's snapshot at that revision: the same digest answers the stored snapshot and its candidates, another digest is 409 QUOTA_SOURCE_CONFLICT, and a revision below the target's highest is 409 QUOTA_SOURCE_STALE - `inst-quota-replay`
4. [x] - `p1` - Record review notes: a capability key that two records name rejects every record that names it; a valid record whose capability has an approved mapping with other digests is drift, an approved mapping whose capability no record names is retired, and every rejected record with its refusal; nothing published changes - `inst-quota-notes`

### definition

- [x] `p1` - **ID**: `cpt-cf-bss-products-algo-quota-catalog-definition`

1. [x] - `p1` - Judge the proposal: a SKU version the tenant holds, a named definition of the same SKU and metric, one to eight plan units of the metric's canonical unit into which the record's bounds convert exactly, the bases `fixed` and/or `per_plan_unit` (the SDK's `Fixed` and `PerPlanUnit`), conditions with English text, the six operations and initial installation supported; pin the registry entities the definition carries at the content intake judged; else the refusal codes of DESIGN §3.3 - `inst-quota-proposal`
2. [x] - `p1` - Refuse an approver or a rejecter who submitted the candidate or authored the manual record, 403 SOD_VIOLATION - `inst-quota-separation`
3. [x] - `p1` - Build the definition version from the stored candidate and proposal only, with no registry call: the mapping and profile versions of the capability's lineage (reused when equal), the next version of the definition, and each digest over its canonical content - `inst-quota-build`
4. [x] - `p1` - Write availability revision 1 `disabled`, reason `qualification_pending`, beside the definition - `inst-quota-availability`

## 4. States (CDSL)

### Quota candidate states

- [x] `p1` - **ID**: `cpt-cf-bss-products-state-quota-candidate`

**States**: staged, submitted, approved, rejected

**Initial State**: staged (a discovered or manual candidate)

**Transitions**:
1. [x] - `p1` - **FROM** staged **TO** submitted **WHEN** a submitter proposes it - `inst-quota-state-submit`
2. [x] - `p1` - **FROM** submitted **TO** approved **WHEN** an independent approver approves it - `inst-quota-state-approve`
3. [x] - `p1` - **FROM** submitted **TO** rejected **WHEN** an independent approver rejects it with a note - `inst-quota-state-reject`

## 5. Definitions of Done

These definitions own this feature's 5 DoDs. Each implements Products' half of the package's
`cpt-cf-bss-quotas-dod-catalog`. Design constraints: `cpt-cf-bss-products-constraint-two-backends`.

### The quota contract in products-sdk

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-quota-catalog-contract`

Verified by `quota::gts_tests::unit_and_policy_schemas_are_final_and_metric_and_capability_are_not`,
`quota::gts_tests::every_reference_is_narrowed_to_its_family`,
`quota::gts_tests::the_ids_are_major_only_v1_and_the_plugin_derives_from_the_toolkit_base`,
`quota::gts_tests::the_package_example_record_parses_and_an_unknown_member_does_not`,
`quota::gts_tests::an_absent_operation_flag_is_refused`,
`quota::gts_tests::the_package_example_instances_parse`,
`quota::gts_tests::each_schema_type_id_is_its_constant`,
`quota::gts_tests::a_malformed_type_id_is_refused_and_other_versions_still_parse`,
`quota::gts_tests::every_reference_is_a_typed_gts_id_at_the_parse`,
`quota::gts_tests::metric_and_capability_are_closed_at_the_top_level_and_open_in_properties`,
`quota::gts_tests::the_unit_and_resource_type_references_are_narrowed`,
`quota::gts_tests::every_enum_property_is_a_flat_enum`,
`quota::gts_tests::a_zero_step_is_outside_the_schema_and_max_has_one_encoding`,
`quota::units_tests::gb_and_gib_reach_bytes_by_their_own_factors`,
`quota::units_tests::a_zero_denominator_is_refused`,
`quota::units_tests::a_factor_that_is_not_reduced_is_refused`,
`quota::units_tests::amounts_are_canonical_decimal_integers`,
`quota::units_tests::an_overflowing_conversion_is_refused_not_wrapped`,
`quota::units_tests::a_fractional_factor_converts_exactly_when_the_result_is_whole`,
`quota::units_tests::every_refusal_names_its_stable_code`,
`quota::digest_tests::the_rendering_sorts_keys_at_every_depth_and_keeps_arrays_in_order`,
`quota::digest_tests::the_snapshot_digest_ignores_item_order_and_excludes_cursor_and_digest`,
`quota::digest_tests::every_member_of_the_snapshot_identity_moves_the_digest`,
`quota::digest_tests::the_kinds_are_domain_separated`,
`quota::digest_tests::the_package_example_snapshot_has_a_pinned_digest`,
`quota::digest_tests::an_item_without_a_string_id_is_refused`,
`quota::digest_tests::an_instant_outside_the_utc_range_is_refused_not_a_panic`,
`quota::digest_tests::the_observation_time_is_hashed_as_utc_with_a_fraction_only_when_non_zero`,
`quota::catalog_tests::quota_version_round_trips_as_a_decimal_string`,
`quota::catalog_tests::the_package_example_row_deserializes_into_a_definition`,
`quota::catalog_tests::a_definition_serializes_its_bases_as_the_package_spells_them` and
`quota::catalog_tests::the_package_example_row_writes_back_unchanged`. Implementation markers in
`products-sdk/src/quota/gts.rs`, `products-sdk/src/quota/units.rs`, `products-sdk/src/quota/discovery.rs`,
`products-sdk/src/quota/digest.rs` and `products-sdk/src/quota/catalog.rs`.

`products-sdk` declares the five quota schemas and the discovery plugin specification, which the toolkit inventory
seeds into types-registry. The unit and policy schemas are final, the metric and capability schemas extensible only
under `properties`, and every reference is narrowed to its family. Exact units refuse a decimal that is not
canonical, a factor that is not positive or not reduced, and an inexact or overflowing conversion. The canonical
rendering and the domain-separated digests are the ones an adapter computes; the package's example snapshot has a
pinned digest. The SDK spells the bases `Fixed` and `PerPlanUnit`, as the quotas package does.
`ProductsQuotaDiscoveryPluginClientV1` and `ProductsQuotaCatalogClientV1` are exported from the crate root (DESIGN
§3.1, §3.4; P-D-267, P-D-272).

### The quota store on both backends

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-quota-catalog-store`

Verified by `m20261119_000015_quota_catalog_tests::applies_replays_and_reverts_on_sqlite`,
`m20261119_000015_quota_catalog_tests::every_evidence_table_refuses_update_and_delete`,
`m20261119_000015_quota_catalog_tests::a_candidate_moves_only_its_review_columns`,
`m20261119_000015_quota_catalog_tests::a_decided_candidate_is_frozen`,
`m20261119_000015_quota_catalog_tests::a_replace_never_deletes_a_stored_row`,
`m20261119_000015_quota_catalog_tests::no_quota_table_keeps_a_rowid`,
`m20261119_000015_quota_catalog_tests::one_snapshot_per_target_and_revision`,
`m20261119_000015_quota_catalog_tests::the_checks_hold`,
`m20261119_000015_quota_catalog_tests::a_child_row_cannot_name_another_tenants_parent`,
`m20261119_000015_quota_catalog_tests::one_candidate_per_snapshot_key_and_manual_record`,
`m20261119_000015_quota_catalog_tests::one_mapping_version_per_lineage_and_version`,
`m20261119_000015_quota_catalog_tests::the_lists_read_through_tenant_led_indexes`,
`m20261119_000015_quota_catalog_tests::down_refuses_while_one_quota_row_exists`,
`m20261119_000015_quota_catalog_tests::down_refuses_while_any_quota_row_exists`,
`migrations_tests::every_migration_reverses_except_the_named_irreversible_ones`,
`quota_intake_repo_tests::a_target_reads_back_and_the_same_binding_is_taken`,
`quota_intake_repo_tests::another_tenant_reads_no_target`,
`quota_intake_repo_tests::the_target_list_pages_by_name_then_id`,
`quota_intake_repo_tests::the_highest_revision_compares_as_a_number`,
`quota_intake_repo_tests::the_highest_revision_reads_only_the_revision`,
`quota_intake_repo_tests::a_snapshot_and_its_candidates_read_back_as_written`,
`quota_intake_repo_tests::a_second_snapshot_at_a_revision_is_refused_as_a_conflict`,
`quota_intake_repo_tests::a_manual_record_is_one_candidate_per_target`,
`quota_intake_repo_tests::a_review_write_moves_once_per_seen_version`,
`quota_intake_repo_tests::a_review_write_moves_only_from_its_own_state`,
`quota_intake_repo_tests::a_stored_candidate_that_does_not_decode_is_corrupt`,
`quota_intake_repo_tests::the_candidate_list_is_newest_first_and_narrows_by_target_and_state`,
`quota_intake_repo_tests::the_candidate_list_walks_its_cursor_across_pages`,
`quota_intake_repo_tests::a_json_column_that_is_not_an_object_is_never_written`,
`quota_catalog_repo_tests::the_definition_list_answers_versions_up_to_the_sku_version`,
`quota_catalog_repo_tests::the_definition_list_filters_by_resource_type`,
`quota_catalog_repo_tests::the_definition_list_filters_by_subject_type_and_profile`,
`quota_catalog_repo_tests::the_current_availability_is_the_highest_revision`,
`quota_catalog_repo_tests::the_list_reads_availability_in_one_statement_per_page`,
`quota_catalog_repo_tests::the_definition_digests_of_a_page_are_one_statement`,
`quota_catalog_repo_tests::the_definition_list_pages_by_definition_then_version`,
`quota_catalog_repo_tests::a_definition_reads_as_stored_and_versions_count_up`,
`quota_catalog_repo_tests::a_definition_without_availability_is_corrupt_not_missing`,
`quota_catalog_repo_tests::the_latest_mapping_of_each_lineage_reads_back`,
`quota_catalog_repo_tests::a_version_beyond_the_stored_range_is_refused_not_wrapped`,
`postgres_quota_catalog::every_evidence_table_refuses_update_and_delete_on_postgres`,
`postgres_quota_catalog::a_candidate_moves_only_its_review_columns_on_postgres`,
`postgres_quota_catalog::a_decided_candidate_is_frozen_on_postgres`,
`postgres_quota_catalog::no_quota_table_can_be_truncated_on_postgres`,
`postgres_quota_catalog::the_checks_and_tenant_keys_hold_on_postgres`,
`postgres_quota_catalog::the_quota_migration_reverts_and_applies_again_on_postgres`,
`postgres_quota_catalog::the_quota_migration_refuses_to_revert_while_a_row_exists_on_postgres`,
`postgres_quota_catalog::a_second_snapshot_at_a_revision_is_refused_as_a_conflict_on_postgres`,
`postgres_quota_catalog::two_concurrent_review_writes_move_one_on_postgres`,
`postgres_quota_catalog::the_highest_revision_compares_as_a_number_on_postgres`,
`postgres_quota_catalog::the_definition_list_answers_versions_up_to_the_sku_version_on_postgres` and
`postgres_quota_catalog::the_definition_digests_answer_each_stored_version_on_postgres`. Implementation markers in
`products/src/infra/storage/migrations/m20261119_000015_quota_catalog.rs`,
`products/src/infra/storage/repo/quota_intake_repo.rs` and `products/src/infra/storage/repo/quota_catalog_repo.rs`.

Migration 000015 creates the seven quota tables on SQLite and Postgres with tenant-led keys, tenant-qualified foreign
keys, the CHECKs, unique and tenant-led listing indexes of DESIGN §3.7, append-only triggers on the six evidence
tables (`TRUNCATE` included on Postgres), and a guard that lets a candidate move only its review columns until it is
decided. On SQLite no `REPLACE` deletes a stored row and every quota table is `WITHOUT ROWID`. The migration's `down`
refuses while any quota row exists, naming each table that holds one, and reverts empty tables. The repositories read
and write under the caller's `AccessScope` and tenant, compare revisions as numbers, move a candidate once per seen
version, answer the definitions applicable up to a SKU version with the page's current availability in one
statement, and map a stored row that does not decode to a corrupt row naming its table, tenant and id (DESIGN §3.7;
P-D-274, P-D-270, P-D-271).

### Discovery intake and manual records

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-quota-catalog-intake`

Verified by `domain::quota::record_tests::a_valid_record_is_judged_with_its_evidence`,
`domain::quota::record_tests::an_unregistered_metric_is_unknown`,
`domain::quota::record_tests::a_unit_named_as_the_metric_is_a_family_refusal`,
`domain::quota::record_tests::a_minor_bearing_reference_is_refused`,
`domain::quota::record_tests::a_minor_bearing_adapter_instance_is_refused`,
`domain::quota::record_tests::a_v2_family_is_unsupported`,
`domain::quota::record_tests::a_type_id_outside_the_capability_family_is_refused`,
`domain::quota::record_tests::a_metric_that_is_not_allocation_is_unsupported`,
`domain::quota::record_tests::a_metric_without_english_text_is_refused`,
`domain::quota::record_tests::a_zero_step_is_refused`,
`domain::quota::record_tests::the_record_unit_and_the_metric_must_share_a_canonical_unit`,
`domain::quota::record_tests::a_registry_outage_is_unavailable_not_a_refusal`,
`domain::quota::record_tests::a_derived_capability_schema_with_validated_properties_is_accepted`,
`domain::quota::record_tests::an_abstract_subject_type_is_a_family_refusal`,
`domain::quota::record_tests::one_run_asks_the_registry_once_per_entity`,
`domain::quota::record_tests::a_unit_converts_directly_into_a_canonical_unit_that_refers_to_itself`,
`domain::quota::record_tests::a_policy_outside_its_v1_meaning_is_refused`,
`domain::quota::record_tests::a_record_outside_the_v1_shape_is_invalid_on_its_field`,
`domain::quota::record_tests::the_platform_registry_judges_references_before_the_schema`,
`domain::quota::record_tests::a_capability_schema_that_does_not_compile_rejects_the_record`,
`domain::quota::record_tests::a_canonical_unit_that_does_not_name_itself_is_invalid`,
`domain::quota::locale_tests::a_missing_locale_falls_back_to_english`,
`domain::quota::locale_tests::a_bad_tag_is_validation`,
`domain::quota::locale_tests::more_than_32_locales_is_too_long`,
`domain::quota::locale_tests::english_is_required_and_not_blank`,
`domain::quota::snapshot_tests::a_complete_snapshot_of_three_pages_is_verified_and_sorted`,
`domain::quota::snapshot_tests::pages_of_two_revisions_leave_intake_unresolved`,
`domain::quota::snapshot_tests::a_repeated_record_id_leaves_intake_unresolved`,
`domain::quota::snapshot_tests::a_count_that_disagrees_leaves_intake_unresolved`,
`domain::quota::snapshot_tests::a_digest_that_disagrees_leaves_intake_unresolved`,
`domain::quota::snapshot_tests::a_cursor_loop_leaves_intake_unresolved`,
`domain::quota::snapshot_tests::more_items_than_the_bound_leave_intake_unresolved`,
`domain::quota::snapshot_tests::short_pages_inside_the_bound_read_complete`,
`domain::quota::snapshot_tests::an_empty_page_that_is_not_the_last_is_incomplete`,
`domain::quota::snapshot_tests::an_adapter_error_is_unavailable_and_carries_no_adapter_text`,
`domain::quota::snapshot_tests::a_page_later_than_the_timeout_is_unavailable`,
`domain::quota::snapshot_tests::the_whole_read_is_bounded_by_the_discovery_deadline`,
`domain::quota::snapshot_tests::every_page_repeats_the_snapshot_identity`,
`domain::quota::intake_tests::drift_is_a_changed_digest_of_an_approved_capability`,
`domain::quota::intake_tests::retired_is_an_approved_capability_absent_from_the_snapshot`,
`domain::quota::intake_tests::a_rejected_record_is_neither_drift_nor_retired`,
`domain::quota::intake_tests::a_capability_key_claimed_twice_rejects_both_records`,
`domain::quota::intake_tests::a_capability_key_named_by_a_rejected_record_rejects_the_valid_one_too`,
`domain::quota::intake_tests::revisions_order_as_numbers`,
`infra::quota::registry_tests::an_instance_reads_with_its_type_and_resource_version`,
`infra::quota::registry_tests::an_unknown_instance_is_none_and_an_outage_is_unavailable`,
`infra::quota::registry_tests::without_a_registry_in_the_hub_every_read_is_unavailable`,
`infra::quota::registry_tests::the_well_known_instances_validate_against_their_schemas`,
`infra::quota::registry_tests::a_record_the_capability_schema_refuses_is_invalid`,
`infra::quota::registry_tests::a_derived_capability_schema_validates_through_its_ancestors`,
`infra::quota::registry_tests::a_live_schema_that_does_not_compile_is_a_verdict_not_an_outage`,
`infra::quota::discovery_tests::a_registered_scoped_client_resolves_and_none_is_closed`,
`api::rest::quota::intake_tests::the_same_snapshot_twice_is_one_snapshot_and_answers_the_stored_candidates`,
`api::rest::quota::intake_tests::other_content_at_the_same_revision_is_a_conflict_and_writes_nothing`,
`api::rest::quota::intake_tests::a_lower_revision_is_stale`,
`api::rest::quota::intake_tests::staging_asks_no_registry_inside_its_transaction`,
`api::rest::quota::intake_tests::a_replay_inside_the_transaction_runs_no_insert`,
`api::rest::quota::intake_tests::a_stored_lower_revision_replays_after_a_higher_one`,
`api::rest::quota::intake_tests::a_staged_snapshot_keeps_its_records_evidence_and_notes`,
`api::rest::quota::targets_tests::a_target_is_created_read_and_listed`,
`api::rest::quota::targets_tests::a_plugin_instance_of_another_type_is_refused`,
`api::rest::quota::targets_tests::the_same_binding_twice_is_taken`,
`api::rest::quota::targets_tests::another_tenant_cannot_read_or_discover_a_target`,
`api::rest::quota::targets_tests::a_denied_caller_is_403_before_the_body_is_read`,
`api::rest::quota::targets_tests::every_quota_request_asks_for_the_callers_tenant_only`,
`api::rest::quota::targets_tests::a_target_refusal_names_the_adapter_target`,
`api::rest::quota::targets_tests::a_target_list_cursor_from_another_tenant_is_refused`,
`api::rest::quota::targets_tests::a_target_outside_its_caps_is_refused`,
`api::rest::quota::targets_tests::a_registry_outage_binds_no_target`,
`api::rest::quota::targets_tests::a_keyed_target_create_replays_its_first_answer`,
`api::rest::quota::dto_tests::every_sdk_enum_maps_to_its_rest_mirror`,
`api::rest::quota::discovery_tests::a_complete_snapshot_stages_one_candidate_per_valid_record`,
`api::rest::quota::discovery_tests::the_same_snapshot_again_recovers_the_stored_candidates`,
`api::rest::quota::discovery_tests::a_replay_answers_while_the_registry_is_down`,
`api::rest::quota::discovery_tests::other_content_at_the_same_revision_is_a_source_conflict`,
`api::rest::quota::discovery_tests::a_registry_outage_stores_nothing_and_answers_503`,
`api::rest::quota::discovery_tests::an_incomplete_snapshot_stores_nothing_and_answers_503`,
`api::rest::quota::discovery_tests::a_changed_registry_record_is_reported_as_drift_and_leaves_the_definition`,
`api::rest::quota::discovery_tests::a_capability_missing_from_a_complete_snapshot_is_a_retired_note`,
`api::rest::quota::discovery_tests::without_an_adapter_discovery_is_closed_before_any_effect`,
`api::rest::quota::discovery_tests::a_keyed_discovery_replays_its_first_answer`,
`api::rest::quota::discovery_tests::a_stale_revision_is_refused_before_any_record_is_judged`,
`api::rest::quota::discovery_tests::a_discovery_past_its_deadline_stores_nothing`,
`api::rest::quota::discovery_tests::judging_past_the_discovery_deadline_stores_nothing`,
`api::rest::quota::discovery_tests::the_discovery_route_declares_its_deadline_and_the_staging_as_its_timeout`,
`api::rest::quota::discovery_tests::an_oversized_record_stores_nothing_and_answers_503`,
`api::rest::quota::discovery_tests::a_record_holding_a_nul_stores_nothing_and_answers_503`,
`api::rest::quota::discovery_tests::a_discovery_refusal_names_the_adapter_target`,
`api::rest::quota::candidates_tests::a_manual_record_passes_the_discovery_judgement`,
`api::rest::quota::candidates_tests::a_manual_record_with_an_unknown_unit_is_refused`,
`api::rest::quota::candidates_tests::a_manual_record_with_the_same_id_and_other_content_conflicts`,
`api::rest::quota::candidates_tests::a_keyed_manual_record_replays_its_first_answer`,
`api::rest::quota::candidates_tests::the_candidate_list_filters_by_state_and_target`,
`api::rest::quota::candidates_tests::a_candidate_list_cursor_with_another_state_is_refused`,
`api::rest::quota::candidates_tests::a_candidate_list_cursor_from_another_tenant_is_refused`,
`api::rest::quota::candidates_tests::another_tenants_candidate_is_not_found`,
`api::rest::quota::candidates_tests::a_stored_candidate_that_does_not_decode_is_500`,
`api::rest::quota::candidates_tests::a_candidate_refusal_names_the_quota_definition`,
`api::rest::quota::candidates_tests::a_candidate_page_reads_its_definition_digests_in_one_statement`,
`api::rest::quota::candidates_tests::a_nul_in_a_quota_body_is_validation_on_its_path`,
`config_tests::the_quota_settings_default_and_refuse_out_of_range`,
`postgres_quota_catalog::two_concurrent_intakes_of_one_revision_store_one_snapshot_on_postgres`,
`postgres_quota_catalog::a_manual_record_that_loses_the_insert_race_answers_the_winner_on_postgres` and
`postgres_quota_catalog::a_nul_in_a_review_body_is_validation_on_postgres`.
Implementation markers in `products/src/domain/quota/record.rs`, `products/src/domain/quota/snapshot.rs`,
`products/src/infra/quota/registry.rs`, `products/src/infra/quota/discovery.rs`,
`products/src/api/rest/quota/intake.rs`, `products/src/api/rest/quota/targets.rs`,
`products/src/api/rest/quota/discovery.rs` and `products/src/api/rest/quota/candidates.rs`.

An adapter target binds a plugin instance types-registry holds under the quota discovery specification. Discovery
resolves the target's scoped plugin client, or answers 501 QUOTA_DISCOVERY_CLOSED before any effect. The target's
stored revision answers first: a replay, a conflict or a stale revision judges no record. Otherwise discovery reads
one complete snapshot outside any transaction, within the page timeout, the record bound, 64 KiB a record, no U+0000
in a record, and one deadline over the read and the judging (25 s by default; the operation's request timeout adds
15 s for the staging, so the 503 comes before the API gateway cuts the request); an incomplete one is 503 and stores
nothing. Each record is judged through `TypesRegistryApi` as the caller, with no minor version in any segment of its
`type_id` or its metric, unit and policy references: a refused record is a `rejected` note, a capability key two
records name rejects both, and a registry outage aborts with 503 and stores nothing. A body that holds U+0000 is 400
VALIDATION on its path. One transaction stores the snapshot, one candidate per valid record and the notes with the
audit row, or answers the stored revision, or refuses a conflict or a stale revision. A manual record passes the same
judgement and keeps its author, and a keyed retry replays its first answer. Every endpoint authorizes before it reads
the body, asks for the caller's tenant only, binds its list cursor to that tenant, and names the adapter target or
the quota definition in every refusal (DESIGN §3.1, §3.3, §3.5; P-D-268, P-D-273).

### Review with separation of duties

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-quota-catalog-review`

Verified by `domain::quota::review_tests::adapter_bounds_convert_exactly_into_each_plan_unit`,
`domain::quota::review_tests::a_gib_step_is_inexact_in_gb`,
`domain::quota::review_tests::a_capability_missing_an_operation_is_profile_unsupported`,
`domain::quota::review_tests::initial_installation_is_required_and_increase_is_recorded`,
`domain::quota::review_tests::bases_and_units_are_non_empty_sets`,
`domain::quota::review_tests::a_stored_proposal_reads_back_as_written`,
`domain::quota::definition_tests::the_definition_digest_covers_the_pinned_dependencies`,
`domain::quota::definition_tests::an_equal_record_reuses_the_lineages_mapping`,
`domain::quota::definition_tests::a_changed_mapping_key_is_the_next_mapping_version`,
`domain::quota::definition_tests::a_named_definition_takes_its_next_version`,
`domain::quota::definition_tests::the_availability_is_disabled_pending_qualification`,
`domain::quota::definition_tests::a_stored_candidate_without_its_proposal_does_not_build`,
`api::rest::quota::review_tests::submit_moves_a_staged_candidate_with_its_proposal`,
`api::rest::quota::review_tests::submit_needs_english_profile_conditions`,
`api::rest::quota::review_tests::submit_with_an_unknown_sku_version_is_refused`,
`api::rest::quota::review_tests::a_gib_record_cannot_be_offered_in_gb`,
`api::rest::quota::review_tests::each_plan_unit_refusal_names_its_index_and_leaves_the_candidate_staged`,
`api::rest::quota::review_tests::submit_with_a_stale_if_match_is_stale_revision`,
`api::rest::quota::review_tests::submit_without_if_match_is_validation`,
`api::rest::quota::review_tests::a_keyed_submit_replays_its_first_answer`,
`api::rest::quota::review_tests::a_registry_revision_since_intake_is_a_source_conflict`,
`api::rest::quota::review_tests::an_approval_publishes_one_definition_version_unavailable_for_sale`,
`api::rest::quota::review_tests::the_submitter_cannot_approve_and_the_author_of_a_manual_record_cannot_either`,
`api::rest::quota::review_tests::a_reject_needs_a_note_and_closes_the_candidate`,
`api::rest::quota::review_tests::a_decision_needs_its_if_match_and_a_short_note`,
`api::rest::quota::review_tests::a_keyed_approval_replays_after_the_decision`,
`api::rest::quota::review_tests::a_keyed_reject_replays_its_first_answer`,
`api::rest::quota::review_tests::a_registry_revision_after_approval_leaves_the_definition_unchanged`,
`api::rest::quota::review_tests::a_second_version_of_a_definition_keeps_the_first_readable`,
`api::rest::quota::review_tests::a_named_definition_of_another_metric_is_refused`,
`api::rest::quota::review_tests::a_named_definition_of_another_sku_is_refused_at_submit`,
`api::rest::quota::review_tests::a_named_definition_of_another_sku_is_refused_at_approval`,
`api::rest::quota::review_tests::a_candidate_of_another_target_versions_a_definition_of_its_sku_and_metric`,
`api::rest::quota::review_tests::every_review_request_asks_its_grant_for_the_callers_tenant_only`,
`api::rest::quota::review_tests::a_grant_on_some_definitions_versions_only_those`,
`api::rest::quota::review_tests::a_lost_race_for_a_definitions_next_version_is_contended`,
`api::rest::quota::review_tests::the_review_operations_declare_their_answers`,
`postgres_quota_catalog::two_concurrent_approvals_publish_one_version_on_postgres`,
`postgres_quota_catalog::two_first_approvals_of_one_lineage_store_one_mapping_on_postgres` and
`postgres_quota_catalog::a_decided_candidate_is_frozen_on_postgres`. Implementation markers in
`products/src/domain/quota/review.rs`, `products/src/domain/quota/definition.rs` and
`products/src/api/rest/quota/review.rs`.

Submit judges the proposal against the stored record: a SKU version the tenant holds, a named definition the whole
`submit` grant reaches (else 404) of the same SKU and metric, plan units into which the bounds convert exactly, the
bases (`fixed` and `per_plan_unit` on REST), English conditions and the qualified profile. It pins the registry entities the definition carries at the content
intake judged: a revised entity is 409 QUOTA_SOURCE_CONFLICT, and a candidate staged before the revision waits for a
new snapshot revision or a manual record under a new record id. Approve and reject refuse the submitter and a manual
record's author with 403 SOD_VIOLATION, and a reject needs a note. Every review write is conditional on the
candidate's `If-Match`, with 409 QUOTA_CANDIDATE_STATE or STALE_REVISION on a miss, and a keyed retry replays its
first answer, after the decision too. Submit and reject answer the candidate's new `ETag`; an approval answers none.
An approval reads the named definition again in its transaction under the whole `approve` grant and checks its SKU
and metric, builds the definition, mapping and profile versions from stored rows alone, each with its digest, and writes availability revision 1 `disabled` with
reason `qualification_pending`; two concurrent approvals publish one version, a lineage holds one mapping row per
version, and the loser of a version race, a lineage's first included, is 409 CONTENDED without a retry (DESIGN §3.1, §3.3; P-D-269, P-D-270, P-D-271).

### Catalog reads and the SDK client

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-quota-catalog-reads`

Verified by `api::rest::quota::definitions_tests::the_list_answers_the_versions_applicable_to_the_sku_version`,
`api::rest::quota::definitions_tests::the_default_list_is_enabled_only_and_all_carries_reasons`,
`api::rest::quota::definitions_tests::a_cursor_with_changed_filters_is_refused`,
`api::rest::quota::definitions_tests::a_cursor_from_another_tenant_is_refused`,
`api::rest::quota::definitions_tests::the_list_makes_no_registry_call`,
`api::rest::quota::definitions_tests::the_definition_answers_the_requested_locale_with_an_english_fallback`,
`api::rest::quota::definitions_tests::the_filter_takes_only_eq_on_its_five_fields`,
`api::rest::quota::definitions_tests::an_exact_version_reads_as_stored_after_a_newer_version`,
`api::rest::quota::definitions_tests::another_tenants_definition_is_not_found`,
`api::rest::quota::definitions_tests::a_pdp_outage_is_503_not_an_empty_page`,
`api::rest::quota::definitions_tests::a_staged_or_submitted_candidate_is_not_listed`,
`api::rest::quota::definitions_tests::the_definition_carries_bounds_and_no_amount`,
`api::rest::quota::definitions_tests::a_stored_definition_that_does_not_decode_is_500`,
`api::rest::quota::definitions_tests::a_grant_on_some_definitions_reads_only_those`,
`api::rest::quota::definitions_tests::the_odata_budget_answers_before_the_terms_are_read`,
`api::rest::quota::definitions_tests::an_unparsable_filter_is_the_bindings_invalid_filter`,
`api::rest::quota::definitions_tests::every_read_asks_read_for_the_callers_tenant_only`,
`api::rest::quota::dto_tests::the_package_example_row_is_the_rest_definition`,
`infra::quota::catalog_client_tests::the_sdk_client_answers_what_rest_answers`,
`infra::quota::catalog_client_tests::the_sdk_client_refuses_another_tenants_version`,
`gear_tests::the_hubs_quota_catalog_is_products_client_after_the_gears_init`,
`gear_tests::the_production_wiring_closes_discovery_before_any_effect`,
`gear_tests::the_production_wiring_answers_503_without_a_registry`,
`gear_tests::served_query_parameters_are_typed_and_the_list_publishes_its_odata_vocabulary` and
`config_tests::the_quota_settings_default_and_refuse_out_of_range`. Implementation markers in
`products/src/api/rest/quota/definitions.rs` and `products/src/infra/quota/catalog_client.rs`.

The list answers the versions of a SKU approved at a SKU version up to the one asked, enabled ones by default and all
with their reasons under `availability=all`, in `Page<T>`, filtered only by its `eq` terms, with a cursor bound to
the caller's tenant and the filters. `$filter` is required, and the operation publishes its vocabulary. The reads
keep the whole `read` grant, so a grant on some definitions lists and reads only those. The version read answers one
stored version with its current availability. Text resolves the asked locale with an English fallback. Neither read
asks types-registry or a plugin, and a stored row that does not decode is a 500. `ProductsQuotaCatalogClientV1` is
registered in the ClientHub at init and answers what the endpoints answer, as its caller; its test compares the two
through the REST mirror of the SDK types. The gear-built router closes discovery without an adapter and answers 503
without a registry (DESIGN §3.3, §3.5; P-D-272, P-D-273, P-D-266).

## 6. Acceptance Criteria

Each criterion below corresponds to exactly one DoD above and cites [PRD §9](../PRD.md#9-acceptance-criteria).

| DoD | PRD trace | Given / When / Then |
| --- | --- | --- |
| `cpt-cf-bss-products-dod-quota-catalog-contract` | AC #34, #36; `cpt-cf-bss-products-fr-quota-catalog` | Given the SDK's quota schemas and the package's example records, when they are generated and parsed, then the unit and policy schemas are final, every reference is narrowed, every id is major-only `v1`, an absent operation flag or an unknown member is refused, GB and GiB reach bytes exactly while an inexact or overflowing conversion is refused, and the example snapshot's digest is the pinned one. |
| `cpt-cf-bss-products-dod-quota-catalog-store` | AC #29, #33; `cpt-cf-bss-products-fr-quota-catalog` | Given stored quota rows on either engine, when anything updates or deletes an evidence row, changes a candidate's content, stores a second snapshot at a revision or names another tenant's parent, then the storage refuses; a candidate's review columns move once per seen version. |
| `cpt-cf-bss-products-dod-quota-catalog-intake` | AC #33, #34; `cpt-cf-bss-products-fr-quota-catalog` | Given a target whose plugin answers a complete snapshot with valid and invalid records, when discovery runs, runs again, and runs with other content at the revision, then one candidate per valid record is staged with the others as rejected notes, the replay answers the stored candidates, and the conflict is 409; mixed pages and a registry outage store nothing; without an adapter discovery is 501 before any effect. |
| `cpt-cf-bss-products-dod-quota-catalog-review` | AC #37; `cpt-cf-bss-products-fr-quota-catalog` | Given a submitted candidate, when its submitter or a manual record's author approves it, or a reject has no note, then it is refused 403 SOD_VIOLATION or 400 NOTE_REQUIRED; when an independent approver approves it, then one immutable definition version is published with availability `disabled`, reason `qualification_pending`, without a registry call. |
| `cpt-cf-bss-products-dod-quota-catalog-reads` | AC #35; `cpt-cf-bss-products-fr-quota-catalog` | Given definitions approved at SKU versions 2 and 4, when a plan author or Pricing lists SKU version 1, 2 or 4 and reads an exact version, then each answers the versions approved up to it, disabled ones only under `availability=all` with their reason, a cursor under other filters or from another tenant is 400, the older version reads as stored, and no read asks the registry. |
