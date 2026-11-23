<!-- CONFLUENCE_TITLE: [BSS]: Products — Quota Set Validation (Feature) -->
<!-- Related: ../DECOMPOSITION.md, ../DESIGN.md, ../PRD.md, ../DECISIONS.md | Owners: BSS Product Catalog team -->

# Feature: Quota Set Validation

- [ ] `p1` - **ID**: `cpt-cf-bss-products-featstatus-quota-set-validation-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-bss-products-feature-quota-set-validation`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Pricing validates a quota set](#pricing-validates-a-quota-set)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [rule](#rule)
  - [whole](#whole)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [The quota-set validation for Pricing](#the-quota-set-validation-for-pricing)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

This feature is Products' part of the quotas package's feature 2, plan quota authoring and acceptance
([`cpt-cf-bss-quotas-feature-plan-rules`](../../../docs/quotas/features/0002-plan-rules.md)), on the contract of the
package's [DESIGN §3.3.1.1](../../../docs/quotas/DESIGN.md#3311-catalog-reads-for-plan-authoring) (P-D-275). Pricing
stores a plan revision's quota rules and decides what each of its boundaries refuses (pricing D-540, D-542); for
every save, check, submit, vote and acceptance it asks Products to judge the complete set against the catalog.
Products answers with every check it finds and each definition it resolved (P-D-275, P-D-276).

`ProductsQuotaCatalogClientV1::validate_quota_set` is a method of the trait Pricing already reads the catalog through
(P-D-272), called in the same process as Pricing's caller, under `read` on `quota_definition`. It has no REST
endpoint, writes nothing and needs no migration: it reads the definition versions of the quota catalog's store
(P-D-274) in three statements and judges them with a pure function. The feature has no design slice of its own:
[DESIGN](../DESIGN.md) §3.2, §3.3 and §3.4 are its design, and [DECOMPOSITION](../DECOMPOSITION.md) entry 2.7 places
it.

### 1.2 Purpose

Give Pricing one answer for a complete set of plan quota rules: whether each rule names an exact catalog version the
caller may read, applies to its SKU version, uses an allowed unit and basis, states an amount on the unit's grid that
converts exactly, selects a supported scope and names a sellable version; whether the rules of one logical binding fit
together; and whether a change of limits is an allowed one. Nothing is refused here: Pricing decides.

Requirements: `cpt-cf-bss-products-fr-quota-set-validation`. It serves the package's
`cpt-cf-bss-quotas-fr-plan-amount`, `cpt-cf-bss-quotas-fr-qualification-recheck` and
`cpt-cf-bss-quotas-fr-qualification-changes`.

### 1.3 Actors

`cpt-cf-bss-products-actor-pricing` calls `validate_quota_set` through the ClientHub as its caller. These callers need
`read` on `quota_definition` (P-D-273, P-D-275): the authors who save plan quota rules, the readers of a plan's checks
(single and batch), the plan submitter, the final approver (whose apply re-runs the checks) and the caller of an
acceptance (Orders' principal). A non-final vote keeps a refusal as an observation (pricing D-503) and needs no
grant.

### 1.4 References

- [PRD](../PRD.md): `fr-quota-set-validation`, AC #38.
- [DESIGN](../DESIGN.md): §3.2 (the quota catalog component), §3.3 (the quota-set validation and its codes), §3.4
  (products-sdk).
- [DECISIONS](../DECISIONS.md): P-D-275, P-D-276; P-D-265 (the caller's tenant only), P-D-267 (exact units), P-D-270
  (applicability and the mapping lineage), P-D-271 (availability), P-D-272 (the SDK client), P-D-273 (the grant).
- The quotas package: [feature 2](../../../docs/quotas/features/0002-plan-rules.md) (flow
  `cpt-cf-bss-quotas-flow-plan-rules` step `inst-select`, algorithm `cpt-cf-bss-quotas-algo-plan-rules` step
  `inst-validate`, DoD `cpt-cf-bss-quotas-dod-plan-rules`, criteria Q-C02, Q-C58 and Q-C65) and
  [DESIGN](../../../docs/quotas/DESIGN.md) §3.3.1.1–§3.3.1.3.
- Pricing: the plan quota rules feature, its decisions D-540 … D-548.

## 2. Actor Flows (CDSL)

### Pricing validates a quota set

- [x] `p1` - **ID**: `cpt-cf-bss-products-flow-quota-set-validate`

Implements the Products part of the package's `cpt-cf-bss-quotas-flow-plan-rules` step `inst-select` and
`cpt-cf-bss-quotas-algo-plan-rules` step `inst-validate`.

1. [x] - `p1` - Pricing calls `validate_quota_set` as its caller; ask `read` on `quota_definition` for the caller's tenant; then a malformed or oversized request is 400 VALIDATION - `inst-quota-set-authorize`
2. [x] - `p1` - Read every named version with its current availability and its mapping's target in three statements: the versions under the whole grant, their mappings under its tenant-only scope - `inst-quota-set-read`
3. [x] - `p1` - Judge each rule by algorithm rule and the whole set by algorithm whole - `inst-quota-set-judge`
4. [x] - `p1` - Answer every check found and each resolved definition; never a partial answer on a failed read, and a stored unit whose grid or factor does not read is a 500 - `inst-quota-set-answer`

## 3. Processes / Business Logic (CDSL)

### rule

- [x] `p1` - **ID**: `cpt-cf-bss-products-algo-quota-set-rule`

1. [x] - `p1` - The version exists in the scope, else QUOTA_RULE_INVALID on `definition_ref`; its stored digest is the rule's, else QUOTA_DEFINITION_CHANGED - `inst-quota-rule-definition`
2. [x] - `p1` - The definition's SKU is the rule's and was approved at the rule's SKU version or an earlier one, else QUOTA_RULE_INVALID on `definition_ref` - `inst-quota-rule-applicability`
3. [x] - `p1` - The basis is a supported basis and the unit an allowed unit, else QUOTA_RULE_INVALID on `basis` or `unit_ref` - `inst-quota-rule-unit`
4. [x] - `p1` - The amount is a canonical integer on the unit's grid within its bounds, and so is the final limit at the acceptance quantity, else QUOTA_AMOUNT_INVALID; it converts exactly into the canonical unit, else QUOTA_UNIT_INEXACT - `inst-quota-rule-amount`
5. [x] - `p1` - The subject selector is the scope policy's and the scope selection names exactly its required dimensions, each with a value of 1 to 255 printable ASCII bytes, else QUOTA_SCOPE_UNSUPPORTED - `inst-quota-rule-scope`
6. [x] - `p1` - A version that is not enabled is QUOTA_QUALIFICATION_UNAVAILABLE with its state and reason - `inst-quota-rule-availability`

### whole

- [x] `p1` - **ID**: `cpt-cf-bss-products-algo-quota-set-whole`

1. [x] - `p1` - The rules of one logical binding share the profile's content and one adapter target, else QUOTA_SET_INCOMPATIBLE on `logical_binding_key` - `inst-quota-set-compatible`
2. [x] - `p1` - The rules of one logical binding share one subject and scope selection, whatever their item or dimension value, else QUOTA_SET_INCOMPATIBLE on `scope_selection` - `inst-quota-set-scope`
3. [x] - `p1` - An initial set needs initial-set support; an increase or an unchanged set needs a prior limit for each rule's binding and metric and a rule for each prior limit (else on `prior`); an increase needs increase support and every final limit at least its prior one with one above; a decrease is refused; unchanged needs equal limits; else QUOTA_SET_INCOMPATIBLE on `change_kind` - `inst-quota-set-change`

## 4. States (CDSL)

No new state. The validation reads the stored definition versions and their current availability and writes
nothing; a version's availability changes only through the quota catalog's own records (P-D-271).

## 5. Definitions of Done

This definition owns this feature's 1 DoD. It implements Products' half of the package's
`cpt-cf-bss-quotas-dod-plan-rules`. Design constraints: `cpt-cf-bss-products-constraint-two-backends`.

### The quota-set validation for Pricing

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-quota-set-validation`

Verified by `quota::validation::tests::every_check_code_has_its_stable_token`,
`quota::validation::tests::a_validation_is_ok_exactly_without_checks`,
`quota::validation::tests::a_definition_ref_round_trips_with_a_string_version`,
`infra::storage::repo::quota_catalog_repo::quota_catalog_repo_tests::a_set_of_versions_reads_with_its_availability_in_two_statements`,
`infra::storage::repo::quota_catalog_repo::quota_catalog_repo_tests::a_version_not_stored_or_of_another_tenant_is_absent_from_the_set_read`,
`infra::storage::repo::quota_catalog_repo::quota_catalog_repo_tests::a_set_read_matches_each_definition_with_its_own_version`,
`infra::storage::repo::quota_catalog_repo::quota_catalog_repo_tests::the_mapping_facts_of_a_set_read_in_one_statement`,
`domain::quota::set::set_tests::an_exact_valid_rule_passes_with_its_definition`,
`domain::quota::set::set_tests::an_unknown_definition_is_rule_invalid_without_its_content`,
`domain::quota::set::set_tests::a_changed_digest_is_definition_changed`,
`domain::quota::set::set_tests::a_sku_version_before_the_approval_is_rule_invalid`,
`domain::quota::set::set_tests::a_unit_outside_the_allowed_units_is_rule_invalid`,
`domain::quota::set::set_tests::amounts_off_the_grid_or_not_canonical_are_amount_invalid`,
`domain::quota::set::set_tests::zero_is_an_explicit_limit_when_the_minimum_is_zero`,
`domain::quota::set::set_tests::an_unbounded_definition_still_needs_a_finite_amount`,
`domain::quota::set::set_tests::per_plan_unit_limits_are_judged_at_the_acceptance_quantity`,
`domain::quota::set::set_tests::an_amount_that_does_not_convert_exactly_is_unit_inexact`,
`domain::quota::set::set_tests::an_overflow_is_amount_invalid`,
`domain::quota::set::set_tests::scope_selection_must_name_exactly_the_required_dimensions`,
`domain::quota::set::set_tests::rules_of_one_binding_need_one_compatible_profile_and_one_adapter_target`,
`domain::quota::set::set_tests::rules_of_one_binding_share_one_scope_selection`,
`domain::quota::set::set_tests::a_definition_not_enabled_is_qualification_unavailable`,
`domain::quota::set::set_tests::an_increase_needs_increase_support_and_a_decrease_is_refused`,
`domain::quota::set::set_tests::nothing_increases_is_reported_only_without_a_decrease`,
`domain::quota::set::set_tests::a_prior_limit_without_a_rule_is_set_incompatible`,
`domain::quota::set::set_tests::an_initial_set_needs_initial_set_support`,
`domain::quota::set::set_tests::an_increase_judges_the_final_limits`,
`domain::quota::set::set_tests::limits_are_compared_in_the_canonical_unit`,
`domain::quota::set::set_tests::unchanged_needs_equal_limits`,
`domain::quota::set::set_tests::every_failure_is_reported_without_dropping_a_rule`,
`domain::quota::set::set_tests::a_malformed_request_is_refused_before_any_judgement`,
`domain::quota::set::set_tests::a_repeated_or_non_canonical_prior_limit_is_refused`,
`domain::quota::set::set_tests::oversized_inputs_are_refused_without_echoing_them`,
`domain::quota::set::set_tests::a_binding_key_in_a_detail_is_quoted`,
`domain::quota::set::set_tests::an_extra_scope_key_in_a_detail_is_quoted`,
`domain::quota::set::set_tests::a_rule_unit_in_a_detail_is_quoted`,
`domain::quota::set::set_tests::a_prior_metric_in_a_detail_is_quoted`,
`domain::quota::set::set_tests::a_prior_unit_in_a_detail_is_quoted`,
`infra::quota::catalog_client::catalog_client_tests::the_set_validation_answers_the_catalogs_definitions_and_their_availability`,
`infra::quota::catalog_client::catalog_client_tests::the_set_validation_reads_in_three_statements_after_the_pdp`,
`infra::quota::catalog_client::catalog_client_tests::a_definition_outside_the_grant_is_unknown_to_the_set`,
`infra::quota::catalog_client::catalog_client_tests::a_grant_narrowed_to_the_definition_resolves_it_in_the_set`,
`infra::quota::catalog_client::catalog_client_tests::validation_asks_read_on_quota_definition_for_the_callers_tenant`,
`infra::quota::catalog_client::catalog_client_tests::a_malformed_set_is_400_validation`,
`infra::quota::catalog_client::catalog_client_tests::a_pdp_outage_is_503_for_a_set`,
`infra::quota::catalog_client::catalog_client_tests::a_caller_without_the_grant_is_403_before_the_request_is_read`,
`infra::quota::catalog_client::catalog_client_tests::another_tenants_definition_is_unknown_to_the_set`,
`infra::quota::catalog_client::catalog_client_tests::a_stored_definition_that_does_not_read_is_500_for_a_set`,
`infra::quota::catalog_client::catalog_client_tests::a_stored_unit_grid_that_does_not_read_is_500_for_a_set`,
`infra::quota::catalog_client::catalog_client_tests::vcpu_and_ram_approved_on_one_target_share_a_binding`,
`infra::quota::catalog_client::catalog_client_tests::vcpu_and_ram_approved_on_two_targets_are_refused_in_one_binding`,
`api::rest::quota::plan_rules_e2e_tests::vcpu_and_ram_of_two_adapter_targets_cannot_share_a_binding`,
`postgres_quota_catalog::the_definition_versions_of_a_set_read_on_postgres` and
`postgres_quota_catalog::a_set_read_matches_each_definition_with_its_own_version_on_postgres`. Implementation markers in
`products-sdk/src/quota/validation.rs`, `products/src/domain/quota/set.rs`, `products/src/infra/quota/catalog_client.rs`,
`products/src/infra/storage/repo/quota_catalog_repo.rs` and `products/src/api/rest/quota/definitions.rs`.

`products-sdk` adds the validation contract: the request, the answer, the seven stable check codes and
`QuotaDefinitionRef`, and the method on `ProductsQuotaCatalogClientV1`. The method asks `read` on `quota_definition` for
the caller's tenant before it reads anything and refuses a malformed or oversized request with 400 `VALIDATION` on its
field. It reads the named versions and their current availability under the whole grant, so a definition outside the
grant or of another tenant is unknown and none of its content is answered, and the mapping facts of the resolved
versions; three statements whatever the set holds, with no registry or plugin call. A resolved version that does not
project, or whose allowed units' grids or factors do not read, is a 500 and no partial answer. The pure judgement checks
each rule (the exact version and its digest, the SKU and SKU version, the basis and unit, the amount on its grid and the
final limit at the quantity, the exact conversion, the scope, the availability) and the whole set (one profile content
and one adapter target per binding, one scope per binding whatever the dimension value, a prior limit for each rule and
a rule for every prior limit, the change kind), and reports every failure without dropping a rule; every caller text a
detail repeats is quoted, escaped and cut after 32 characters. The answer carries one verdict per rule, in request
order, and every check (DESIGN §3.3; P-D-275, P-D-276).

## 6. Acceptance Criteria

Each criterion below corresponds to exactly one DoD above and cites [PRD §9](../PRD.md#9-acceptance-criteria).

| DoD | PRD trace | Given / When / Then |
| --- | --- | --- |
| `cpt-cf-bss-products-dod-quota-set-validation` | AC #38; `cpt-cf-bss-products-fr-quota-set-validation` | Given definitions approved in the caller's tenant, one of them not enabled, and one of a second tenant, when Pricing validates sets that name an unknown or foreign version, a changed digest, another SKU, a unit outside the allowed ones, an amount off the grid or a fraction, a wrong scope selection, rules of one binding on two adapter targets, or a decrease, then each set is answered with each check on its rule and field; a valid set answers its definitions; a version that is not enabled is reported with its state and reason; a malformed request is 400 `VALIDATION`. |
