# S2-09 draft authoring and field classification

Status: **implemented, independently gap-fix reviewed and locally verified** ([implementation report](../../../../../artifacts/orders-lifecycle-s2-20261006/S2-09-implementation.md), [gap review](../../../../../artifacts/orders-lifecycle-s2-20261006/S2-09-gap-review.md)). Scope: the [Capture contract](../features/02-capture.md) and the [authored field classification](../DESIGN.md#contract-02-4-3). Wire bindings are recorded in [D-206](../DECISIONS.md#d-206--authoring-wire-bindings-snake_case-bodies-typed-createline-results-removal-body-unauthored-draft-cycle-s2-09).

Draft authoring is the first delivered business surface. The five authoring routes and the matching local SDK methods run one shared service. Every write enters the S2-04 engine: the dedicated create branch, or row 2 `draft-mutate`. Nothing calls a catalog, Pricing, Products or Contracts port. Administrative edits (row 3), submit and every later operation stay explicitly unavailable until their packages ship.

## Delivered

| Area | Implementation |
|---|---|
| SDK contract | [authoring.rs](../../orders-lifecycle-sdk/src/authoring.rs): `CreateOrder`, `AddLine`, `HeaderPatch`, `LinePatch` (presence names a field; `null` clears a nullable one), `Category` (registered GTS IDs; registration is not admission), `Currency`, positive exact-decimal `Quantity`, D-193 `AuthoredTerm`, `BillingCycle`, `OrderView`. [api.rs](../../orders-lifecycle-sdk/src/api.rs) adds `create`, `patch_order`, `add_line`, `patch_line`, `remove_line`. `TransitionResult` gains `line_id`; all body names are `snake_case` (D-206) |
| Shared service | [infra/capture.rs](../../orders-lifecycle/src/infra/capture.rs) `CaptureService`: boundary-validated input → field classification → engine. Admin-only PATCH answers unavailable before any authorization, audit, registry or write. Draft preparation builds the guard facts from the engine's coherent snapshot, the header edit and the child-document writer (insert/replace/remove through `ChildDocuments`). `LocalOrdersClient` registers the same service in `ClientHub` |
| Classification and guards | [domain/capture.rs](../../orders-lifecycle/src/domain/capture.rs): trigger selection from the shared declaration only; the six row-2 capture guards (membership, mixed classes, fixed seller, category, currency, line cap) and the create category guard, bound to the registered names; D-193 term columns |
| Field declaration | [contributions.rs](../../orders-lifecycle/src/domain/contributions.rs) `FIELD_CLASSES` now uses the `snake_case` wire names of `models.json`; a test pins it to the catalog's PATCH names |
| Engine (S2-04 owner change) | `AggregateContribution::DraftEdit(DraftHeaderEdit)` carries named category/contract values, applied in the engine's post-state; `TransitionRequest.proposed` is an `ArrangementDelta` completed over the PEP-authorized current facts; `Prepared.line_id` enters the settled body; create answers `OrderView`; refusal `context.data` is `snake_case` |
| REST | [api/rest/capture.rs](../../orders-lifecycle/src/api/rest/capture.rs) with [dto.rs](../../orders-lifecycle/src/api/rest/dto.rs) OpenAPI mirrors (pinned to the SDK wire by tests). Boundary order: `If-Match` (428) → `Idempotency-Key` → `X-Delegation-Proof-Ref` → `expected_draft_revision` → body. Settled outcomes render verbatim. Mounted in [gear.rs](../../orders-lifecycle/src/gear.rs) only after a successful init |
| Storage (S2-02 owner change) | `orders_draft_content.billing_cycle` is nullable (CONTRACTS: cycle may be incomplete in draft); `bss_orders__term_valid` keeps a period count without an interval until the cycle is authored. Inventory regenerated |
| Configuration | Optional `capture.line_cap` (1..200, default the 200-line baseline) |

## Evidence

| Requirement | Tests |
|---|---|
| Empty draft v1/rev 0, seller-unique number, trusted actor and sales path, retry identity, eventless, no lines/pin/total | PG `create_commits_an_empty_draft_with_trusted_evidence_and_replays_it` |
| Category refusal creates nothing; seller-role-only principal denied; PDP outage fails closed; unready gear writes nothing | PG `create_refusals_and_pdp_outage_create_nothing`; gear `postgres_boot_registers_unavailable_sdk_and_shutdown_is_cooperative` (HTTP 503 before readiness, 428 for missing `If-Match`) |
| Stable server line IDs, same-key replay, authored values retained, removed IDs never reused, revision +1 per write, version stays 1, v3 audit, no event | PG `authoring_keeps_stable_line_identities_and_increments_only_the_draft_revision` |
| Currency, mixed classes, seller, category, omitted revision, precedence, line cap, edits do not consume cap, differing cycles valid, settled refusal replay | PG `structural_and_field_refusals_follow_the_declared_precedence`; unit `structural_guards_refuse_with_their_registered_reasons`, `guard_precedence_follows_registration_and_the_engine_checks_come_first` |
| Outside draft `not-admissible` first (revision omitted); admin-only PATCH unavailable with no effect | PG `outside_draft_inadmissibility_wins_and_administrative_edits_stay_unavailable` |
| Concurrent writes at one revision | PG `concurrent_writes_at_one_revision_commit_exactly_once` |
| Header category/contract written by the engine; payer/resource changes under complete proposed authorization; unauthorized payer refused unsettled | PG `header_and_axis_edits_are_written_by_the_engine_under_complete_authorization`; unit `draft_header_edits_write_only_named_values_and_increment_the_revision_once` |
| One declaration; classes alone select the trigger; boundary validation; wire pinning | unit `trigger_selection_uses_the_field_classes_alone`, `every_patch_wire_field_is_classified_once_in_the_shared_declaration`, `authored_values_are_bounded_structurally`, `term_columns_preserve_intent_and_calendar_units`, REST `write_metadata_follows_the_catalog_boundary_order`, `patch_bodies_name_fields_exactly`, `openapi_mirrors_are_pinned_to_the_sdk_wire` |
| Same-key retry of a committed payer/resource edit replays (fingerprint binds the established arrangement, D-206 item 6) | PG `header_and_axis_edits_are_written_by_the_engine_under_complete_authorization` (retry of `h-3`) |
| Only `draft-mutate` (resource/payer) and `amendment` (payer) may propose an axis change | unit `only_draft_edits_and_amendments_propose_axis_changes`; PG `only_draft_mutation_and_amendment_may_propose_an_axis_change` |
| Startup refuses an unclassified authoring wire field (names read from the SDK serde derives) | unit `startup_refuses_an_unclassified_authoring_wire_field` |
| SDK settled refusals carry the stored Problem diagnostics (D-206 item 5) | PG `the_local_sdk_returns_settled_refusals_with_their_diagnostics` |
| Wire through the mounted router on real PostgreSQL: 201/ETag/Location, replay, snake_case bodies, problem+json refusals, optional DELETE body, admin 503 | PG `the_mounted_routes_carry_engine_outcomes_on_the_wire` (in-process HTTP, not the S2-12 live E2E); gear test pins the optional DELETE `requestBody` |
| Seller-role-only principal (with a seller read/hold/resume grant) cannot edit or add lines | PG `create_refusals_and_pdp_outage_create_nothing` |

## Not delivered here

- Administrative edits (row 3) are S4-05. Draft reads and S6-04 access logging are the early-S6 package; S2-09 exposes no read.
- Date cascade and policy are delivered by S2-10 ([DATES](DATES.md)). Throttling and the route census (S2-12), capture metrics/alerts (S6-06), live HTTP E2E (S2-12).
- REST correlation propagation is not wired (`correlation_id` is `None`); create and draft mutation are eventless, so no event carries it yet.
