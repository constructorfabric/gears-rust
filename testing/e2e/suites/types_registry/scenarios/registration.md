# Entity registration scenarios

End-to-end registration scenarios cover HTTP submission, outbox admission, persisted outcomes and reads. The local launcher uses **SQLite**; these tests do not prove PostgreSQL/MySQL or tenant/PDP behavior.

Every scenario has an e2e test; the [suite README](../README.md) lists the test file of each group. Scenarios proved by one workflow share a test: TR-REG-207 with TR-REG-701, and TR-REG-706 with TR-REG-405. Each scenario lists its source fixtures before Given and refers to them by name; revisions and identifier variants are described in the scenario. Every scenario uses a fresh namespace.

Unless stated otherwise, each mutation uses a fresh `Idempotency-Key`. Accepted requests return `202` with a JSON receipt and a `Location` for `GET /operations/{operation_id}`; polling reaches `completed`, and the items report success or failure. Synchronous refusals return Problem JSON without an operation `Location` and admit no item. Reads explicitly select the compared fields.

Valid root Type Schema fixtures follow the GTS §4.4.1 closed-envelope pattern: the top level rejects undeclared properties and `payload` is the open extension point. The intentionally unresolved derived schema inherits no root constraints and is not closed locally. Instances of object schemas always carry `payload`, even when empty. The `label_schema` fixture is the deliberate exception: a root Type Schema that describes strings rather than objects, so its Instances have string `content`.

## Contents

- [Successful registration](#successful-registration)
  - [TR-REG-001 — Create a Type Schema](#tr-reg-001--create-a-type-schema)
  - [TR-REG-002 — Register an Instance in a later operation](#tr-reg-002--register-an-instance-in-a-later-operation)
  - [TR-REG-003 — Register an Instance before its schema in one batch](#tr-reg-003--register-an-instance-before-its-schema-in-one-batch)
  - [TR-REG-004 — Register a scalar Type Schema and a string Instance](#tr-reg-004--register-a-scalar-type-schema-and-a-string-instance)
- [Partial success and refusals](#partial-success-and-refusals)
  - [TR-REG-101 — Preserve partial success and structured failures](#tr-reg-101--preserve-partial-success-and-structured-failures)
  - [TR-REG-102 — Refuse a Type Schema whose `$id` does not name its item](#tr-reg-102--refuse-a-type-schema-whose-id-does-not-name-its-item)
  - [TR-REG-103 — Invalid Instance content fails against a scalar Type Schema](#tr-reg-103--invalid-instance-content-fails-against-a-scalar-type-schema)
- [Request identity and idempotency](#request-identity-and-idempotency)
  - [TR-REG-201 — Replay returns the original operation after the entity changes](#tr-reg-201--replay-returns-the-original-operation-after-the-entity-changes)
  - [TR-REG-202 — Reusing a key for a different request is a conflict](#tr-reg-202--reusing-a-key-for-a-different-request-is-a-conflict)
  - [TR-REG-203 — A new key does not turn duplicate creation into replay](#tr-reg-203--a-new-key-does-not-turn-duplicate-creation-into-replay)
  - [TR-REG-204 — An unknown operation is a 404 naming an operation](#tr-reg-204--an-unknown-operation-is-a-404-naming-an-operation)
  - [TR-REG-205 — Replay a refused registration after its dependency arrives](#tr-reg-205--replay-a-refused-registration-after-its-dependency-arrives)
  - [TR-REG-206 — Changing only the precondition conflicts with a used key](#tr-reg-206--changing-only-the-precondition-conflicts-with-a-used-key)
  - [TR-REG-207 — Dry run and real registration require different keys](#tr-reg-207--dry-run-and-real-registration-require-different-keys)
- [Content revisions and optimistic preconditions](#content-revisions-and-optimistic-preconditions)
  - [TR-REG-301 — Add an optional property at a closed schema level](#tr-reg-301--add-an-optional-property-at-a-closed-schema-level)
  - [TR-REG-302 — Revise an Instance value without revising its Type Schema](#tr-reg-302--revise-an-instance-value-without-revising-its-type-schema)
  - [TR-REG-303 — Equal current Instance content is unchanged](#tr-reg-303--equal-current-instance-content-is-unchanged)
  - [TR-REG-304 — A stale writer reads again and retries](#tr-reg-304--a-stale-writer-reads-again-and-retries)
  - [TR-REG-305 — Registration cannot reuse a tombstoned name](#tr-reg-305--registration-cannot-reuse-a-tombstoned-name)
- [Dependency graph and partial admission](#dependency-graph-and-partial-admission)
  - [TR-REG-401 — Register a derived Type Schema before its base](#tr-reg-401--register-a-derived-type-schema-before-its-base)
  - [TR-REG-402 — Resolve an in-batch `$ref` target submitted later](#tr-reg-402--resolve-an-in-batch-ref-target-submitted-later)
  - [TR-REG-403 — Reconcile an Instance after its missing schema arrives](#tr-reg-403--reconcile-an-instance-after-its-missing-schema-arrives)
  - [TR-REG-404 — Invalid Instance content does not block a valid batch neighbour](#tr-reg-404--invalid-instance-content-does-not-block-a-valid-batch-neighbour)
  - [TR-REG-405 — An in-batch failed revision blocks its new referrer](#tr-reg-405--an-in-batch-failed-revision-blocks-its-new-referrer)
  - [TR-REG-406 — A cycle fails beside an independent success](#tr-reg-406--a-cycle-fails-beside-an-independent-success)
  - [TR-REG-407 — Register a complete derivation and conformance chain in reverse order](#tr-reg-407--register-a-complete-derivation-and-conformance-chain-in-reverse-order)
  - [TR-REG-408 — A failed branch does not block its sibling under a shared base](#tr-reg-408--a-failed-branch-does-not-block-its-sibling-under-a-shared-base)
- [Compatibility and dependent safety](#compatibility-and-dependent-safety)
  - [TR-REG-501 — Adding a property at an open level is incompatible](#tr-reg-501--adding-a-property-at-an-open-level-is-incompatible)
  - [TR-REG-502 — A live direct Instance prevents an abstract transition](#tr-reg-502--a-live-direct-instance-prevents-an-abstract-transition)
  - [TR-REG-503 — A live derived Type Schema prevents a final transition](#tr-reg-503--a-live-derived-type-schema-prevents-a-final-transition)
  - [TR-REG-504 — Publish a breaking contract under a new major](#tr-reg-504--publish-a-breaking-contract-under-a-new-major)
  - [TR-REG-505 — A revision refreshes transitive schema references](#tr-reg-505--a-revision-refreshes-transitive-schema-references)
- [Minor versions and version-family shape](#minor-versions-and-version-family-shape)
  - [TR-REG-601 — Order contiguous minors within one batch](#tr-reg-601--order-contiguous-minors-within-one-batch)
  - [TR-REG-602 — Refuse a minor with a missing predecessor](#tr-reg-602--refuse-a-minor-with-a-missing-predecessor)
  - [TR-REG-603 — One major cannot mix major-only and minor-bearing shapes](#tr-reg-603--one-major-cannot-mix-major-only-and-minor-bearing-shapes)
  - [TR-REG-604 — Refuse a content revision of a minor-bearing schema](#tr-reg-604--refuse-a-content-revision-of-a-minor-bearing-schema)
  - [TR-REG-605 — A failed predecessor blocks the next minor](#tr-reg-605--a-failed-predecessor-blocks-the-next-minor)
  - [TR-REG-606 — Admit a compatible minor with changed content without force](#tr-reg-606--admit-a-compatible-minor-with-changed-content-without-force)
  - [TR-REG-607 — Refuse an incompatible minor without force](#tr-reg-607--refuse-an-incompatible-minor-without-force)
- [Dry-run admission](#dry-run-admission)
  - [TR-REG-701 — Predict creation, then commit under a new key](#tr-reg-701--predict-creation-then-commit-under-a-new-key)
  - [TR-REG-702 — A mixed dry run predicts the committed item verdicts](#tr-reg-702--a-mixed-dry-run-predicts-the-committed-item-verdicts)
  - [TR-REG-703 — Predict a revision beside an unchanged schema](#tr-reg-703--predict-a-revision-beside-an-unchanged-schema)
  - [TR-REG-704 — An Instance sees its schema's virtual revision](#tr-reg-704--an-instance-sees-its-schemas-virtual-revision)
  - [TR-REG-705 — A passing dry run does not reserve the observed resource version](#tr-reg-705--a-passing-dry-run-does-not-reserve-the-observed-resource-version)
  - [TR-REG-706 — A failed virtual revision blocks its selected referrer](#tr-reg-706--a-failed-virtual-revision-blocks-its-selected-referrer)
- [Managed schema dialect and unstable major-zero profile](#managed-schema-dialect-and-unstable-major-zero-profile)
  - [TR-REG-801 — Refuse an inadmissible schema dialect before admission](#tr-reg-801--refuse-an-inadmissible-schema-dialect-before-admission)
  - [TR-REG-802 — Major zero allows breaking revisions but no registered Instance](#tr-reg-802--major-zero-allows-breaking-revisions-but-no-registered-instance)
  - [TR-REG-803 — Stable schemas cannot derive from or `$ref` major zero](#tr-reg-803--stable-schemas-cannot-derive-from-or-ref-major-zero)
- [Deployment policy and compatibility waiver](#deployment-policy-and-compatibility-waiver)
  - [TR-REG-901 — A region outside the allowlist refuses a non-platform vendor](#tr-reg-901--a-region-outside-the-allowlist-refuses-a-non-platform-vendor)
  - [TR-REG-902 — Disabled `force` is a synchronous refusal](#tr-reg-902--disabled-force-is-a-synchronous-refusal)
  - [TR-REG-903 — Enabled `force` records a waived minor transition](#tr-reg-903--enabled-force-records-a-waived-minor-transition)
  - [TR-REG-904 — A configured region admits its allowed vendor](#tr-reg-904--a-configured-region-admits-its-allowed-vendor)

## Successful registration

### TR-REG-001 — Create a Type Schema

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json).

**Given:** a new dependency-free `person_schema`.

**When:** submit this item alone and await completion.

**Then:**

1. The item succeeds at resource version 1.
2. Reading by GTS ID returns the submitted Type Schema and materialized traits.
3. Reading by `gts_uuid` returns the same body and timestamps.

### TR-REG-002 — Register an Instance in a later operation

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json), [person_instance](../fixtures/registration/person_instance.json).

**Given:** a `person_schema` and conforming `person_instance`.

**When:**

1. Submit and complete the schema operation.
2. Submit and complete the Instance under a new key.

**Then:** the Instance succeeds at version 1 and reads back with its exact content; schema-only artifacts are absent, not `null`.

### TR-REG-003 — Register an Instance before its schema in one batch

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json), [person_instance](../fixtures/registration/person_instance.json).

**Given:** `person_schema` and its conforming `person_instance` are both absent from a fresh namespace.

**When:** submit `person_instance` before `person_schema` in one batch.

**Then:** both succeed at version 1 and read back with the expected kind and content.

This checks batch-level ordering, not every graph-ordering case.

### TR-REG-004 — Register a scalar Type Schema and a string Instance

**Fixtures:** [label_schema](../fixtures/registration/label_schema.json), [label_instance](../fixtures/registration/label_instance.json).

**Given:** a new root `label_schema` whose content defines `type: string` with `minLength: 1`, and its `label_instance` whose `content` is the JSON string `"primary"`. Both are absent.

**When:** submit `[instance, schema]` in one batch and await completion.

**Then:** both succeed at version 1. The schema reads back with a `resolved_schema` equal to its authored schema document. The Instance reads back with `content` equal to the JSON string `"primary"`: it is neither wrapped in an object nor re-encoded as a string.

## Partial success and refusals

### TR-REG-101 — Preserve partial success and structured failures

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json), [missing_ref_schema](../fixtures/registration/missing_ref_schema.json), [blocked_instance](../fixtures/registration/blocked_instance.json).

**Given:**

- A: valid, independent `person_schema`.
- B: `missing_ref_schema`, referencing absent `absent.v1~`.
- C: `blocked_instance`, an Instance conforming to B.

**When:** submit exactly `[C, B, A]` in one request and await completion.

**Then:**

| Item | Status | Resource version | Error reason |
|---|---|---|---|
| A | succeeded | 1 | No error |
| B | failed | null | dependency_not_found |
| C | failed | null | blocked_by_dependency |

B also reports `dependency_kind=ref` and the missing ID. A is readable; B and C return RFC-9457 `404` responses.

### TR-REG-102 — Refuse a Type Schema whose `$id` does not name its item

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json), [person_instance](../fixtures/registration/person_instance.json).

**Given:** the `person_schema` and its conforming `person_instance`, both absent, with the schema's `content.$id` replaced by one of: absent, a non-string, a malformed URI, or the `gts://` URI of another Type Schema.

**When:** submit `person_instance` before `person_schema` in one request.

**Then:**

1. The request is refused synchronously with `400` RFC-9457 `invalid_argument` naming the schema's `gts_id` as `resource_name`, with one field violation on `entity`, reason `VALIDATION_FAILED`, whose description names the expected `gts://<gts_id>`.
2. No `202`, operation or `Location` is returned, so nothing is admitted.
3. Neither the schema nor its valid batch neighbour, the Instance, is readable; both return `404`.

### TR-REG-103 — Invalid Instance content fails against a scalar Type Schema

**Fixtures:** [label_schema](../fixtures/registration/label_schema.json), [label_instance](../fixtures/registration/label_instance.json).

**Given:** the `label_schema` is registered. Prepare a valid neighbour from `label_instance` with entity name `secondary` and content `"secondary"`, and six new Instances of the same type whose `content` is `{}`, `42`, `true`, `["primary"]`, `null` or `""`. The `null` is a present JSON value, not missing content. The empty string has the right JSON type but violates `minLength`.

**When:** submit all seven Instances in one batch and await completion.

**Then:** acceptance returns `202`; content that does not match the schema is not a synchronous refusal. The valid neighbour succeeds at version 1 and reads back with its string content. Each of the six others is `failed` with `error.reason=invalid_value` and `resource_version=null`, and its key returns `404`. The operation is `completed`, and one failure does not block the other items.

## Request identity and idempotency

### TR-REG-201 — Replay returns the original operation after the entity changes

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json).

**Given:** the `person_schema` was created under key K1. Save its `operation_id`, terminal operation body and receipt `Location`. Under K2, revise only its `title` with `expected_resource_version=1`, and confirm by exact read that it is now at `resource_version` 2 with that title.

**When:** send the byte-for-byte same creation request again under K1, then follow the returned `Location`.

**Then:**

1. The replay responds with `200` and a receipt, not an inline operation body. It has the original `operation_id`, `status=completed` and `replayed=true`; `Idempotency-Replayed: true` and `Location` are present, while `Retry-After` is absent.
2. Polling returns the original completed operation and its version-1 creation result, even though the entity is now at version 2.
3. Reading the entity still returns the revised version-2 content. Replay creates no new revision.

### TR-REG-202 — Reusing a key for a different request is a conflict

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json).

**Given:** K1 completed a successful creation of the `person_schema` at version 1. Prepare a request for the same ID whose authored content differs only by a `title`; `$id` and `$schema` stay valid, so the request passes static validation and reaches the key check.

**When:** submit that different request under K1.

**Then:** the response is synchronous `409` RFC-9457 `already_exists`, naming the original operation as the conflicting resource: `resource_type` is the operation type `gts.cf.core.types_registry.operation.v1~` and `resource_name` its `operation_id`. It has no new operation `Location`. Polling the original ID still yields the original outcome, and reading the schema still returns its version-1 content.

### TR-REG-203 — A new key does not turn duplicate creation into replay

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json).

**Given:** the `person_schema` was created at version 1 under K1.

**When:** submit the identical creation candidate without `expected_resource_version` under a new key K2 and await completion.

**Then:** K2 receives its own `202` receipt and completed operation, but its item has `status=failed`, `resource_version=null` and `error.reason=already_exists`. The existing schema remains at version 1 with the same content and timestamps. This differs from both K1 replay (TR-REG-201) and an update with a matching precondition (TR-REG-303).

### TR-REG-204 — An unknown operation is a 404 naming an operation

**Fixtures:** None.

**Given:** a random operation UUID that no submission returned.

**When:** poll `GET /operations/{operation_id}`.

**Then:** the response is `404` RFC-9457 `not_found` whose `resource_type` is the operation type `gts.cf.core.types_registry.operation.v1~` and whose `resource_name` is the requested UUID. It does not claim that an entity is missing.

### TR-REG-205 — Replay a refused registration after its dependency arrives

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json), [person_referrer_schema](../fixtures/registration/person_referrer_schema.json).

**Given:** submit `person_referrer_schema` alone under K1 while `person_schema` is absent. Await `dependency_not_found`, with `dependency_kind=ref` and the target's ID, and retain the terminal operation. Then register `person_schema` under K2 and await success at version 1.

**When:** replay the identical referrer request under K1, poll its returned `Location`, and confirm that the referrer is still absent. Submit that same candidate under a new key K3 and await completion.

**Then:** replay returns a `200` receipt with the original operation ID, `replayed=true`, `Idempotency-Replayed: true` and the original `Location`, without `Retry-After`. Polling still returns K1's original failed outcome. K3 receives a new `202` receipt and succeeds at version 1; the referrer reads back with its authored `$ref` and a resolved schema containing `person_schema`'s constraints. The target remains unchanged at version 1.

### TR-REG-206 — Changing only the precondition conflicts with a used key

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json).

**Given:** register `person_schema` at version 1, then complete a revision adding `title: "Current Person"` at version 2. Prepare a candidate retaining that title and adding `description: "Updated description"`. Submit it under K1 with stale `expected_resource_version=1` and await `precondition_failed`; save the failed operation and the current version-2 entity.

**When:** resubmit the identical candidate content under K1, changing only `expected_resource_version` to 2. After the conflict, submit that corrected request under K2.

**Then:** the K1 reuse returns synchronous `409` RFC-9457 `already_exists`, naming K1's operation as the conflicting resource and returning no new operation `Location`. The saved failed operation and version-2 entity are unchanged. K2 is accepted and succeeds at version 3; exact read returns both the retained title and new description under the original identity.

### TR-REG-207 — Dry run and real registration require different keys

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json), [person_instance](../fixtures/registration/person_instance.json).

**Given:** both fixture IDs are absent. Submit `[person_instance, person_schema]` with `dry_run=true` under K1; await two successful predictions with `resource_version=null` and confirm that neither entity is readable.

**When:** submit the same candidates under K1 with only `dry_run` changed to false. After the conflict, submit the real request under K2.

**Then:** K1 reuse returns synchronous `409` naming the original dry-run operation, with no new operation `Location`. Both IDs remain absent and polling K1 still returns the original prediction. K2 receives a new `202` receipt and both items succeed at version 1; their exact reads return the expected schema and Instance content.


## Content revisions and optimistic preconditions

### TR-REG-301 — Add an optional property at a closed schema level

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json).

**Given:** the `person_schema` is active at version 1. Its root rejects undeclared properties. Prepare the same authored schema with an optional string `nickname` property at that root; leave existing constraints intact.

**When:** submit the revised schema with `expected_resource_version=1` and await completion.

**Then:** the item `succeeded` at resource version 2. Exact read returns the new authored property; GTS ID, deterministic `gts_uuid`, lifecycle and `created_at` remain the same. This is the compatible side of TR-REG-501.

### TR-REG-302 — Revise an Instance value without revising its Type Schema

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json), [person_instance](../fixtures/registration/person_instance.json).

**Given:** the `person_schema` and `person_instance` are registered at version 1. Prepare the same Instance ID with `name` changed from `Alice` to `Alicia`.

**When:** submit the Instance with `expected_resource_version=1` and await completion.

**Then:** the Instance item `succeeded` at version 2 and exact read returns `Alicia` under the same GTS ID and Registry Reference. The Type Schema remains at version 1 with unchanged authored content.

### TR-REG-303 — Equal current Instance content is unchanged

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json), [person_instance](../fixtures/registration/person_instance.json).

**Given:** the `person_schema` and `person_instance` are registered. Read the Instance and keep its version-1 content and `origin` timestamps for comparison after the operation.

**When:** submit the same Instance ID and authored content under a new key with `expected_resource_version=1`.

**Then:** the operation item is `unchanged` with `resource_version=1` and no error. Exact read returns the same content, version and `updated_at`. The new operation does not create a content revision.

### TR-REG-304 — A stale writer reads again and retries

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json).

**Given:** two callers A and B read the `person_schema` at version 1. B prepares a `title` revision; A prepares a `description` revision.

**When:**

1. B submits with `expected_resource_version=1` and completes at version 2.
2. A submits its `description` change with the older version-1 precondition under another key. After the refusal A reads again and rebuilds its change on the current document, keeping B's `title`, then resubmits under a third key with `expected_resource_version=2`.

**Then:** A's stale request is accepted with `202` but its item finishes `failed`, `resource_version=null` and `error.reason=precondition_failed`; there is no HTTP `412` and B's content remains current. A's reconciled request succeeds at version 3; exact read returns both B's `title` and A's `description`.

### TR-REG-305 — Registration cannot reuse a tombstoned name

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json).

**Given:** register an isolated `person_schema`, then complete its deletion with `expected_resource_version=1`. Exact read returns its tombstone at version 2.

**When:** under separate new keys, submit the same ID once as a creation (without `expected_resource_version`) and once as a content revision with `expected_resource_version=2`.

**Then:** both requests receive `202` and terminal registration operations. Creation fails with `already_exists`; revision fails with `entity_deleted`. Neither changes the readable version-2 tombstone, its content or its Registry Reference.

## Dependency graph and partial admission

### TR-REG-401 — Register a derived Type Schema before its base

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json), [derived_employee_schema](../fixtures/registration/derived_employee_schema.json).

**Given:** a new `person_schema` with a closed root and open `payload` extension point, plus a valid `derived_employee_schema` whose GTS ID names that base and whose own constraint requires `payload.employee_id`. Neither is registered.

**When:** submit `[derived, base]` in one batch and await completion.

**Then:** both items `succeeded` at version 1. Exact reads return each authored document; the derived read's `resolved_schema` includes the base's constraints and the derived `employee_id` constraint. Request order did not require a separate prerequisite operation.

### TR-REG-402 — Resolve an in-batch `$ref` target submitted later

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json), [person_referrer_schema](../fixtures/registration/person_referrer_schema.json).

**Given:** two new, independently named Type Schemas: a valid `person_schema` and a `person_referrer_schema` whose authored `$ref` names that target. The target is absent before the request.

**When:** submit `[referrer, target]` in one batch.

**Then:** both items `succeeded` at version 1. Reading the referrer's `resolved_schema` shows the target's constraints in place of the `$ref`. Each authored document reads back unchanged, including the referrer's `$ref` and both `$id` values.

### TR-REG-403 — Reconcile an Instance after its missing schema arrives

**Fixtures:** [person_instance](../fixtures/registration/person_instance.json), [person_schema](../fixtures/registration/person_schema.json).

**Given:** the `person_instance` and its `person_schema` are both absent.

**When:**

1. Submit the Instance alone under K1 and await its failure.
2. Submit and complete the schema under K2.
3. Submit the same Instance under K3 and await completion.

**Then:** K1's item is `failed` with `dependency_not_found`, `dependency_kind=conforming_type` and `dependency_id` equal to the schema ID; the Instance is initially absent. K2 and K3 each `succeeded` at version
1. The Instance is readable after K3, while polling K1 still reports its original failure. Separate operations require the caller to await the schema before retrying the Instance.

### TR-REG-404 — Invalid Instance content does not block a valid batch neighbour

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json).

**Given:** the `person_schema` is registered. Prepare two new Instances of that type: one with valid `name` and `payload`, and one missing the required `name`.

**When:** submit both Instances in one batch and await completion.

**Then:** the valid item `succeeded` at version 1 and reads back with its content. The invalid item is `failed` with `error.reason=invalid_value` and `resource_version=null`; its key returns `404`. The operation as a whole is `completed` even though an item failed.

### TR-REG-405 — An in-batch failed revision blocks its new referrer

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json), [person_referrer_schema](../fixtures/registration/person_referrer_schema.json).

**Given:** a registered `person_schema` at version 1. Prepare an incompatible revision that removes a property accepted by its current closed schema, plus the new `person_referrer_schema` whose `$ref` names that base ID.

**When:** submit `[referrer, base revision]` in one batch with the base's `expected_resource_version=1`. The shared test with TR-REG-706 adds its independent schema to the batch.

**Then:** the base revision is `failed` with `incompatible_with_baseline`; the referrer is `failed` with `blocked_by_dependency`. The stored version-1 base remains readable and unchanged, but the referrer does not fall back to that stored revision and remains absent.

### TR-REG-406 — A cycle fails beside an independent success

**Fixtures:** [cycle_a_schema](../fixtures/registration/cycle_a_schema.json), [cycle_b_schema](../fixtures/registration/cycle_b_schema.json), [person_schema](../fixtures/registration/person_schema.json).

**Given:** new schemas `cycle_a_schema` and `cycle_b_schema` that `$ref` each other, forming one cycle, and the independent `person_schema`. All three IDs are absent. A cycle of derivation edges alone is impossible: a base ID is always a proper prefix of its derived ID.

**When:** submit C, B and A in one batch and await completion.

**Then:** A and B each finish `failed` with `invalid_schema` and no resource version; both remain absent. C `succeeded` at version 1 and is readable. `completed` on the operation does not imply that every item succeeded.

### TR-REG-407 — Register a complete derivation and conformance chain in reverse order

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json), [derived_employee_schema](../fixtures/registration/derived_employee_schema.json), [person_instance](../fixtures/registration/person_instance.json).

**Given:** all entities are new. Prepare `employee_instance` from `person_instance`: replace its conforming-schema prefix with the complete ID of `derived_employee_schema`, retain the final `alice.v1` segment and `name: "Alice"`, and set `payload` to `{"employee_id": "E101"}`. The dependency chain is `employee_instance → derived_employee_schema → person_schema`.

**When:** submit `[employee_instance, derived_employee_schema, person_schema]` in one batch and await completion.

**Then:** all three items succeed at version 1. Exact reads return each entity's expected identity and authored content. The derived `resolved_schema` includes the base's required `name`, closed root and open payload extension point together with the derived `payload.employee_id` constraint. The Instance conforms to the full derived identifier and retains its `employee_id`. Match outcomes by GTS ID; registration result order is not the claim.

### TR-REG-408 — A failed branch does not block its sibling under a shared base

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json), [derived_employee_schema](../fixtures/registration/derived_employee_schema.json), [person_instance](../fixtures/registration/person_instance.json).

**Given:** prepare two new derived schemas sharing `person_schema`: the unchanged `derived_employee_schema`, and `broken_employee_schema`, a copy with final entity name `broken_employee`, a matching `$id`, and an additional optional `payload.manager` property whose `$ref` names `missing_manager`: `person_schema`'s identifier with entity name `missing_manager`, which is never registered. Preserve its valid base reference. Prepare `employee_instance` and `broken_employee_instance` from `person_instance`, replacing each conforming-schema prefix with its branch's complete schema ID and setting each payload to `{"employee_id": "E101"}`. No entity is registered.

**When:** submit `[broken_employee_instance, employee_instance, broken_employee_schema, derived_employee_schema, person_schema]` in one batch.

**Then:** `person_schema`, `derived_employee_schema` and `employee_instance` succeed at version 1 and read back with their expected content. `broken_employee_schema` fails `dependency_not_found`, naming the missing `$ref` target; `broken_employee_instance` fails `blocked_by_dependency`. Both failures have `resource_version=null` and both IDs remain absent. The common base commits once and the successful branch is not rolled back.


## Compatibility and dependent safety

### TR-REG-501 — Adding a property at an open level is incompatible

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json).

**Given:** a registered Type Schema at version 1 whose root is closed but whose `payload` level is open with `additionalProperties: true`. Prepare a revision that adds an optional, typed `payload.employee_id` property while leaving the rest of the schema unchanged.

**When:** submit the revision with `expected_resource_version=1`.

**Then:** its item fails with `incompatible_with_baseline` and `resource_version=null`. Exact read still returns the version-1 authored schema and origin. At an open level, the old schema accepted values under that property name which the new type constraint would reject; compare with the successful closed-level addition in TR-REG-301.

### TR-REG-502 — A live direct Instance prevents an abstract transition

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json), [person_instance](../fixtures/registration/person_instance.json).

**Given:** a concrete `person_schema` with `x-gts-abstract=false` and one active, directly conforming `person_instance`. Retain the schema's version-1 body and the Instance's body.

**When:** revise only `x-gts-abstract` to `true` under `expected_resource_version=1`.

**Then:** the schema item fails with `dependent_invalid` and no resource version. The schema remains concrete at version 1; the Instance remains active and unchanged.

### TR-REG-503 — A live derived Type Schema prevents a final transition

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json), [derived_employee_schema](../fixtures/registration/derived_employee_schema.json).

**Given:** a `person_schema` with `x-gts-final=false` and an active `derived_employee_schema` whose identifier names it. Retain both version-1 bodies.

**When:** revise the base to `x-gts-final=true` with `expected_resource_version=1`.

**Then:** the base item fails with `dependent_invalid`; both schemas remain at version 1 with their prior authored and effective content. The refusal comes from the existing dependant even though the authored change is otherwise only an annotation.

### TR-REG-504 — Publish a breaking contract under a new major

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json), [person_instance](../fixtures/registration/person_instance.json).

**Given:** a registered `person_schema` and a conforming `person_instance`. Prepare `person.v2~` in the same version family with an intentionally incompatible accepted-value set and its own matching `$id`.

**When:** submit v2 as a creation without `expected_resource_version`.

**Then:** v2 `succeeded` at version 1 with a different GTS ID and Registry Reference. Exact reads of v1 and v2 return their respective content; the v1 Instance still conforms to the exact v1 identifier. No compatibility verdict is required across different majors.

### TR-REG-505 — A revision refreshes transitive schema references

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json), [person_referrer_schema](../fixtures/registration/person_referrer_schema.json).

**Given:** register `person_schema`, `person_referrer_schema`, `outer_referrer_schema` and `independent_schema`. Build `outer_referrer_schema` from `person_referrer_schema` with entity name `outer_referrer`, a matching `$id`, and its `content.properties.payload.properties.person.$ref` retargeted to the GTS URI of `person_referrer_schema`. Build `independent_schema` from `person_schema` with entity name `independent` and a matching `$id`. The reference chain is `outer_referrer_schema → person_referrer_schema → person_schema`, without derivation edges. Read all four with `$select=content,origin,resolved_schema`, retaining bodies and ETags.

**When:** revise only `person_schema`, adding optional root property `nickname` of type string with `expected_resource_version=1`. Await success, then read all four with the same projection and their old ETags.

**Then:** `person_schema` succeeds at version 2 and returns its revised authored content. Both referrers return `200` with changed ETags and resolved schemas containing `nickname` at the nested person definition, including through both reference levels for `outer_referrer_schema`. Their authored content, GTS IDs, UUIDs, active lifecycle and own resource versions remain unchanged at version 1. `independent_schema` returns bodyless `304` with its previous ETag. Repeating the referrer reads with their new ETags returns bodyless `304`; no sleep or read retry is required after operation completion.


## Minor versions and version-family shape

### TR-REG-601 — Order contiguous minors within one batch

**Fixtures:** [person_minor_0_schema](../fixtures/registration/person_minor_0_schema.json), [person_minor_1_schema](../fixtures/registration/person_minor_1_schema.json).

**Given:** new compatible Type Schemas `person_minor_0_schema` and `person_minor_1_schema` in one family, each with its own matching `$id`. Neither minor exists.

**When:** submit `[v1.1, v1.0]` in one batch.

**Then:** both items `succeeded` at their own resource version 1. Exact reads find both distinct GTS IDs and Registry References. That an exact key never falls back to the major-only `person.v1~` is TR-READ-203's claim and is not repeated here.

### TR-REG-602 — Refuse a minor with a missing predecessor

**Fixtures:** [person_minor_0_schema](../fixtures/registration/person_minor_0_schema.json), [person_minor_2_schema](../fixtures/registration/person_minor_2_schema.json).

**Given:** `person_minor_0_schema` is registered, while `person.v1.1~` is absent. Prepare a compatible `person_minor_2_schema` candidate.

**When:** submit v1.2 alone and await completion.

**Then:** its item is `failed` with `missing_predecessor` and `resource_version=null`. V1.2 remains absent; v1.0 remains readable. The registry does not skip the absent v1.1 baseline.

### TR-REG-603 — One major cannot mix major-only and minor-bearing shapes

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json).

**Given:** two fresh families A and B. A has a registered major-only `v1~`. B has a registered first minor `v1.0~`. Neither family has the opposite shape.

**When:** submit A's `v1.0~` and B's `v1~` as creations, using separate operations or independent batch items.

**Then:** each candidate fails with `family_shape_conflict` and no resource version. Reads still find A's major-only and B's first minor, and do not find either rejected ID.

### TR-REG-604 — Refuse a content revision of a minor-bearing schema

**Fixtures:** [person_minor_0_schema](../fixtures/registration/person_minor_0_schema.json).

**Given:** `person_minor_0_schema` is registered at resource version 1. Prepare changed content for that same ID.

**When:** submit it with `expected_resource_version=1`; separately, submit an absent `person.v1.1~` with `expected_resource_version=1`.

**Then:** acceptance refuses the request synchronously with `400` RFC-9457 `invalid_argument` and a field violation for `expected_resource_version`. There is no `202` or operation `Location`; exact read still returns the original version-1 document. The absent minor is refused the same way: the refusal does not depend on whether the named minor currently exists.

### TR-REG-605 — A failed predecessor blocks the next minor

**Fixtures:** [person_minor_0_schema](../fixtures/registration/person_minor_0_schema.json), [person_minor_1_schema](../fixtures/registration/person_minor_1_schema.json).

**Given:** both `person_minor_0_schema` and `person_minor_1_schema` are absent. The v1.0 candidate refers to a missing schema; v1.1 would otherwise be compatible with a valid predecessor.

**When:** submit `[v1.1, v1.0]` in one batch.

**Then:** v1.0 fails with `dependency_not_found`; v1.1 fails with `blocked_by_predecessor`. Neither ID becomes readable. This reason differs from the `blocked_by_dependency` of a failed authored `$ref` or conforming-type edge.

### TR-REG-606 — Admit a compatible minor with changed content without force

**Fixtures:** [person_minor_0_schema](../fixtures/registration/person_minor_0_schema.json), [person_minor_1_schema](../fixtures/registration/person_minor_1_schema.json).

**Given:** register `person_minor_0_schema` and retain its version-1 body. Prepare `person_minor_1_schema` with optional string property `nickname` added at its closed root, leaving all existing constraints and its own matching `$id` intact.

**When:** submit the v1.1 candidate as a creation, without `expected_resource_version` or `force`, and await completion.

**Then:** v1.1 succeeds at its own resource version 1. Exact read returns its distinct GTS ID and UUID, the new optional property and `provenance.compat_forced=false`. V1.0 retains its original content, identity, timestamps and version. The content change is accepted by the ordinary cross-minor compatibility check.

### TR-REG-607 — Refuse an incompatible minor without force

**Fixtures:** [person_minor_0_schema](../fixtures/registration/person_minor_0_schema.json), [person_minor_1_schema](../fixtures/registration/person_minor_1_schema.json).

**Given:** register `person_minor_0_schema` and retain its version-1 body and ETag. Prepare `person_minor_1_schema` with `content.properties.name.minLength=2`; the predecessor accepted one-character names. Keep the candidate's `$id`, dialect and other constraints valid.

**When:** submit the v1.1 candidate as a creation without `force`, even though the default deployment permits explicitly requested compatibility waivers.

**Then:** acceptance returns `202`; the item fails `incompatible_with_baseline` with `resource_version=null`. V1.1 remains absent. V1.0's body and ETag remain unchanged. Deployment permission to use `force` does not apply it implicitly; TR-REG-903 covers an explicit waiver.


## Dry-run admission

### TR-REG-701 — Predict creation, then commit under a new key

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json), [person_instance](../fixtures/registration/person_instance.json).

**Given:** a new `person_schema` and its conforming `person_instance`.

**When:**

1. Submit `[instance, schema]` with `dry_run=true` under K1 and poll.
2. Confirm that neither ID is readable.
3. Submit the same items with `dry_run=false` under K2 and poll.

**Then:** the dry-run operation has `dry_run=true` and both items `succeeded` with `resource_version=null`; it reserves neither ID. The commit operation has `dry_run=false` and both items `succeeded` at version 1; both are then readable. K2 must differ from K1 because the mode participates in the idempotency fingerprint.

### TR-REG-702 — A mixed dry run predicts the committed item verdicts

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json), [missing_ref_schema](../fixtures/registration/missing_ref_schema.json), [blocked_instance](../fixtures/registration/blocked_instance.json).

**Given:** a batch like TR-REG-101: one independent valid schema A, one schema B with a missing `$ref`, and one Instance C conforming to B. All IDs are absent.

**When:** submit `[C, B, A]` as a dry run under K1, verify that no ID was written, then commit the same batch under K2 without an intervening entity mutation.

**Then:** both completed operations agree per GTS ID on item `status` and failure `reason`: A succeeds, B fails `dependency_not_found`, and C fails `blocked_by_dependency`. Dry-run A has no resource version; committed A has version 1 and is readable. B and C stay absent in both modes. The comparison does not require equal operation IDs, timestamps or successful-item versions.

### TR-REG-703 — Predict a revision beside an unchanged schema

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json).

**Given:** register `person_schema` and `other_person_schema`, a copy with entity name `other_person` and a matching `$id`; both are at version 1. Read each with `$select=content,origin,resolved_schema` and retain its body and ETag. Prepare a compatible revision of `person_schema` adding optional root string property `nickname`; submit `other_person_schema` with identical authored content. Both candidates carry `expected_resource_version=1`.

**When:** dry-run `[person_schema revision, other_person_schema unchanged]` under K1. After polling, read both again, then commit the same batch under K2 without any intervening entity mutation.

**Then:** the dry run reports `succeeded` with `resource_version=null` for `person_schema` and `unchanged` with `resource_version=1` for `other_person_schema`. After prediction both original bodies and ETags are unchanged. The real operation reports `succeeded` at version 2 for `person_schema` and `unchanged` at version 1 for `other_person_schema`. Only the revised schema's content and ETag change; the other schema retains its timestamps and original body.

### TR-REG-704 — An Instance sees its schema's virtual revision

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json), [person_instance](../fixtures/registration/person_instance.json).

**Given:** register only `person_schema` at version 1; its closed root has no `nickname` property. Retain its body and ETag with `$select=content,origin,resolved_schema`. Prepare a revision adding optional root string property `nickname` with `expected_resource_version=1`. Prepare the new `person_instance` with `nickname: "Al"` added to its content and omit `expected_resource_version` to require absence; that value would be rejected by the stored version-1 schema.

**When:** submit `[person_instance, person_schema revision]` with `dry_run=true` under K1. After checking the stored state, submit the identical batch with `dry_run=false` under K2.

**Then:** both dry-run items succeed with `resource_version=null`: the Instance is checked against the virtual revised schema even though it precedes that candidate in the request. The actual schema remains at version 1 with its original content and ETag, and the Instance is still absent. Real execution succeeds with schema version 2 and Instance version 1. Exact reads return the schema admitting `nickname` and the Instance carrying `nickname: "Al"`.

### TR-REG-705 — A passing dry run does not reserve the observed resource version

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json).

**Given:** register `person_schema` at version 1. Dry-run a candidate adding `description: "Proposed description"` with `expected_resource_version=1` under K1, and await `succeeded` with no resulting version. Then complete a real revision under K2 adding `title: "Concurrent title"` at version 2; retain that entity body and ETag.

**When:** submit the original description candidate as a real revision under K3, keeping its original `expected_resource_version=1`.

**Then:** K3 is accepted with `202` but finishes `failed`, `resource_version=null`, `error.reason=precondition_failed`. The entity retains K2's title, version-2 body and ETag; the proposed description is not installed. Polling K1 still returns its original successful prediction. The dry run neither reserved the version nor authorized rebasing a later write.

### TR-REG-706 — A failed virtual revision blocks its selected referrer

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json), [person_referrer_schema](../fixtures/registration/person_referrer_schema.json).

**Given:** register `person_schema` at version 1 and retain its body and ETag. Prepare an incompatible revision removing `name` from both `content.properties` and `content.required`, while keeping the root closed and payload required; the candidate is a valid schema but rejects previously accepted objects containing `name`. Give it `expected_resource_version=1`. Prepare new `person_referrer_schema` and `independent_schema`, a copy of the original `person_schema` under entity name `independent` with a matching `$id`.

**When:** dry-run `[person_referrer_schema, person_schema revision, independent_schema]` under K1. Read the stored state, then commit the identical batch under K2 with no intervening entity mutation.

**Then:** both operations report `incompatible_with_baseline` for `person_schema`, `blocked_by_dependency` for `person_referrer_schema`, and `succeeded` for `independent_schema`. The stored schema remains at version 1 with unchanged content and ETag after both operations; the referrer remains absent and cannot fall back to that stored definition. The independent schema is absent after the dry run, whose success has `resource_version=null`, and readable at version 1 only after the real operation. Match statuses and failure reasons by GTS ID across the two operations.


## Managed schema dialect and unstable major-zero profile

### TR-REG-801 — Refuse an inadmissible schema dialect before admission

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json), [person_instance](../fixtures/registration/person_instance.json).

**Given:** an absent `person_schema` and its valid `person_instance`. Remove the schema's top-level `$schema`. Other inadmissible spellings (a non-Draft-07 dialect, a divergent nested `$schema`) are covered by Rust acceptance tests.

**When:** submit `[instance, schema]` in one request.

**Then:** the entire request receives synchronous `400` RFC-9457 `invalid_argument` with one `entity` field violation naming the schema, and no operation `Location`. Neither ID is readable. The registry does not admit the valid batch neighbour and does not infer a default dialect.

### TR-REG-802 — Major zero allows breaking revisions but no registered Instance

**Fixtures:** [person_major_zero_schema](../fixtures/registration/person_major_zero_schema.json).

**Given:** a valid `person_major_zero_schema` is registered at version 1. Prepare a revision whose accepted-value set is incompatible with the first definition. Also prepare a new conforming Instance whose **own last segment** is stable `v1`, so its only unstable marker is the conforming Type Schema's `v0~` segment.

**When:** submit the schema revision with `expected_resource_version=1`, then submit the Instance under a new key.

**Then:** the breaking v0 revision `succeeded` at version 2 and is readable. The Instance request receives `202`, but its item finishes `failed` with `instance_of_major_zero` and no resource version; the Instance remains absent. This is distinct from the synchronous identifier refusal for an Instance whose own last segment is v0.

### TR-REG-803 — Stable schemas cannot derive from or `$ref` major zero

**Fixtures:** [person_major_zero_schema](../fixtures/registration/person_major_zero_schema.json).

**Given:** a registered `person_major_zero_schema` as the base. Prepare two new stable `v1~` schemas: one derives from that base by identifier only, with no authored `$ref` to it, and the other is an independent root containing an authored `$ref` to it. A derived schema that also `$ref`s its base reports `stable_refs_major_zero`, because the first quarantined edge in extraction order wins.

**When:** submit both candidates in a registration batch and poll.

**Then:** acceptance returns `202`. The derived candidate finishes `failed` with `stable_derives_from_major_zero`; the referrer finishes `failed` with `stable_refs_major_zero`. Neither is readable. These are worker item outcomes in the current API, not synchronous HTTP refusals.

## Deployment policy and compatibility waiver

### TR-REG-901 — A region outside the allowlist refuses a non-platform vendor

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json).

**Given:** a new valid Type Schema whose GTS ID has a last-segment vendor other than `cf`, such as an `acme` root Type Schema, and a matching authored `$id`, built from the person schema by replacing its vendor and package so the ID falls under `gts.acme.outside.*`. The local launcher allows `acme` only under `gts.acme.e2e.*`; other regions retain the closed default.

**When:** submit it as a creation.

**Then:** acceptance refuses it synchronously with `400` RFC-9457 `failed_precondition`: one `REGISTRATION_POLICY_ALLOWED_VENDORS` violation whose subject is the implicit `<default>` region, because no matching region provides `allowed_vendors`. There is no operation `Location` and the ID is not readable. A `cf`-vendor ID would not exercise this policy gate.

### TR-REG-902 — Disabled `force` is a synchronous refusal

**Fixtures:** [person_minor_0_schema](../fixtures/registration/person_minor_0_schema.json), [person_minor_1_schema](../fixtures/registration/person_minor_1_schema.json).

**Given:** a registered first minor `person_minor_0_schema` and a `person_minor_1_schema` candidate carrying `force=true`. The local launcher keeps `allow_compatibility_force=false` in its `force-disabled` profile.

**When:** submit the candidate, then submit it again with `dry_run=true`.

**Then:** each request is refused before `202` with `400` RFC-9457 `invalid_argument` and a `force` field violation. Neither creates an operation or v1.1 entity: a dry run cannot be used to probe a waiver the deployment does not allow.

### TR-REG-903 — Enabled `force` records a waived minor transition

**Fixtures:** [person_minor_0_schema](../fixtures/registration/person_minor_0_schema.json), [person_minor_1_schema](../fixtures/registration/person_minor_1_schema.json).

**Given:** the shared e2e config has `allow_compatibility_force=true`. A stable `person_minor_0_schema` schema is registered. Its `person_minor_1_schema` candidate is incompatible with v1.0 but carries `force=true`.

**When:** submit v1.1 and await completion, then read both minors with `$select=content,provenance`.

**Then:** v1.1 `succeeded` at version 1, with its incompatible content and `provenance.compat_forced=true`. V1.0 remains unchanged and reports `compat_forced=false`. The waiver belongs only to the cross-minor comparison and does not revise v1.0. Every run but the force-disabled profile runs this scenario.

### TR-REG-904 — A configured region admits its allowed vendor

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json).

**Given:** the shared e2e config configures `registration_policy` with `gts.acme.e2e.*` allowing vendor `acme`. Build a new `person_schema` in the fresh e2e namespace by changing its vendor from `cf` to `acme`, with a matching authored `$id`.

**When:** submit the schema as a creation and await completion.

**Then:** acceptance returns `202`; its item `succeeded` at resource version 1. An exact read returns the complete authored and resolved schema under its `acme` GTS ID and deterministic Registry Reference. TR-REG-901 confirms that the same vendor remains refused outside the configured region.
