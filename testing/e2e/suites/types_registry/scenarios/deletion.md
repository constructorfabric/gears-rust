# Entity deletion scenarios

End-to-end deletion scenarios cover both routes, dependency ordering, partial success, tombstones and dry run. The local launcher uses **SQLite**; these tests do not prove PostgreSQL/MySQL or tenant/PDP behavior.

Each scenario describes its starting entities, requests, terminal item outcomes and observable reads; the [suite README](../README.md) lists the test file of each group. Scenarios proved by one workflow share a test: TR-DEL-201 with 404, 203 with 213, 301 with 305, 302 with 209, and 403 with 208. Each scenario lists its source fixtures before Given; named variants are described in Given. All fixture links belong to `fixtures/deletion/`.

Unless stated otherwise, setup registrations and revisions have completed, each mutation uses a fresh `Idempotency-Key`, and deletion supplies the target's current positive `expected_resource_version`. Accepted requests return `202` with an operation `Location`; polling reaches `completed`. Outcomes preserve request order and echo each key as `entity_key`: a GTS ID as sent, a UUID in canonical lowercase hyphenated form however it was sent; an unknown UUID produces an item refusal under its own key rather than an invented identifier. Synchronous refusals return Problem JSON without an operation `Location` and change no entity.

Successful deletion increments `resource_version` once and leaves a readable tombstone with the same UUID and content. Refused items have no resulting version and leave their entities unchanged. Reads explicitly select the compared fields; conditional reads retain the same projection. `A → B` means A depends on B, so A must be deleted first. Named schema copies change both `gts_id` and `content.$id`; version or base changes also update the affected chain segments and `$ref` targets. Every scenario uses a fresh namespace.

## Contents

- [Successful deletion](#successful-deletion)
  - [TR-DEL-001 — Delete one entity and leave a readable tombstone](#tr-del-001--delete-one-entity-and-leave-a-readable-tombstone)
  - [TR-DEL-002 — Match mixed-key batch outcomes in request order](#tr-del-002--match-mixed-key-batch-outcomes-in-request-order)
  - [TR-DEL-003 — Delete an Instance without deleting its schema or sibling](#tr-del-003--delete-an-instance-without-deleting-its-schema-or-sibling)
  - [TR-DEL-004 — Delete through a previously issued Registry Reference](#tr-del-004--delete-through-a-previously-issued-registry-reference)
  - [TR-DEL-005 — Delete the current content after several revisions](#tr-del-005--delete-the-current-content-after-several-revisions)
- [Preconditions and refusals](#preconditions-and-refusals)
  - [TR-DEL-101 — Recover from a genuinely stale deletion precondition](#tr-del-101--recover-from-a-genuinely-stale-deletion-precondition)
  - [TR-DEL-102 — Refuse an absent identifier without reserving it](#tr-del-102--refuse-an-absent-identifier-without-reserving-it)
  - [TR-DEL-103 — Refuse a new deletion request for a tombstone](#tr-del-103--refuse-a-new-deletion-request-for-a-tombstone)
  - [TR-DEL-104 — Preserve independent successes beside item refusals](#tr-del-104--preserve-independent-successes-beside-item-refusals)
  - [TR-DEL-105 — Reject missing preconditions and misleading conditional headers](#tr-del-105--reject-missing-preconditions-and-misleading-conditional-headers)
  - [TR-DEL-106 — Require request identity on both deletion routes](#tr-del-106--require-request-identity-on-both-deletion-routes)
  - [TR-DEL-107 — Reject duplicate entities hidden behind different key forms](#tr-del-107--reject-duplicate-entities-hidden-behind-different-key-forms)
  - [TR-DEL-108 — Unknown UUIDs fail per item without blocking valid neighbours](#tr-del-108--unknown-uuids-fail-per-item-without-blocking-valid-neighbours)
- [Dependency graph and partial deletion](#dependency-graph-and-partial-deletion)
  - [TR-DEL-201 — A live Instance blocks deletion of its conforming schema](#tr-del-201--a-live-instance-blocks-deletion-of-its-conforming-schema)
  - [TR-DEL-202 — Delete an Instance before its schema despite request order](#tr-del-202--delete-an-instance-before-its-schema-despite-request-order)
  - [TR-DEL-203 — A derived schema blocks deletion of its base](#tr-del-203--a-derived-schema-blocks-deletion-of-its-base)
  - [TR-DEL-204 — A schema reference blocks deletion of its target](#tr-del-204--a-schema-reference-blocks-deletion-of-its-target)
  - [TR-DEL-205 — Delete a complete derivation and conformance chain](#tr-del-205--delete-a-complete-derivation-and-conformance-chain)
  - [TR-DEL-206 — Delete every branch before their shared target](#tr-del-206--delete-every-branch-before-their-shared-target)
  - [TR-DEL-207 — Keep partial success when a dependant is outside the batch](#tr-del-207--keep-partial-success-when-a-dependant-is-outside-the-batch)
  - [TR-DEL-208 — A failed dependant deletion leaves its target blocked](#tr-del-208--a-failed-dependant-deletion-leaves-its-target-blocked)
  - [TR-DEL-209 — Remove a blocker and retry deletion of its target](#tr-del-209--remove-a-blocker-and-retry-deletion-of-its-target)
  - [TR-DEL-210 — Only the current revision's references protect a target](#tr-del-210--only-the-current-revisions-references-protect-a-target)
  - [TR-DEL-211 — An x-gts-ref constraint does not protect its named entity](#tr-del-211--an-x-gts-ref-constraint-does-not-protect-its-named-entity)
  - [TR-DEL-212 — Count direct dependants rather than the transitive closure](#tr-del-212--count-direct-dependants-rather-than-the-transitive-closure)
  - [TR-DEL-213 — Multiple dependency kinds do not multiply dependent entities](#tr-del-213--multiple-dependency-kinds-do-not-multiply-dependent-entities)
- [Request identity and idempotency](#request-identity-and-idempotency)
  - [TR-DEL-301 — Replay a successful deletion without changing its tombstone](#tr-del-301--replay-a-successful-deletion-without-changing-its-tombstone)
  - [TR-DEL-302 — Replay a refused deletion even after its blocker disappears](#tr-del-302--replay-a-refused-deletion-even-after-its-blocker-disappears)
  - [TR-DEL-303 — Changed targets or preconditions conflict with a used key](#tr-del-303--changed-targets-or-preconditions-conflict-with-a-used-key)
  - [TR-DEL-304 — Dry run and real deletion are different requests](#tr-del-304--dry-run-and-real-deletion-are-different-requests)
  - [TR-DEL-305 — Replay across deletion routes and equivalent entity keys](#tr-del-305--replay-across-deletion-routes-and-equivalent-entity-keys)
- [Dry-run deletion](#dry-run-deletion)
  - [TR-DEL-401 — Predict a single deletion without changing entity state](#tr-del-401--predict-a-single-deletion-without-changing-entity-state)
  - [TR-DEL-402 — Predict deletion of a complete graph using virtual changes](#tr-del-402--predict-deletion-of-a-complete-graph-using-virtual-changes)
  - [TR-DEL-403 — Predict partial success without removing failed blockers](#tr-del-403--predict-partial-success-without-removing-failed-blockers)
  - [TR-DEL-404 — Predict a refusal caused by a dependant outside the batch](#tr-del-404--predict-a-refusal-caused-by-a-dependant-outside-the-batch)
  - [TR-DEL-405 — A passing dry run does not reserve deletion eligibility](#tr-del-405--a-passing-dry-run-does-not-reserve-deletion-eligibility)
- [Tombstones and subsequent use](#tombstones-and-subsequent-use)
  - [TR-DEL-501 — Retain full reads and invalidate the old ETag after deletion](#tr-del-501--retain-full-reads-and-invalidate-the-old-etag-after-deletion)
  - [TR-DEL-502 — Follow deletion through discovery and exact reads](#tr-del-502--follow-deletion-through-discovery-and-exact-reads)
  - [TR-DEL-503 — A readable tombstone cannot become a new live dependency](#tr-del-503--a-readable-tombstone-cannot-become-a-new-live-dependency)
- [Version-family lifecycle](#version-family-lifecycle)
  - [TR-DEL-601 — Delete major members independently of version succession](#tr-del-601--delete-major-members-independently-of-version-succession)
  - [TR-DEL-602 — Delete a middle minor while a higher minor remains active](#tr-del-602--delete-a-middle-minor-while-a-higher-minor-remains-active)
  - [TR-DEL-603 — Admit the next minor after deleting its predecessor](#tr-del-603--admit-the-next-minor-after-deleting-its-predecessor)
  - [TR-DEL-604 — Compare a new minor against the deleted predecessor](#tr-del-604--compare-a-new-minor-against-the-deleted-predecessor)
  - [TR-DEL-605 — A tombstone still fixes its major's shape](#tr-del-605--a-tombstone-still-fixes-its-majors-shape)
  - [TR-DEL-606 — Major zero keeps ordinary deletion safety](#tr-del-606--major-zero-keeps-ordinary-deletion-safety)

## Successful deletion

### TR-DEL-001 — Delete one entity and leave a readable tombstone

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json).

**Given:** register `person_schema` and read it at `lifecycle_status=active`, `resource_version=1`.

**When:** `DELETE {registry_api}/entities/{gts_id}?expected_resource_version=1`, then await completion.

**Then:** the item succeeds at version 2. The full selected tombstone preserves its UUID, content and applicable effective documents. Lifecycle, resource version and mutation timestamp reflect deletion.

### TR-DEL-002 — Match mixed-key batch outcomes in request order

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [other_schema](../fixtures/deletion/other_schema.json), [person_instance](../fixtures/deletion/person_instance.json).

**Given:** register `person_schema`, `person_instance`, `other_schema`, and `spare_schema`, a copy of `other_schema` with entity name `spare`. `other_schema` sorts before `spare_schema`. Read all UUIDs and versions.

**When:** batch-delete `[spare_schema by UUID, person_instance by GTS ID, other_schema by UUID]`.

**Then:** outcomes name `[spare_schema, person_instance, other_schema]` in that order and all succeed. Each deleted entity advances one version; `person_schema` remains active and unchanged.

### TR-DEL-003 — Delete an Instance without deleting its schema or sibling

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [person_instance](../fixtures/deletion/person_instance.json).

**Given:** register `person_schema`, `person_instance`, and `second_person_instance`, a copy of `person_instance` whose final entity name is `bob` and whose content has `name: "Bob"`. All are active at version 1.

**When:** delete only `person_instance` with expected version 1.

**Then:** `person_instance` is a readable version-2 tombstone preserving its value. `person_schema` and `second_person_instance` remain active with their original content, identity and versions.

### TR-DEL-004 — Delete through a previously issued Registry Reference

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json).

**Given:** register isolated `person_schema` and obtain its `gts_uuid` from an exact read.

**When:** use that UUID as the single DELETE path key with the observed version.

**Then:** the outcome echoes the UUID as its `entity_key`. Reading by either key returns the same tombstone.

### TR-DEL-005 — Delete the current content after several revisions

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json).

**Given:** register isolated `person_schema`, then revise its `content.title` first to `Person revision 2` and then to `Person revision 3`. Both revisions succeed; the entity is at resource version 3. Read its current content and effective documents.

**When:** delete it with expected version 3.

**Then:** deletion succeeds at version 4. The tombstone retains the latest content and effective documents, without reverting to either earlier definition.

## Preconditions and refusals

### TR-DEL-101 — Recover from a genuinely stale deletion precondition

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json).

**Given:** register and read `person_schema` at version 1, then complete a revision adding `content.title: "Revised Person"` to reach version 2. Retain the earlier read's version.

**When:** delete with expected version 1; after the refusal, read again and retry with version 2 under a new key.

**Then:** the first submission returns `202` and terminal `precondition_failed`. The revised active entity remains intact at version 2. The retry succeeds and leaves a version-3 tombstone containing the revised definition.

### TR-DEL-102 — Refuse an absent identifier without reserving it

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json).

**Given:** load `person_schema` but do not register it; its GTS ID has never been admitted.

**When:** delete by that ID with expected version 1.

**Then:** the request is accepted and its item fails `precondition_failed`. An exact read still reports absence. A subsequent valid creation under a new key succeeds at version 1: the failed deletion created no identity reservation.

### TR-DEL-103 — Refuse a new deletion request for a tombstone

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json).

**Given:** register `person_schema`, then successfully delete it from version 1 to version 2.

**When:** submit new deletion requests under fresh keys, first with the old version 1 and then with the tombstone's version 2.

**Then:** both accepted requests complete with `status=failed`, `error.reason=not_active` and no resulting resource version. Lifecycle is checked before version, so both the stale and current positive versions give the same reason. Neither request reports `succeeded` or `unchanged`; the tombstone, including its version and ETag, stays unchanged.

### TR-DEL-104 — Preserve independent successes beside item refusals

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [other_schema](../fixtures/deletion/other_schema.json).

**Given:** register `person_schema` at version 1 and `other_schema`, then revise `other_schema` by adding `content.title: "Revised Other"` to reach version 2. Prepare `absent_schema`, a copy of `person_schema` with entity name `absent`, without registering it.

**When:** batch-delete `[person_schema at 1, other_schema at 1, absent_schema at 1]`.

**Then:** `person_schema` succeeds at version 2; `other_schema` and `absent_schema` fail `precondition_failed`. `other_schema` retains its current content at version 2, `absent_schema` remains absent, and results preserve request order.

### TR-DEL-105 — Reject missing preconditions and misleading conditional headers

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [other_schema](../fixtures/deletion/other_schema.json).

**Given:** register independent `person_schema` and `other_schema`. Read their versions and the ETag of `person_schema`.

**When:** separately submit a single deletion without `expected_resource_version`, a batch naming `other_schema` with its correct version and `person_schema` without its version, and each deletion route with a valid explicit version plus `If-Match` carrying the read ETag.

**Then:** each request fails synchronously with `400`. No entity changes; the batch does not partially execute. `If-Match` cannot substitute for, or supplement, the explicit version precondition.

### TR-DEL-106 — Require request identity on both deletion routes

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json).

**Given:** register isolated `person_schema` at version 1.

**When:** submit a valid single deletion and a valid batch deletion, each without `Idempotency-Key`.

**Then:** both requests fail synchronously with `400`; the entity remains active at its original version.

### TR-DEL-107 — Reject duplicate entities hidden behind different key forms

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [other_schema](../fixtures/deletion/other_schema.json).

**Given:** register independent `person_schema` and `other_schema`, and obtain `person_schema`'s UUID from a read.

**When:** batch-delete `[person_schema by GTS ID, other_schema, person_schema by UUID]`, with correct versions.

**Then:** the request fails synchronously with `400` and a duplicate-identity validation error on `entity_key` naming positions `items[0]` and `items[2]`; neither `person_schema` nor `other_schema` changes. Duplicate detection concerns resolved identity, not key spelling.

### TR-DEL-108 — Unknown UUIDs fail per item without blocking valid neighbours

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [other_schema](../fixtures/deletion/other_schema.json).

**Given:** register isolated `person_schema` at version 1. Leave `other_schema` absent and prepare `unregistered_schema`, a copy of `other_schema` with entity name `unregistered`, without registering it; its Registry Reference is the unknown UUID. Confirm both missing keys are absent by exact read.

**When:** first submit single-route deletion of the unknown UUID with `expected_resource_version=1` and await completion. Then, under a new key, submit batch deletion `[unknown UUID at 1, person_schema at 1, other_schema by GTS ID at 1]`.

**Then:** both submissions return `202` with operation receipts and `Location`, rather than synchronous `404`. The single operation's item fails `precondition_failed`. In the batch, the unknown UUID and absent GTS ID each fail `precondition_failed` with no resulting resource version, while `person_schema` succeeds at version 2. Results preserve request order; the UUID item echoes the UUID as its `entity_key`. `person_schema` is a readable tombstone, and both missing keys remain absent. Neither refusal creates an identity reservation: `unregistered_schema` then registers at version 1 and reads back by the formerly unknown UUID.


## Dependency graph and partial deletion

### TR-DEL-201 — A live Instance blocks deletion of its conforming schema

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [person_instance](../fixtures/deletion/person_instance.json).

**Given:** register `person_schema` and its conforming `person_instance`; both are active.

**When:** delete only `person_schema` with its current version.

**Then:** `person_schema` fails `has_registered_dependents`; the diagnostic reports one live direct dependant without identifying `person_instance`. Both entities remain unchanged.

### TR-DEL-202 — Delete an Instance before its schema despite request order

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [person_instance](../fixtures/deletion/person_instance.json).

**Given:** register `person_schema` and its conforming `person_instance`, both at version 1.

**When:** batch-delete `[person_schema, person_instance]`, each with expected version 1.

**Then:** both succeed at version 2. Outcomes are `[person_schema, person_instance]`; both read as tombstones.

### TR-DEL-203 — A derived schema blocks deletion of its base

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [derived_employee_schema](../fixtures/deletion/derived_employee_schema.json).

**Given:** register `person_schema` and `derived_by_id_schema`, a variant of `derived_employee_schema` that removes its `allOf` `$ref` and restates the base's `name` property, `required` list and closed root inline. Dropping only the `$ref` would widen the closed base, which GTS refuses as not included in its base; restated, the identifier's derivation is the variant's only edge. Register no Instances.

**When:** delete only `person_schema`.

**Then:** `person_schema` fails `has_registered_dependents` with one live direct dependant. `person_schema` and `derived_by_id_schema` remain active and unchanged: derivation alone is sufficient protection.

### TR-DEL-204 — A schema reference blocks deletion of its target

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [person_referrer_schema](../fixtures/deletion/person_referrer_schema.json).

**Given:** register `person_schema` and `person_referrer_schema`, whose `payload.person` contains a resolving `$ref` to `person_schema`.

**When:** delete only `person_schema`.

**Then:** `person_schema` fails `has_registered_dependents` with one live direct dependant; `person_referrer_schema` and `person_schema` remain unchanged.

### TR-DEL-205 — Delete a complete derivation and conformance chain

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [derived_employee_schema](../fixtures/deletion/derived_employee_schema.json), [employee_instance](../fixtures/deletion/employee_instance.json).

**Given:** register `person_schema`, `derived_employee_schema` and its conforming `employee_instance`. The chain is `employee_instance → derived_employee_schema → person_schema`; all are at version 1.

**When:** batch-delete `[person_schema, derived_employee_schema, employee_instance]` with correct versions.

**Then:** all succeed at version 2, with outcomes in request order. Reads of all three entities return retained content and deleted lifecycle.

### TR-DEL-206 — Delete every branch before their shared target

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [person_referrer_schema](../fixtures/deletion/person_referrer_schema.json).

**Given:** register `person_schema`, `person_referrer_schema` and `second_person_referrer`, a copy of `person_referrer_schema` with entity name `second_referrer`. Both referrers target `person_schema`.

**When:** batch-delete `[person_schema, person_referrer_schema, second_person_referrer]` with correct versions.

**Then:** all three succeed. `person_schema` is deleted only after both blocking entities have been removed; request-order outcomes still start with `person_schema`.

### TR-DEL-207 — Keep partial success when a dependant is outside the batch

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [person_referrer_schema](../fixtures/deletion/person_referrer_schema.json).

**Given:** register `person_schema`, `person_referrer_schema` and `second_person_referrer`, a copy of `person_referrer_schema` with entity name `second_referrer`. Both referrers target `person_schema`.

**When:** batch-delete `[person_schema, person_referrer_schema]`, leaving `second_person_referrer` outside the request.

**Then:** `person_referrer_schema` succeeds; `person_schema` fails `has_registered_dependents` reporting the one remaining live direct dependant. `person_referrer_schema` is a tombstone; `person_schema` and `second_person_referrer` remain unchanged. The failure of `person_schema` does not roll back `person_referrer_schema`'s deletion.

### TR-DEL-208 — A failed dependant deletion leaves its target blocked

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [person_referrer_schema](../fixtures/deletion/person_referrer_schema.json), [other_schema](../fixtures/deletion/other_schema.json).

**Given:** register `person_schema`, `person_referrer_schema`, `second_person_referrer` (a copy of `person_referrer_schema` with entity name `second_referrer`) and independent `other_schema`. Both referrers target `person_schema`. Revise only `second_person_referrer`, adding `content.title: "Revised Referrer"`, to reach version 2 while preserving its `$ref`.

**When:** batch-delete `[person_schema, person_referrer_schema, second_person_referrer, other_schema]`, using stale version 1 for `second_person_referrer` and correct versions for every other item.

**Then:** `second_person_referrer` fails `precondition_failed`; `person_referrer_schema` and `other_schema` succeed. `person_schema` fails `has_registered_dependents`, not `blocked_by_dependency`, because `second_person_referrer` is still live. `second_person_referrer` keeps its latest content and version; `person_schema` remains unchanged. Outcomes preserve `[person_schema, person_referrer_schema, second_person_referrer, other_schema]` even though execution order differs.

### TR-DEL-209 — Remove a blocker and retry deletion of its target

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [person_referrer_schema](../fixtures/deletion/person_referrer_schema.json).

**Given:** register `person_schema` and `person_referrer_schema`. An attempted deletion of `person_schema` has completed with `has_registered_dependents`.

**When:** successfully delete `person_referrer_schema`, then submit deletion of `person_schema` under a new key using `person_schema`'s original observed version.

**Then:** `person_schema` now succeeds. The refused request and `person_referrer_schema`'s deletion did not advance `person_schema`'s resource version. Both entities are readable tombstones.

### TR-DEL-210 — Only the current revision's references protect a target

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [person_referrer_schema](../fixtures/deletion/person_referrer_schema.json).

**Given:** register `person_schema`, `replacement_person_schema` (a copy of `person_schema` with entity name `replacement_person`) and `person_referrer_schema`. Complete a revision of `person_referrer_schema` changing only `content.properties.payload.properties.person.$ref` to the GTS URI of `replacement_person_schema`. The targets have equivalent definitions apart from identity.

**When:** batch-delete `[person_schema, replacement_person_schema]` with their current versions.

**Then:** `person_schema` succeeds; `replacement_person_schema` fails `has_registered_dependents`. `person_referrer_schema` remains active at its revised version referring to `replacement_person_schema`. `person_referrer_schema`'s retained historical reference to `person_schema` does not block deletion of `person_schema`.

### TR-DEL-211 — An x-gts-ref constraint does not protect its named entity

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [person_x_gts_ref_schema](../fixtures/deletion/person_x_gts_ref_schema.json).

**Given:** register `person_schema` and `person_x_gts_ref_schema`. The latter names `person_schema` through `x-gts-ref` on its string-valued `payload.person` property and has no `$ref`, derivation or conformance dependency on it.

**When:** delete `person_schema`.

**Then:** `person_schema` succeeds. `person_x_gts_ref_schema` stays active with unchanged content, effective documents, resource version and same-projection ETag. The constraint does not promise target availability or cause deletion-time refresh.

### TR-DEL-212 — Count direct dependants rather than the transitive closure

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [person_referrer_schema](../fixtures/deletion/person_referrer_schema.json).

**Given:** register `person_schema`, `person_referrer_schema` and `indirect_person_referrer`, a copy of `person_referrer_schema` with entity name `indirect_referrer` and its `payload.person.$ref` retargeted to `person_referrer_schema`. The chain is `indirect_person_referrer → person_referrer_schema → person_schema`, with no direct reference from the first to the last.

**When:** delete only `person_schema`.

**Then:** `person_schema` fails `has_registered_dependents` reporting one live direct dependant, not two. All three entities remain unchanged; the diagnostic contains no identifiers of dependent entities.

### TR-DEL-213 — Multiple dependency kinds do not multiply dependent entities

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [derived_employee_schema](../fixtures/deletion/derived_employee_schema.json).

**Given:** register `person_schema` and `derived_employee_schema`, whose identifier derives from `person_schema` and whose `allOf` `$ref`s it, so one entity holds two kinds of edge to the same target. No other entity depends on `person_schema`.

**When:** first delete `person_schema` alone, then submit a new batch `[person_schema, derived_employee_schema]` with correct versions.

**Then:** the first operation refuses `person_schema` and counts one dependent entity despite the two kinds of edge. The subsequent batch successfully deletes both entities.

## Request identity and idempotency

### TR-DEL-301 — Replay a successful deletion without changing its tombstone

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json).

**Given:** register `person_schema`, complete its deletion under key K, and read its tombstone.

**When:** resubmit the identical deletion with K and the original precondition.

**Then:** HTTP `200` returns the same operation ID and stored terminal outcomes. The tombstone's version, content, timestamps and ETag do not move again. This is request replay, not a new successful deletion of an already deleted entity.

### TR-DEL-302 — Replay a refused deletion even after its blocker disappears

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [person_referrer_schema](../fixtures/deletion/person_referrer_schema.json).

**Given:** register `person_schema` and `person_referrer_schema`. Deletion of `person_schema` under K fails because `person_referrer_schema` is live. Subsequently complete deletion of `person_referrer_schema`.

**When:** replay the original request with K, then submit it with a new key.

**Then:** replay returns HTTP `200` and the original refused operation, without re-evaluating `person_schema`. The new request succeeds using `person_schema`'s still-current original version.

### TR-DEL-303 — Changed targets or preconditions conflict with a used key

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [other_schema](../fixtures/deletion/other_schema.json).

**Given:** register `person_schema` and independent `other_schema`. Complete deletion of `person_schema` under K; `other_schema` remains active.

**When:** reuse K, separately changing the target to `other_schema` and changing `person_schema`'s expected version to its tombstone version.

**Then:** each request returns synchronous `409`. The original operation and both entities remain unchanged.

### TR-DEL-304 — Dry run and real deletion are different requests

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json).

**Given:** register `person_schema`, then complete a successful deletion dry run under K, leaving it active.

**When:** submit the same target and precondition with `dry_run=false` and K, then submit that real deletion with a new key.

**Then:** reuse of K returns `409` and leaves the target active. The new-key request performs the deletion successfully.

### TR-DEL-305 — Replay across deletion routes and equivalent entity keys

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json).

**Given:** register `person_schema`, read its GTS ID, UUID and resource version, then complete single-route deletion by GTS ID under K. Save the terminal operation and the tombstone's body and ETag.

**When:** replay under K through single DELETE by UUID, one-item `batchDelete` by GTS ID, and one-item `batchDelete` by UUID. Keep the original positive precondition and `dry_run=false` in every request. In a separate fresh setup, start with one-item `batchDelete` by UUID and replay through single DELETE by GTS ID.

**Then:** each replay returns HTTP `200` with a receipt carrying the original `operation_id`, `status=completed` and `replayed=true`, plus `Idempotency-Replayed: true` and the original `Location`, without `Retry-After`. Polling returns the original terminal operation and deletion result. Route and equivalent key spelling do not create a different request or cause `409`; the tombstone's version, content, timestamps and ETag remain unchanged. A new key would instead invoke a new deletion and fail `not_active`, as in TR-DEL-103.

## Dry-run deletion

### TR-DEL-401 — Predict a single deletion without changing entity state

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json).

**Given:** register isolated `person_schema`; capture its full selected read and ETag.

**When:** use single DELETE with the observed version and `dry_run=true`.

**Then:** the completed operation has `dry_run=true`; the item is `succeeded` with no resulting `resource_version`. The entity's read and ETag remain unchanged. A real request with a new key and the same version succeeds.

### TR-DEL-402 — Predict deletion of a complete graph using virtual changes

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [derived_employee_schema](../fixtures/deletion/derived_employee_schema.json), [employee_instance](../fixtures/deletion/employee_instance.json).

**Given:** register `person_schema`, `derived_employee_schema` and its conforming `employee_instance`; all are active. The chain is `employee_instance → derived_employee_schema → person_schema`.

**When:** dry-run batch `[person_schema, derived_employee_schema, employee_instance]` with correct versions.

**Then:** all items predict success in request-order results, without allocated versions. Virtual deletion of `employee_instance` and `derived_employee_schema` allows prediction for `person_schema`. All actual entities remain active and unchanged. A real batch under a new key yields the same success statuses and increments each entity's version once.

### TR-DEL-403 — Predict partial success without removing failed blockers

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [person_referrer_schema](../fixtures/deletion/person_referrer_schema.json), [other_schema](../fixtures/deletion/other_schema.json).

**Given:** register `person_schema`, `person_referrer_schema`, `second_person_referrer` (a copy of `person_referrer_schema` with entity name `second_referrer`) and independent `other_schema`. Both referrers target `person_schema`. Revise only `second_person_referrer` by adding `content.title: "Revised Referrer"`, reaching version 2. Capture all four entities; retain stale version 1 for its deletion request.

**When:** dry-run `[person_schema, person_referrer_schema, second_person_referrer, other_schema]`, then submit the same batch as a real request under a new key with no intervening entity mutations.

**Then:** the dry run predicts success for `person_referrer_schema` and `other_schema`, `precondition_failed` for `second_person_referrer`, and `has_registered_dependents` for `person_schema`. Every actual entity is unchanged after the dry run. Real deletion has identical statuses and reasons, commits `person_referrer_schema` and `other_schema`, and leaves `person_schema` and `second_person_referrer` unchanged. Successful dry-run items have no resulting version; real successes do.

### TR-DEL-404 — Predict a refusal caused by a dependant outside the batch

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [person_instance](../fixtures/deletion/person_instance.json).

**Given:** TR-DEL-201's `person_schema` and its conforming `person_instance`; one test runs both.

**When:** dry-run deletion of `person_schema` alone, then the real deletion.

**Then:** `person_schema` fails `has_registered_dependents` with one live direct dependant in both modes. `person_instance` and `person_schema` remain unchanged, and the dry run assumes no hypothetical removal of an unrequested entity.

### TR-DEL-405 — A passing dry run does not reserve deletion eligibility

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [person_referrer_schema](../fixtures/deletion/person_referrer_schema.json).

**Given:** register isolated `person_schema` and complete its successful deletion dry run, leaving it active at version 1. Load `person_referrer_schema` for later registration, but do not submit it yet.

**When:** register `person_referrer_schema` → `person_schema`, await its success, then actually delete `person_schema` using the same version 1 under a new key.

**Then:** real deletion fails `has_registered_dependents`. `person_schema`'s version did not change when `person_referrer_schema` was registered, yet the eligibility prediction became stale. Both entities remain active. The earlier dry-run result remains unchanged.

## Tombstones and subsequent use

### TR-DEL-501 — Retain full reads and invalidate the old ETag after deletion

**Fixtures:** [trait_base_schema](../fixtures/deletion/trait_base_schema.json), [trait_derived_schema](../fixtures/deletion/trait_derived_schema.json).

**Given:** register `trait_base_schema` and `trait_derived_schema`; register no dependants of the derived schema. The base declares and supplies trait `category=internal`, and the derived schema adds required `payload.employee_id`. Read `trait_derived_schema` by GTS ID and UUID, selecting `content,origin,resolved_schema,effective_traits,effective_traits_schema`; retain each ETag. The expected documents are computed from the two fixtures: the base inlined without identity and trait keywords, its traits, and its traits schema. TR-READ-007 covers the conditional-read rule itself; this scenario adds the effective documents and the UUID key.

**When:** delete `trait_derived_schema`, then conditionally read each key with its corresponding old ETag.

**Then:** each read returns `200` and a tombstone with a changed ETag. UUID, content and effective documents match the pre-deletion values. Repeating each read with its new ETag returns bodyless `304`. `trait_derived_schema`'s base remains active.

### TR-DEL-502 — Follow deletion through discovery and exact reads

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [other_schema](../fixtures/deletion/other_schema.json).

**Given:** register isolated `person_schema` and `other_schema` in one scenario namespace; both appear in default discovery.

**When:** delete `person_schema`, then walk discovery for that namespace with default lifecycle, explicit `active`, `deleted` and `all` filters.

**Then:** default and `active` return only `other_schema`; `deleted` returns only `person_schema`; `all` returns both. Exact and batch reads using `person_schema`'s previously issued keys still return its tombstone.

### TR-DEL-503 — A readable tombstone cannot become a new live dependency

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [person_instance](../fixtures/deletion/person_instance.json), [derived_employee_schema](../fixtures/deletion/derived_employee_schema.json), [person_referrer_schema](../fixtures/deletion/person_referrer_schema.json), [other_schema](../fixtures/deletion/other_schema.json).

**Given:** register and then delete `person_schema`; confirm that its definition remains readable. Load but do not register `person_instance`, `derived_employee_schema`, `person_referrer_schema` and independent `other_schema`.

**When:** submit `person_instance`, `derived_employee_schema`, `person_referrer_schema` and `other_schema` together as new registration candidates.

**Then:** each `person_schema`-dependent candidate fails `dependency_deleted` with `dependency_id` naming `person_schema` and remains absent: the Instance with `dependency_kind=conforming_type`, the derived schema with `base` (its derivation role wins over its `allOf` `$ref`), and the referrer with `ref`. The independent candidate succeeds. `person_schema` stays deleted and unchanged. A deleted predecessor still serves as a cross-minor baseline (TR-DEL-603, TR-DEL-604), which is no edge.

## Version-family lifecycle

### TR-DEL-601 — Delete major members independently of version succession

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json).

**Given:** register `person_schema` and `person_major_2_schema`, a copy whose last-segment version is v2. Both are active members of one family without dependencies.

**When:** in separate fresh setups, delete the older member and delete the newer member.

**Then:** either deletion succeeds while the other member stays active and unchanged. Deleting the highest major requires no successor, and deleting the older one causes no transition of the newer one.

### TR-DEL-602 — Delete a middle minor while a higher minor remains active

**Fixtures:** [person_minor_0_schema](../fixtures/deletion/person_minor_0_schema.json), [person_minor_1_schema](../fixtures/deletion/person_minor_1_schema.json), [person_minor_2_schema](../fixtures/deletion/person_minor_2_schema.json).

**Given:** register `person_minor_0_schema`, `person_minor_1_schema` and `person_minor_2_schema` in ascending minor order. They have no explicit dependencies or registered dependants.

**When:** delete `person_minor_1_schema`.

**Then:** `person_minor_1_schema` becomes a tombstone; `person_minor_0_schema` and `person_minor_2_schema` remain active and unchanged. Predecessor ordering at registration creates no deletion blocker.

### TR-DEL-603 — Admit the next minor after deleting its predecessor

**Fixtures:** [person_minor_0_schema](../fixtures/deletion/person_minor_0_schema.json), [person_minor_1_schema](../fixtures/deletion/person_minor_1_schema.json), [person_minor_2_schema](../fixtures/deletion/person_minor_2_schema.json).

**Given:** register `person_minor_0_schema` and `person_minor_1_schema`, then successfully delete `person_minor_1_schema`. Prepare `person_minor_2_schema` for registration.

**When:** register compatible `person_minor_2_schema` under a new key.

**Then:** `person_minor_2_schema` succeeds. Retained `person_minor_1_schema` satisfies contiguity despite its deleted lifecycle, and `person_minor_1_schema` remains deleted at the same version.

### TR-DEL-604 — Compare a new minor against the deleted predecessor

**Fixtures:** [person_minor_0_schema](../fixtures/deletion/person_minor_0_schema.json), [person_minor_1_schema](../fixtures/deletion/person_minor_1_schema.json), [person_minor_2_incompatible_schema](../fixtures/deletion/person_minor_2_incompatible_schema.json).

**Given:** register `person_minor_0_schema` and `person_minor_1_schema`. The latter adds optional root field `extra` while retaining open `payload`. Delete `person_minor_1_schema`. Prepare `person_minor_2_incompatible_schema`, which removes `extra` and otherwise returns to the v1.0 definition apart from its ID.

**When:** register `person_minor_2_incompatible_schema` without `force`.

**Then:** registration fails compatibility: `person_minor_1_schema` accepted objects containing `extra`, and the proposed closed `person_minor_2_incompatible_schema` rejects them. The registry must use retained `person_minor_1_schema` as baseline, not skip it in favor of `person_minor_0_schema`. `person_minor_2_incompatible_schema` remains absent; `person_minor_1_schema` stays deleted.

### TR-DEL-605 — A tombstone still fixes its major's shape

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [person_minor_0_schema](../fixtures/deletion/person_minor_0_schema.json).

**Given:** register `person_schema` as the family's only member, then delete it. Prepare `person_minor_0_schema` for creation. Recreating the tombstoned name itself is TR-REG-305.

**When:** attempt to create `person_minor_0_schema`.

**Then:** it fails `family_shape_conflict` and remains absent. The retained tombstone still establishes the major's major-only shape; a family with no active member is not an empty registry family.

### TR-DEL-606 — Major zero keeps ordinary deletion safety

**Fixtures:** [person_schema](../fixtures/deletion/person_schema.json), [derived_employee_schema](../fixtures/deletion/derived_employee_schema.json).

**Given:** prepare `person_major_zero_schema` from `person_schema` by changing its last-segment version to v0. Prepare `derived_employee_major_zero_schema` from `derived_employee_schema` by changing both its base segment and its own final segment to v0 and retargeting its base `$ref`. Register both successfully.

**When:** delete `person_major_zero_schema` alone, then submit a new batch `[person_major_zero_schema, derived_employee_major_zero_schema]` with correct versions.

**Then:** the first deletion fails `has_registered_dependents` and both entities stay unchanged; the complete batch succeeds and leaves readable tombstones. The unstable compatibility profile does not waive dependency safety.
