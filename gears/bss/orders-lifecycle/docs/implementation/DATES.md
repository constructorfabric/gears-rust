# S2-10 date policy and admission-time date preparation

Status: **implemented, independently gap-fix reviewed and locally verified** ([implementation report](../../../../../artifacts/orders-lifecycle-s2-20261006/S2-10-implementation.md), [gap review](../../../../../artifacts/orders-lifecycle-s2-20261006/S2-10-gap-review.md)).

- **Scope:** the [date cascade](../features/02-capture.md#contract-02-4-2), [`orders_date_policy`](../DESIGN.md#contract-02-table-orders_date_policy), the [policy channel](../DESIGN.md#38-deployment-topology) and Capture DoD 5.3.
- **Decisions:** [D-207](../DECISIONS.md#d-207--the-date-policy-channel-is-a-startup-promotion-the-date-basis-is-strict-and-frozen-s2-10).

Preview, submit (S3-12) and amendment (S4) are not delivered, so no public operation prepares dates yet. S2-10 supplies four things those packages call:

- the pure resolver;
- the frozen basis and its `capture.date-basis` guard predicate;
- the composite gate's date failures;
- the admitted-line builder.

It also delivers the policy channel and the startup default check. The engine-level behaviour is proven through the S2-04 engine's submit row on real PostgreSQL.

## Delivered

| Area | Implementation |
|---|---|
| Pure cascade | [domain/dates.rs](../../orders-lifecycle/src/domain/dates.rs) has these parts. `DatePolicySnapshot::effective` takes the tenant row, else the default. A missing default is `Missing`; a bad revision, a duplicate, a foreign row or a default under another ID is `Invalid`. `resolve_line` retains supplied values. It defaults an unauthored contract-effective date to the proposed UTC date, and an optional dependent date to the contract-effective date. A required dependent date is never defaulted, which is how D-60 is enforced. `DateBasis` holds the frozen run basis, with `failures()` for the gate, `verdict_at(t)`/`guard()` for `capture.date-basis`, and `frozen()`/`from_frozen()` for the D-188 attempt. `unresolvable_policy_guard()` is the always-failing guard for a missing policy |
| Policy channel | [infra/dates.rs](../../orders-lifecycle/src/infra/dates.rs) `DatePolicyPlan` validates the optional `date_policy` configuration ([config.rs](../../orders-lifecycle/src/config.rs)). `promote` reads first, then applies changes in one transaction through the S2-02 `replace_date_policy`/`insert_date_policy` and the new fenced `delete_date_policy_override` ([repo/mutable.rs](../../orders-lifecycle/src/infra/storage/repo/mutable.rs)), with revisions above the namespace high-water mark. `install` = promote + `verify_platform_default`, called by gear `init` ([gear.rs](../../orders-lifecycle/src/gear.rs)) before any client or route is published |
| Preparation | `DatePreparer::prepare` reads the effective rows together with the database clock in one statement (`repo::private::date_policy_rows_at`) and resolves every line. `PreparationClock::Fixed` is test-only. `supplied_from_draft` reads authored values only and assumes no billing cycle; `supplied_from_admitted` carries amendment values forward |
| Admission | `admitted_line` builds an admitted `orders_order_line` row from a draft line. It refuses a basis prepared for another resource tenant (`DateBasis.resource_tenant_id`) and a basis whose supplied values are not exactly the line's authored dates. It sets the three resolved dates and `date_policy_switch_state` = the identical snapshot, and copies the authored term (intent, kind, duration, cycle) and content verbatim. Pricing/pin/overlap columns stay with S3-12/S4. A missing cycle or an unresolved line is refused |
| Storage | The `date_policy` entity has the PEP property `resource_tenant_id`. Migration 07 adds `bss_orders__date_snapshot_valid` and `ck_bss_orders__order_line__dates`, so admitted lines need both dependent dates and a well-formed snapshot. The inventory is regenerated |
| E2E host | [e2e-orders-lifecycle.yaml](../../config/e2e-orders-lifecycle.yaml) declares the platform default explicitly (unchanged from the seed) |

## Evidence

| Requirement (source) | Tests |
|---|---|
| All switch combinations × absent/explicit dates; defaults never satisfy a required date; optional dates default to contract-effective; supplied values retained (02 §4.2, §3.2 steps 2-3; D-60) | unit `every_switch_and_presence_combination_follows_the_cascade` (4 × 8 cases), `the_basis_reports_every_failing_line_for_the_gate` |
| Effective-row selection; missing/invalid policy never permissive (02 §4.2; DESIGN 02 §3.7) | unit `the_tenant_row_wins_and_the_platform_default_is_the_fallback`, `a_missing_or_invalid_policy_never_invents_permissive_switches`; PG `a_missing_platform_default_fails_startup_and_preparation`, `preparation_snapshots_the_effective_row_and_the_database_utc_date` |
| Midnight rollover with an injected clock; offset normalisation (02 §4.2; 03 step 15) | unit `a_utc_day_rollover_refuses_and_the_same_day_passes`; PG `a_utc_day_rollover_refuses_without_admission_and_a_fresh_attempt_commits`, through the engine's row 4 under the lock: settled `DATE_CASCADE_INVALID` 400; no version 2, line or total; same-key replay identical; fresh basis commits |
| Required unauthored date fails the gate; no admission snapshot on refusal (02 AC; 03 §4.2 predicate 8) | PG `a_required_unauthored_date_refuses_at_the_gate_without_admission` |
| Persist three resolved dates and the identical snapshot; exact term preservation; due date is not assent; later policy promotion and reads do not rewrite; amendment re-snapshots and carries values; frozen basis restores exactly (02 §4.2, §3.2 step 6; D-121, D-188) | PG `admission_persists_the_resolved_triple_and_snapshot_immutably`; unit `the_frozen_basis_round_trips_and_rejects_tampering` |
| Schema refuses NULL dependent dates or a malformed snapshot on an admitted line | PG `admission_persists_the_resolved_triple_and_snapshot_immutably`: 10 malformed snapshots and both NULL dates refused; the valid row is accepted |
| Policy promotion: explicit switches, validation, atomic, monotonic revisions, idempotent, runtime role cannot write, retire and re-create (DESIGN 02 §3.7, §3.8) | unit `promotion_config_is_explicit_and_validated`; PG `the_policy_channel_promotes_atomically_with_monotonic_revisions` |
| Admitted-line CHECK shapes: NULL dependent dates refused under either snapshot scope; crossed scope/identity refused; both well-formed scopes accepted (D-208) | PG `admission_persists_the_resolved_triple_and_snapshot_immutably` (`check_shapes`) |
| Stale-basis refusal carries no `context.data`; the basis is identified by the attempt's frozen basis and the refusal audit's instant (D-208) | PG `a_utc_day_rollover_refuses_without_admission_and_a_fresh_attempt_commits` |
| Concurrent replicas promoting the same plan both start: the waiting promotion re-reads under the namespace lock (gap review) | PG `a_promotion_waiting_behind_a_concurrent_one_sees_its_committed_rows` |
| Startup promotes, is idempotent on reboot, refuses an invalid promotion and a missing default without publishing a client (02 §5.3, AC "startup rejects … missing platform date policy") | gear `boot_promotes_the_date_policy_and_refuses_a_missing_default` |

## Not delivered here

- **Preview, submit and amendment** are not delivered. These later packages call `DatePreparer::prepare` before any date-dependent call, freeze `DateBasis::frozen()` into the attempt, bind `capture.date-basis`, and report `failures()` in the composite gate:
  - Preview and submit: S3-12;
  - amendment: S4-04/S4.
- **Missing-required-date metric** (02 §3.8): S6-06.
- **Production deployment:** the promotion step needs a login holding `bss_orders_policy`, which is part of S6 deployment identity binding.
- **Draft reads** (early-S6) return stored values only; the resolved triple exists only on admitted lines.
