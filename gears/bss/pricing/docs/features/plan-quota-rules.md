<!-- CONFLUENCE_TITLE: [BSS]: Pricing — Plan Quota Rules (Feature) -->
<!-- Related: ../DECOMPOSITION.md, ../DESIGN.md, ../PRD.md, ../DECISIONS.md | Owners: BSS Pricing team -->

# Feature: Plan Quota Rules

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-featstatus-plan-quota-rules-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-bss-pricing-feature-plan-quota-rules`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Author, check and publish quota rules](#author-check-and-publish-quota-rules)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [replace](#replace)
  - [govern](#govern)
  - [accept](#accept)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [The rule shape, its digest and its storage](#the-rule-shape-its-digest-and-its-storage)
  - [Reading and replacing a revision's rules](#reading-and-replacing-a-revisions-rules)
  - [Checks, batch checks and the item guard](#checks-batch-checks-and-the-item-guard)
  - [Approval content, rechecks, copy and clone](#approval-content-rechecks-copy-and-clone)
  - [Quota terms in the preview and the receipt](#quota-terms-in-the-preview-and-the-receipt)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

**Delivery:** the quotas programme. Every checkbox is an implementation obligation, not an assertion about the code.

This feature is Pricing's implementation of the quotas package's feature 2, plan quota authoring and acceptance
([`cpt-cf-bss-quotas-feature-plan-rules`](../../../docs/quotas/features/0002-plan-rules.md)), on the contract of the
package's [DESIGN §3.3.1.1–§3.3.1.3](../../../docs/quotas/DESIGN.md#331-catalog-onboarding-and-plan-publication)
(D-540). It implements [slice 08](../design/08-plan-quota-rules.md).
[DECOMPOSITION](../DECOMPOSITION.md) entry 2.8 places it; [DESIGN §3](../DESIGN.md#3-technical-architecture) is the
schema and transaction authority. Products' part, the validation of a complete quota set, is Products' feature
[Quota Set Validation](../../../products/docs/features/quota-set-validation.md) (products P-D-275, P-D-276).

### 1.2 Purpose

Let a plan author attach allocation quota rules to a draft plan revision: each rule an exact Products definition
version, an amount in an allowed unit, a basis, a logical binding and a scope selection (D-541). The author replaces
the complete set at once, under the revision's ETag and an idempotency key (D-543), through the REST endpoints or the
SDK's `PricingQuotaAuthoringClientV1`, which answer the same request identically (D-540). Products validates the set
outside the transaction (D-542); the checks show every failing rule and field (D-544); submit, approval, copy and
clone carry the rules and recheck them with fresh reads (D-545); migration 000024 stores them (D-546). The preview
shows the selected quota terms with a digest (D-547), and an acceptance freezes the selected rules, their evidence,
the original quantity and the digests in the receipt that Subscriptions reads (D-548). No quota is provisioned, and
nothing is sellable before qualification: every Products definition stays `disabled` until the package's feature 5
(products P-D-271).

Requirements: `cpt-cf-bss-pricing-fr-plan-quota-rules`. It serves the package's `cpt-cf-bss-quotas-fr-plan-rules`,
`cpt-cf-bss-quotas-fr-plan-amount`, `cpt-cf-bss-quotas-fr-plan-absence`, `cpt-cf-bss-quotas-fr-plan-revision`,
`cpt-cf-bss-quotas-fr-charging-separation`, `cpt-cf-bss-quotas-fr-plan-edit`,
`cpt-cf-bss-quotas-fr-plan-edit-conflict`, `cpt-cf-bss-quotas-fr-plan-governance` and
`cpt-cf-bss-quotas-fr-qualification-recheck`.

Architecture: `cpt-cf-bss-pricing-component-plans`, `cpt-cf-bss-pricing-component-approvals`,
`cpt-cf-bss-pricing-component-read-contract`, `cpt-cf-bss-pricing-constraint-two-backends`,
`cpt-cf-bss-pricing-constraint-no-row-locks`, `cpt-cf-bss-pricing-constraint-one-replay-store`,
`cpt-cf-bss-pricing-seq-blocked-revision`.

### 1.3 Actors

`cpt-cf-bss-pricing-actor-product-manager` reads and replaces a draft's rules and reads its checks, through REST or
`PricingQuotaAuthoringClientV1`, under `read` and `author` on `plan`; `cpt-cf-bss-pricing-actor-products` validates each
complete set through `ProductsQuotaCatalogClientV1::validate_quota_set`, as the caller, under `read` on
`quota_definition`: the authors who save rules, the readers of the checks (single and batch), the submitter, the final
approver (apply re-runs the checks) and the caller of an acceptance (Orders' principal) need that grant, and a non-final
vote keeps a refusal as an observation and needs none (D-542, D-503); `cpt-cf-bss-pricing-actor-subscriptions` reads the
frozen quota evidence of an acceptance through `PricingAcceptanceV1::acceptance` (D-548). Every operation authenticates
and derives tenant scope before storage, replay or calls to another gear.

### 1.4 References

- [PRD](../PRD.md): `fr-plan-quota-rules`, AC #26, AC #27, AC #28 and AC #29.
- [DESIGN](../DESIGN.md): §3.1 (the rules on a revision), §3.2 (Plans, Read contract), §3.3 (the Quota rules row),
  §3.5 (the quota catalog), §3.6 (the capture before the submit and vote transactions), §3.7 (migration 000024), and
  the seam fixture boundary (the optional quota members).
- [Slice 08](../design/08-plan-quota-rules.md), including its API, data and event obligations.
- [DECISIONS](../DECISIONS.md): D-540, D-541, D-542, D-543, D-544, D-545, D-546, D-547, D-548; D-404 (the draft's
  author), D-466 (check subjects), D-482 (the checks context), D-501 (canonical digests), D-503 (captures outside the
  transaction), D-505 and D-510 (the receipt).
- The quotas package: [feature 2](../../../docs/quotas/features/0002-plan-rules.md) (flow
  `cpt-cf-bss-quotas-flow-plan-rules`, algorithm `cpt-cf-bss-quotas-algo-plan-rules`, DoD
  `cpt-cf-bss-quotas-dod-plan-rules`, criteria Q-C02, Q-C58, Q-C59, Q-C60, Q-C61 and Q-C65) and
  [DESIGN](../../../docs/quotas/DESIGN.md) §3.3.1.1–§3.3.1.3.

## 2. Actor Flows (CDSL)

### Author, check and publish quota rules

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-flow-plan-quota-rules`

Implements the package's `cpt-cf-bss-quotas-flow-plan-rules`.

1. [x] - `p1` - Product Manager reads a draft revision with its items and its quota rules, with the revision's ETag; ask `read` on `plan` - `inst-quota-read`
2. [x] - `p1` - Product Manager selects exact definitions from Products' catalog and puts the complete rule set under the revision's ETag and an idempotency key, by algorithm replace - `inst-quota-replace`
3. [x] - `p1` - Product Manager reads the revision's checks: one red row per failing quota code, each naming the item, the rule and the field, with the evaluated revision version and digest (D-544) - `inst-quota-check`
4. [x] - `p1` - Submit and every vote recheck the rules by algorithm govern; a red quota row refuses, and an approved revision publishes its rules unchanged - `inst-quota-submit`
5. [x] - `p1` - Copying a published revision or cloning its plan copies its rules onto the copied items by algorithm govern - `inst-quota-copy`
6. [ ] - `p1` - A buyer's preview carries the revision's quota terms; the selected quota digest is computed for the requested selection by algorithm accept - `inst-quota-preview`
7. [ ] - `p1` - Acceptance checks the expected quota digest and freezes the selected rules in the receipt by algorithm accept - `inst-quota-accept`

## 3. Processes / Business Logic (CDSL)

### replace

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-algo-plan-quota-rules-replace`

Implements the package's `cpt-cf-bss-quotas-algo-plan-rules`.

1. [x] - `p1` - Authenticate and ask `author` on `plan` for the caller's tenant before any header or body is read; when no stored answer replays (step 2), the revision must be the caller's own unlocked draft (`NOT_DRAFT_AUTHOR`, `REVISION_NOT_DRAFT`) - `inst-quota-authorize`
2. [x] - `p1` - Look up the key's stored answer with the request digest of the `If-Match` value and the rules as they parse (the body itself when it does not parse); the same content answers its stored body and ETag even when the ETag moved since, other content is 409 IDEMPOTENCY_CONFLICT - `inst-quota-replay`
3. [x] - `p1` - Parse the complete set: no unknown member or null collection, at most 200 rules, unique rule ids, canonical integer amounts, the binding-key and scope-key rule, the text bounds, the basis and subject tokens; each failure names `quota_rules[i].<field>` - `inst-quota-parse`
4. [x] - `p1` - Outside any transaction, read each rule item's SKU version on the sale date and validate the set through Products' `validate_quota_set`; add the plan's own checks (item, dimension value, SKU version, duplicates); refuse every known failure but availability at once, changing nothing - `inst-quota-validate`
5. [x] - `p1` - In the transaction, claim the key, then require the unlocked draft at the `If-Match` version (else 409 STALE_REVISION) and the items and sale date the validation saw, each rule's item among them (else capture again) - `inst-quota-fence`
6. [x] - `p1` - Write the rules with their evidence and availability observation, the set row with its digest, the next revision version, the audit row and the key's answer together; answer 200 with the saved set and the new ETag - `inst-quota-commit`

### govern

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-algo-plan-quota-rules-govern`

1. [x] - `p1` - The revision's approval content carries its rules and their digest; a different set is a different content (D-545) - `inst-quota-fingerprint`
2. [x] - `p1` - Submit and every vote capture the quota validation before their transaction and re-check the captured selection inside it; a red quota row refuses submit and apply; a scheduled switch makes no catalog call - `inst-quota-recheck`
3. [x] - `p1` - Deleting an item that a rule names is 409 ITEM_HAS_QUOTA_RULES; deleting a draft removes its rules first - `inst-quota-item-delete`
4. [x] - `p1` - A copy or a clone writes each rule with its copied item, its rule id, definition reference, evidence and observation, and the digest computed again - `inst-quota-remap`

### accept

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-algo-plan-quota-rules-accept`

1. [ ] - `p1` - Resolve answers every rule of the revision with its pinned evidence and the set digest, or none for a revision without rules - `inst-quota-terms`
2. [ ] - `p1` - Select the rules whose item and value the sale requests; an item's rule never applies to another value or through the default chain - `inst-quota-select`
3. [ ] - `p1` - A revision with rules needs the sale's expected quota digest, and it must equal the selected digest; else RESOLUTION_CHANGED - `inst-quota-expected`
4. [ ] - `p1` - Validate the selected set through Products for acceptance at the accepted quantity, then freeze the rules, their evidence, the original quantity and the selected and snapshot digests in the receipt, in the acceptance transaction - `inst-quota-freeze`

## 4. States (CDSL)

No new state. A rule set follows its revision through the plan revision states of
[features/plans.md](plans.md): it is edited only in an unlocked draft and is locked, scheduled, published and
superseded with its revision.

## 5. Definitions of Done

These definitions own this feature's 5 DoDs. Each implements Pricing's half of the package's
`cpt-cf-bss-quotas-dod-plan-rules`. Design constraints: `cpt-cf-bss-pricing-constraint-two-backends`,
`cpt-cf-bss-pricing-constraint-no-row-locks`.

### The rule shape, its digest and its storage

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-plan-quota-rules-store`

Verified by `bss_pricing_sdk::plan_quota::plan_quota_tests::the_binding_key_rule_takes_one_to_64_of_its_characters`,
`bss_pricing_sdk::plan_quota::plan_quota_tests::basis_and_subject_tokens_round_trip`,
`bss_pricing_sdk::plan_quota::plan_quota_tests::the_rule_set_digest_ignores_input_order`,
`bss_pricing_sdk::plan_quota::plan_quota_tests::every_rule_member_moves_the_digest`,
`bss_pricing_sdk::plan_quota::plan_quota_tests::the_empty_set_has_its_own_digest`,
`bss_pricing_sdk::plan_quota::plan_quota_tests::rules_that_share_a_rule_id_digest_alike_in_either_order`,
`digest_vectors::quota_rules_vector_matches_the_sdk_function`,
`digest_vectors::quota_rules_vectors_pin_the_empty_set_and_the_rule_order`,
`infra::storage::migrations::m20261121_000024_plan_quota_rules::tests::the_rule_checks_hold`,
`infra::storage::migrations::m20261121_000024_plan_quota_rules::tests::the_set_checks_hold`,
`infra::storage::migrations::m20261121_000024_plan_quota_rules::tests::a_rule_names_an_item_and_a_revision_that_exist`,
`infra::storage::migrations::m20261121_000024_plan_quota_rules::tests::an_item_with_rules_cannot_be_deleted`,
`infra::storage::migrations::m20261121_000024_plan_quota_rules::tests::down_refuses_while_a_rule_exists`,
`infra::storage::repo::plan_quota_repo::plan_quota_repo_tests::replace_writes_the_set_and_bumps_the_revision_version`,
`infra::storage::repo::plan_quota_repo::plan_quota_repo_tests::replace_at_a_stale_version_writes_nothing`,
`infra::storage::repo::plan_quota_repo::plan_quota_repo_tests::replace_on_a_locked_revision_writes_nothing`,
`infra::storage::repo::plan_quota_repo::plan_quota_repo_tests::replace_on_a_published_revision_is_stale_revision`,
`infra::storage::repo::plan_quota_repo::plan_quota_repo_tests::replace_refuses_a_rule_of_another_revision_and_writes_nothing`,
`infra::storage::repo::plan_quota_repo::plan_quota_repo_tests::clearing_a_set_deletes_its_set_row`,
`infra::storage::repo::plan_quota_repo::plan_quota_repo_tests::a_revision_without_a_set_reads_the_empty_set`,
`infra::storage::repo::plan_quota_repo::plan_quota_repo_tests::a_rule_without_a_set_row_is_a_corrupt_row`,
`infra::storage::repo::plan_quota_repo::plan_quota_repo_tests::a_set_count_that_disagrees_is_a_corrupt_row`,
`infra::storage::repo::plan_quota_repo::plan_quota_repo_tests::a_set_row_without_rules_is_a_corrupt_row`,
`infra::storage::repo::plan_quota_repo::plan_quota_repo_tests::the_sets_of_many_revisions_read_in_two_statements`,
`infra::storage::repo::plan_quota_repo::plan_quota_repo_tests::copy_moves_each_rule_onto_its_copied_item`,
`infra::storage::repo::plan_quota_repo::plan_quota_repo_tests::copy_refuses_a_rule_whose_item_was_not_copied`,
`infra::storage::repo::plan_quota_repo::plan_quota_repo_tests::a_quota_free_copy_writes_no_set_row`,
`infra::storage::repo::plan_quota_repo::plan_quota_repo_tests::delete_for_draft_removes_the_rules_and_the_set_row`,
`infra::storage::repo::plan_quota_repo::plan_quota_repo_tests::delete_for_draft_leaves_a_locked_revisions_rules`,
`infra::storage::repo::plan_quota_repo::plan_quota_repo_tests::item_has_rules_answers_for_one_item`,
`postgres_plan_quota_rules::the_rule_checks_hold_on_postgres`,
`postgres_plan_quota_rules::down_refuses_while_a_rule_exists_on_postgres`,
`postgres_plan_quota_rules::the_rule_repository_round_trips_on_postgres`,
`postgres_plan_quota_rules::a_damaged_set_is_a_corrupt_row_on_postgres`,
`postgres_plan_quota_rules::a_set_read_in_one_transaction_keeps_its_snapshot_on_postgres`,
`postgres_plan_quota_rules::two_replaces_on_one_version_store_one_set_on_postgres`,
`postgres_plan_quota_rules::an_item_delete_racing_a_replace_leaves_no_orphan_on_postgres` and
`migration_chain::m20261121_000024_plan_quota_rules`. Implementation markers in `pricing-sdk/src/plan_quota.rs`,
`pricing-sdk/src/digest.rs`, `pricing/src/infra/storage/migrations/m20261121_000024_plan_quota_rules.rs` and
`pricing/src/infra/storage/repo/plan_quota_repo.rs`.

`bss_pricing_sdk::plan_quota` holds the rule's v1 shape, its caps and the binding key rule, with no GTS id and no serde;
`quota_rules_digest` hashes the rules ordered by `rule_id` in the domain `pricing.quota-rules.v1`, with pinned vectors
for one rule, for two rules given in reverse order and for the empty set, and every older vector is unchanged. Migration
`m20261121_000024_plan_quota_rules` creates `pricing_plan_quota_set` and `pricing_plan_quota_rule` on both engines with
the keys, foreign keys, CHECKs and index of DESIGN §3.7; `down` refuses while a rule exists. The repository reads one
revision's set, or many revisions' sets, in two statements inside the caller's serializable transaction, and a set row
and rules that disagree are a corrupt row. It replaces a draft's set at the version the caller read (a lost version, a
locked or a non-draft revision is `STALE_REVISION`, with nothing written; the version write refreshes the plan summary,
D-484), and a replace with no rules deletes the set row. It copies a set onto copied items, deletes a draft's set, and
answers whether an item has rules. Two replaces on one version store one set, and an item delete racing a replace leaves
no orphan, on PostgreSQL in both orders (D-541, D-543, D-546).

Requirement: `cpt-cf-bss-pricing-fr-plan-quota-rules`; PRD AC #26.

### Reading and replacing a revision's rules

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-plan-quota-rules-authoring`

Verified by `domain::plan_quota::plan_quota_tests::a_complete_body_parses_into_normalized_rules`,
`domain::plan_quota::plan_quota_tests::the_package_example_body_parses`,
`domain::plan_quota::plan_quota_tests::a_body_is_one_object_with_one_array`,
`domain::plan_quota::plan_quota_tests::unknown_members_and_null_collections_are_refused`,
`domain::plan_quota::plan_quota_tests::every_member_is_required_with_its_type`,
`domain::plan_quota::plan_quota_tests::ids_and_tokens_are_exact`,
`domain::plan_quota::plan_quota_tests::a_definition_ref_is_its_three_members_with_a_version_and_lowercase_hex`,
`domain::plan_quota::plan_quota_tests::more_than_200_rules_are_refused`,
`domain::plan_quota::plan_quota_tests::a_repeated_rule_id_is_refused`,
`domain::plan_quota::plan_quota_tests::amounts_are_canonical_integers`,
`domain::plan_quota::plan_quota_tests::a_binding_key_off_the_rule_is_rule_invalid`,
`domain::plan_quota::plan_quota_tests::scope_keys_follow_the_binding_key_rule_and_values_are_printable`,
`domain::plan_quota::plan_quota_tests::unit_and_dimension_texts_stay_within_the_catalogs_bound`,
`domain::plan_quota::plan_quota_tests::a_nul_in_any_member_is_rule_invalid`,
`domain::plan_quota::plan_quota_tests::every_rule_is_parsed_and_its_failures_keep_member_order`,
`domain::plan_quota::plan_quota_tests::a_stray_body_member_still_reports_every_rule_failure`,
`domain::plan_quota::plan_quota_tests::typed_rules_are_judged_with_the_parses_codes`,
`domain::plan_quota::plan_quota_tests::a_caller_text_is_echoed_escaped_and_short`,
`domain::plan_quota::plan_quota_tests::many_unknown_members_name_the_first_ten_and_count_the_rest`,
`domain::plan_quota::plan_quota_tests::a_rule_for_a_foreign_item_is_rule_invalid`,
`domain::plan_quota::plan_quota_tests::a_dimension_value_must_be_the_items_registered_value`,
`domain::plan_quota::plan_quota_tests::a_rule_whose_sku_has_no_version_on_the_sale_date_is_rule_invalid`,
`domain::plan_quota::plan_quota_tests::two_rules_for_one_binding_and_metric_that_apply_together_are_duplicates`,
`domain::plan_quota::plan_quota_tests::dimension_alternatives_of_one_item_are_not_duplicates`,
`domain::plan_quota::plan_quota_tests::catalog_checks_map_to_rule_paths_and_a_refusal_status`,
`domain::plan_quota::plan_quota_tests::products_fields_that_name_no_rule_member_map_to_the_member_they_are_about`,
`domain::plan_quota::plan_quota_tests::a_set_check_names_each_of_its_rules_and_one_naming_none_is_on_the_set`,
`domain::plan_quota::plan_quota_tests::only_a_changed_definition_or_a_duplicate_is_a_conflict`,
`infra::plan_quota::plan_quota_tests::an_absent_catalog_is_503_quota_catalog_unavailable`,
`infra::plan_quota::plan_quota_tests::a_catalog_outage_is_503_and_a_refusal_is_forwarded`,
`infra::plan_quota::plan_quota_tests::a_rate_limit_or_a_contention_conflict_is_an_outage`,
`infra::plan_quota::plan_quota_tests::a_catalog_call_that_does_not_finish_is_503`,
`infra::plan_quota::plan_quota_tests::the_catalog_answers_on_a_task_of_its_own_even_inside_a_transaction`,
`infra::plan_quota::plan_quota_tests::the_sku_versions_are_read_once_per_sku_and_a_missing_version_is_absent`,
`infra::plan_quota::plan_quota_tests::a_set_whose_skus_have_no_version_on_the_sale_date_asks_no_catalog`,
`infra::plan_quota::plan_quota_tests::the_set_request_carries_each_items_sku_version`,
`infra::plan_quota::plan_quota_tests::the_evidence_keeps_identity_and_no_text_or_availability`,
`infra::plan_quota::plan_quota_tests::a_stored_rule_round_trips_with_its_evidence_and_observation`,
`infra::plan_quota::plan_quota_tests::a_tampered_row_is_a_corrupt_row`,
`api::plan_quota_authoring::plan_quota_authoring_tests::a_typed_rule_is_sent_as_the_put_body_carries_it`,
`api::plan_quota_authoring::plan_quota_authoring_tests::an_answer_that_does_not_read_back_is_500`,
`api::rest::closed_sets::closed_sets_tests::quota_rule_sets_carry_the_sdk_and_products_tokens`,
`plan_quota_rules::get_answers_the_empty_set_with_the_revision_etag`,
`plan_quota_rules::the_revision_read_carries_the_rules_and_digest`,
`plan_quota_rules::another_tenants_revision_is_404_for_quota_rules`,
`plan_quota_rules::another_tenants_caller_cannot_replace_quota_rules`,
`plan_quota_rules::a_denied_caller_is_403_before_the_path_is_read`,
`plan_quota_rules::put_replaces_the_complete_set_and_answers_the_new_etag`,
`plan_quota_rules::an_empty_array_clears_every_rule`,
`plan_quota_rules::an_invalid_set_writes_nothing_and_names_every_failure`,
`plan_quota_rules::a_stale_if_match_is_409_and_writes_nothing`,
`plan_quota_rules::the_same_key_and_content_replays_after_the_etag_moved`,
`plan_quota_rules::the_same_key_with_other_content_is_idempotency_conflict`,
`plan_quota_rules::the_same_rules_spelled_otherwise_replay_under_the_key`,
`plan_quota_rules::a_definition_pending_qualification_is_saved_and_checks_red`,
`plan_quota_rules::a_duplicate_set_is_409_quota_rule_duplicate`,
`plan_quota_rules::a_changed_definition_digest_is_409`,
`plan_quota_rules::a_published_or_pending_revision_refuses_rules`,
`plan_quota_rules::another_author_is_not_draft_author`,
`plan_quota_rules::a_quota_catalog_outage_is_503_and_writes_nothing`,
`plan_quota_rules::a_rule_whose_sku_has_no_version_on_the_sale_date_is_refused_on_item_id`,
`plan_quota_rules::a_corrupt_stored_rule_answers_500`,
`plan_quota_rules::a_write_contended_past_its_retries_is_409_contended`,
`plan_quota_rules::a_rule_products_does_not_judge_is_a_logged_500_at_the_put`,
`plan_quota_rules::a_body_of_many_unknown_members_is_refused_with_a_bounded_answer`,
`plan_quota_rules::the_package_example_body_is_accepted`,
`plan_quota_rules::a_missing_if_match_or_key_is_400`,
`plan_quota_rules::a_moved_rule_set_rolls_back_and_is_captured_again`,
`plan_quota_rules::the_catalog_is_never_called_on_a_transactions_task`,
`plan_quota_authoring::the_sdk_get_answers_what_rest_answers`,
`plan_quota_authoring::the_sdk_replace_answers_what_rest_answers`,
`plan_quota_authoring::the_sdk_check_answers_what_rest_answers`,
`plan_quota_authoring::the_hub_resolves_the_quota_authoring_client`,
`postgres_plan_quota_rules::the_quota_rule_endpoints_replace_replay_and_fence_on_postgres` and
`served_contract::the_quota_texts_name_their_outage_and_refusals`. Implementation markers in
`pricing/src/domain/plan_quota.rs`, `pricing/src/infra/plan_quota.rs`, `pricing/src/api/rest/authoring/plan_quota.rs`
and `pricing/src/api/plan_quota_authoring.rs`.

`GET /plan-revisions/{id}/quota-rules` answers a revision's stored rules and digest with the revision's ETag, and the
revision read carries both; neither asks Products, and each reads the set row and the rules in one serializable read
transaction. `PUT /plan-revisions/{id}/quota-rules` follows D-543's order: authorization before input, the key's stored
answer before the version (a replay answers the stored body and ETag byte for byte and calls no other gear; the key is
bound to the `If-Match` version and the rules as they parse, so the same rules spelled otherwise replay), the parse
(every rule's failures, a stray body member notwithstanding, U+0000 on the member that holds it), then the SKU versions
on the sale date and Products' validation on a task of their own, outside any transaction, with Pricing's own checks of
items, dimension values, SKU versions and duplicates. Every known failure but availability is refused at once, each
named `quota_rules[i].<field>` with its code (a failure of the whole set on `quota_rules`), 409 when every failure is a
conflict code and 400 otherwise, one invalid-argument problem either way; a definition that is not enabled is saved
(D-542). One transaction then claims the key, checks the draft's version, its items and its sale date, and commits the
rules, their evidence and availability, the set row, the next version, the audit row and the answer; a transaction
contended past its retries is 409 `CONTENDED`. Every catalog outage is 503 `QUOTA_CATALOG_UNAVAILABLE` with nothing
written, a Products 400, 403 or 500 is forwarded, and the catalog is never called on a transaction's task.
`PricingQuotaAuthoringClientV1`, whose local client is registered in the ClientHub at init, answers `get_quota_rules`,
`replace_quota_rules` and `check_plan_revision` over the same functions as the GET, the PUT and
`GET /plan-revisions/{id}/checks`: the same body, the same refusals and the same ETag as the endpoints, and one key
space with the PUT (D-540, D-541, D-542, D-543).

Requirement: `cpt-cf-bss-pricing-fr-plan-quota-rules`; PRD AC #26.

### Checks, batch checks and the item guard

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-plan-quota-rules-checks`

Verified by `domain::plan_quota::plan_quota_tests::quota_rows_name_the_rule_and_field`,
`domain::plan_quota::plan_quota_tests::a_disabled_definition_turns_only_the_qualification_row_red`,
`domain::plan_quota::plan_quota_tests::the_quota_rows_keep_their_order`,
`domain::plan_quota::plan_quota_tests::the_quota_rows_follow_every_other_row_and_fail_closed_without_a_verdict`,
`domain::plan_quota::plan_quota_tests::the_rows_judge_items_versions_and_duplicates_from_the_context`,
`domain::plan_quota::plan_quota_tests::a_check_that_names_no_rule_is_a_line_of_its_row_without_a_subject`,
`domain::plan_quota::plan_quota_tests::a_check_naming_a_rule_that_was_not_sent_is_on_the_set`,
`domain::plan_quota::plan_quota_tests::a_rule_with_no_definition_and_no_check_is_rule_invalid`,
`domain::plan_quota::plan_quota_tests::a_rule_products_leaves_out_of_its_verdict_reads_red`,
`domain::plan_quota::plan_quota_tests::every_products_code_turns_its_own_row_red`,
`domain::plan_quota::plan_quota_tests::a_violation_without_a_row_of_its_own_turns_rule_invalid_red`,
`domain::plan_quota::plan_quota_tests::a_rule_is_judged_only_against_the_definition_it_names`,
`domain::plan_quota::plan_quota_tests::a_rule_resolved_to_another_version_lends_no_metric`,
`domain::plan::tests::a_revision_without_rules_has_no_quota_row`,
`infra::plan_quota::plan_quota_tests::the_checks_contexts_carry_each_revisions_own_set`,
`api::rest::authoring::plan_items::tests::a_foreign_key_refusal_of_an_item_delete_is_item_has_quota_rules`,
`plan_quota_rules::checks_name_the_rule_field_version_and_digest`,
`plan_quota_rules::a_quota_free_revisions_checks_keep_their_rows`,
`plan_quota_rules::a_catalog_outage_makes_the_checks_503_not_green`,
`plan_quota_rules::a_rule_products_does_not_judge_checks_red`,
`plan_quota_rules::batch_checks_match_the_single_read_with_rules`,
`plan_quota_rules::deleting_an_item_with_rules_is_409_item_has_quota_rules`,
`plan_quota_rules::deleting_a_draft_with_rules_removes_them`,
`postgres_plan_quota_rules::the_checks_read_with_rules_on_postgres` and
`plan_checks_batch::the_checks_read_is_thirteen_statements_whatever_the_set`. Implementation markers in
`pricing/src/domain/plan.rs`, `pricing/src/domain/plan_quota.rs`, `pricing/src/api/rest/authoring/plans.rs` and
`pricing/src/api/rest/authoring/plan_items.rs`.

The checks of a revision with rules validate them through Products on every read, outside the read transaction (one
SKU-version read per distinct SKU and sale date, one validation per revision with rules), and add the eight quota rows
of D-544 after the existing rows, each red row's subjects naming the item, its SKU and entry, the rule and the field
(`""` for the rule as a whole); a failure of the whole set is a line of its row's detail. A revision without rules has
no quota row and makes no catalog call. Every checks answer carries `revision_version` and `quota_rules_digest`, and the
batch answers each revision as the single read does; the checks context reads the rules in two more statements, thirteen
in all, in one serializable read transaction. A catalog outage is 503 `QUOTA_CATALOG_UNAVAILABLE`, never `ready=true`; a
rule Products was sent but did not judge reads red, and a revision whose verdict was not read fails closed. Deleting an
item that a rule names is 409 `ITEM_HAS_QUOTA_RULES`, and deleting a draft removes its rules and set row before its
items (D-543, D-544, D-545).

Requirement: `cpt-cf-bss-pricing-fr-plan-quota-rules`; PRD AC #27.

### Approval content, rechecks, copy and clone

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-plan-quota-rules-governance`

Verified by `plan_quota_governance::submit_refuses_a_definition_pending_qualification`,
`plan_quota_governance::quorum_zero_publishes_a_revision_with_rules_unchanged`,
`plan_quota_governance::a_final_approve_rechecks_and_publishes_a_revision_with_rules`,
`plan_quota_governance::apply_rechecks_and_refuses_a_definition_withdrawn_after_submit`,
`plan_quota_governance::a_catalog_outage_at_submit_is_503_and_records_no_unit`,
`plan_quota_governance::a_catalog_outage_refuses_only_the_final_approve`,
`plan_quota_governance::a_catalog_refusal_at_submit_is_answered_as_products_gave_it`,
`plan_quota_governance::a_catalog_refusal_at_the_final_approve_is_answered_as_products_gave_it`,
`plan_quota_governance::a_set_replaced_during_submits_capture_is_captured_again`,
`plan_quota_governance::a_rule_products_leaves_out_refuses_submit`,
`plan_quota_governance::a_rule_products_leaves_out_refuses_the_final_approve`,
`plan_quota_governance::a_rule_judged_against_another_version_refuses_submit_and_the_final_approve`,
`plan_quota_governance::the_unit_content_carries_the_rules`,
`plan_quota_governance::a_quota_free_units_content_has_no_quota_member`,
`plan_quota_governance::a_submitted_revision_refuses_a_replace`,
`plan_quota_governance::a_price_change_leaves_the_rules_and_their_digest`,
`plan_quota_governance::the_scheduled_switch_publishes_without_a_catalog_call`,
`plan_quota_governance::a_copy_keeps_exact_definitions_and_remaps_items`,
`plan_quota_governance::a_copys_digest_is_computed_again_over_its_items`,
`plan_quota_governance::a_published_revision_reads_its_rules_after_withdrawal`,
`plan_quota_governance::a_clone_copies_rules_into_the_new_plan`,
`plan_quota_governance::the_copy_of_a_withdrawn_definition_checks_red`,
`plan_quota_governance::each_rule_is_judged_against_the_sku_version_on_its_sale_date`,
`plan_quota_governance::products_is_asked_as_the_caller_at_every_step`,
`infra::plan_quota::plan_quota_tests::a_capture_calls_products_outside_the_transaction_and_its_check_sees_a_moved_set`,
`infra::plan_quota::plan_quota_tests::an_empty_capture_sees_a_set_added_before_the_transaction`,
`infra::plan_quota::plan_quota_tests::a_dependency_failure_is_kept_in_the_capture`,
`infra::plan_quota::plan_quota_tests::a_capture_reads_the_sku_version_on_the_sale_date_not_today`,
`infra::plan_revisions::plan_revisions_tests::a_revision_with_rules_and_no_quota_capture_is_the_kept_unit_contended`,
Products' in-crate e2e
`api::rest::quota::plan_rules_e2e_tests::a_plan_carries_10_vcpu_and_20_gb_per_unit_through_the_real_catalog`,
`api::rest::quota::plan_rules_e2e_tests::the_real_catalog_refuses_an_amount_off_its_grid_and_reports_a_pending_definition` and
`api::rest::quota::plan_rules_e2e_tests::vcpu_and_ram_of_two_adapter_targets_cannot_share_a_binding`. Implementation
markers in `pricing/src/infra/plan_revisions.rs`, `pricing/src/api/rest/authoring/approvals.rs` and
`pricing/src/api/rest/authoring/plans.rs`.

A revision's approval content carries its rules and their digest (`quota_rules`, `quota_rules_digest`) when it holds
rules, so a different set is a different content; a quota-free unit's content and fingerprint do not move. Submit and
every vote capture the quota validation before their transaction, at the instant the subject judges, and check inside it
that the captured set, items and sale date have not moved; a missing capture is the kept 409 `UNIT_CONTENDED`. A red
quota row refuses submit and apply; a catalog outage is 503 `QUOTA_CATALOG_UNAVAILABLE` at submit and at the final
approve, a Products refusal keeps Products' own status and code, and a non-final vote still records. A scheduled
revision's switch makes no catalog call. A copy and a clone write each rule onto its copied item with its rule id,
definition reference, evidence and observation, and compute the digest again; eligibility is rechecked, never copied. A
published revision reads its rules after its definition is withdrawn. Against Products' real catalog, a plan carries 10
vCPU and 20 GB per plan unit through checks and publication, an amount off the grid is refused, a pending definition is
saved and checks red, and two definitions on two adapter targets cannot share a binding (D-542, D-545).

Requirement: `cpt-cf-bss-pricing-fr-plan-quota-rules`; PRD AC #27, AC #28.

### Quota terms in the preview and the receipt

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-dod-plan-quota-rules-acceptance`

Verified by `digest_tests::selected_quota_digest_follows_the_requested_value_not_the_default_chain`,
`digest_tests::quota_free_terms_and_request_digests_are_unchanged`,
`digest_tests::the_snapshot_digest_binds_quantity_and_tenant_axes`,
`digest_tests::an_expected_digest_moves_the_request_digest_and_a_snapshot_moves_the_terms_digest`,
`wire::v1::tests::a_receipt_without_quota_reads_and_writes_as_before`,
`wire::v1::tests::a_receipt_with_a_snapshot_round_trips`,
`wire::v1::tests::a_snapshot_with_a_bad_digest_is_a_corrupt_row`,
`pricing_read_sdk::resolve_carries_the_quota_terms_of_every_rule`,
`pricing_read_sdk::a_quota_free_revision_resolves_without_terms`,
`pricing_read_sdk::resolve_reads_the_quota_terms_without_a_catalog_call`,
`read_contract::get_resolve_answers_the_quota_terms`,
`sellability::a_quota_bearing_plan_needs_the_expected_quota_digest`,
`sellability::a_stale_expected_quota_digest_is_resolution_changed_with_prices_unchanged`,
`sellability::acceptance_freezes_rules_quantity_and_digests_in_the_receipt`,
`sellability::a_replay_after_a_catalog_change_answers_the_frozen_receipt`,
`sellability::acceptance_refuses_a_definition_not_enabled`,
`sellability::per_plan_unit_limits_are_validated_at_the_accepted_quantity`,
`sellability::a_fractional_quantity_with_rules_is_invalid_quantity`,
`sellability::a_quota_free_sale_keeps_its_receipt_bytes`,
`acceptance_receipts::the_receipt_read_returns_the_frozen_snapshot` and Products' in-crate e2e
`api::rest::quota::plan_rules_e2e_tests::a_purchase_of_two_units_freezes_20_vcpu_and_40_gb`. Implementation markers
in `pricing-sdk/src/read.rs`, `pricing-sdk/src/acceptance.rs`, `pricing-sdk/src/digest.rs`,
`pricing/src/infra/commercial_terms/wire/v1.rs`, `pricing/src/infra/pricing_reads.rs` and
`pricing/src/infra/commercial_terms/check.rs`.

Resolve answers a revision's quota terms from its stored rules, with no catalog call, and none for a revision without
rules; `GET /resolve` carries them too. `selected_quota_digest` follows the value a sale requests, never the default
chain. A sale of a revision with rules needs the preview's digest, and a missing or other digest is
`RESOLUTION_CHANGED` even when no price changed. Acceptance validates the selected rules through Products at the
accepted quantity and freezes the rules, their evidence, each final limit, the original quantity and the selected and
snapshot digests in the receipt, in the acceptance transaction; a replay answers the frozen receipt after any catalog
change. The three commercial structs gain optional members in place, and a quota-free sale's digests, receipt bytes,
seam fixtures and vectors are unchanged (D-547, D-548).

Requirement: `cpt-cf-bss-pricing-fr-plan-quota-rules`; PRD AC #29.

## 6. Acceptance Criteria

Each criterion below corresponds to exactly one DoD above and cites [PRD §9](../PRD.md#9-acceptance-criteria).

| DoD | PRD criterion | Given / When / Then |
| --- | --- | --- |
| `cpt-cf-bss-pricing-dod-plan-quota-rules-store` | AC #26; `cpt-cf-bss-pricing-fr-plan-quota-rules` | Given stored rules on either engine, when a rule breaks a CHECK, names a missing item, or two replaces race on one version, then the storage refuses or stores one set; an item with rules cannot be deleted, the set's digest ignores input order, and `down` refuses while a rule exists. |
| `cpt-cf-bss-pricing-dod-plan-quota-rules-authoring` | AC #26; `cpt-cf-bss-pricing-fr-plan-quota-rules` | Given a draft with two items, when its author puts a complete set of rules then the set, its digest and a new ETag are saved together; the old ETag is STALE_REVISION, the same key and body replay the first answer, the same key with another body is IDEMPOTENCY_CONFLICT, an empty array clears the set, and a set with a foreign item, a fractional amount or two rules for one binding and metric changes nothing and names each rule and field. |
| `cpt-cf-bss-pricing-dod-plan-quota-rules-checks` | AC #27; `cpt-cf-bss-pricing-fr-plan-quota-rules` | Given a revision with rules, when its checks run then each failing rule is a red quota row naming the rule and field, with the evaluated revision version and digest; a quota-free revision's rows are as before, and deleting an item with rules is ITEM_HAS_QUOTA_RULES. |
| `cpt-cf-bss-pricing-dod-plan-quota-rules-governance` | AC #27, AC #28; `cpt-cf-bss-pricing-fr-plan-quota-rules` | Given a revision with rules, when it is submitted, voted, copied or cloned then submit refuses red quota rows, an apply refuses a definition withdrawn after submit, the copy's rules keep their definitions and point at the copied items, and the published revision keeps reading its rules after the withdrawal. |
| `cpt-cf-bss-pricing-dod-plan-quota-rules-acceptance` | AC #29; `cpt-cf-bss-pricing-fr-plan-quota-rules` | Given a published revision with 10 vCPU per plan unit, when a buyer accepts two units with the preview's quota digest then the receipt freezes the rule, its evidence, quantity 2 and both digests; a stale or missing digest is RESOLUTION_CHANGED even when no price changed. |

Verification uses domain tests, scoped repository tests on both backends and REST positive, denial and precondition
probes. The package's criterion Q-C65 (an increase) is Products' judgement only here: Pricing's new-sale acceptance
passes `initial_set`, and an increase acceptance waits for the package's feature 3.
