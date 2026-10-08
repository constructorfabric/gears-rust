# Exact and batch read scenarios

End-to-end read scenarios cover exact `GET /entities/{entity_key}`, `:batchGet` and conditional reads over live entities and tombstones. The local launcher uses **SQLite**; these tests do not prove PostgreSQL/MySQL or tenant/PDP behavior.

Every scenario has an e2e test in [test_reading.py](../test_reading.py); the [suite README](../README.md) lists each group. Each scenario lists its source fixtures before Given and refers to them by name in Given/When/Then; all fixture links point to `fixtures/reading/`. Every scenario uses a fresh namespace, and registration and deletion in a Given are completed before the read. Independent `other_*` and `device_*` entities are controls: they show that each response answers its own key and that unrequested entities stay out of it.

Responses are compared whole as JSON values. The mandatory `gts_id`, `gts_uuid`, `kind` and `lifecycle_status` accompany every selection, and an absent `$select` returns the document-free default metadata. A field that does not apply to an entity's kind is absent, not `null`. Batch results are matched by echoed `entity_key` after checking completeness and uniqueness, because batch order is not contractual. ETags are compared byte for byte; a `304` carries its ETag and has no body. Reads follow a terminal operation immediately, without sleeps or retries.

## Contents

- [Exact read](#exact-read)
  - [TR-READ-001 — Default read is document-free managed metadata](#tr-read-001--default-read-is-document-free-managed-metadata)
  - [TR-READ-002 — GTS ID and Registry Reference resolve to the same entity](#tr-read-002--gts-id-and-registry-reference-resolve-to-the-same-entity)
  - [TR-READ-003 — Authored content is selected independently](#tr-read-003--authored-content-is-selected-independently)
  - [TR-READ-004 — Effective documents are independently selectable](#tr-read-004--effective-documents-are-independently-selectable)
  - [TR-READ-005 — Provenance is one selected group](#tr-read-005--provenance-is-one-selected-group)
  - [TR-READ-006 — Inapplicable Instance artifacts are absent](#tr-read-006--inapplicable-instance-artifacts-are-absent)
  - [TR-READ-007 — A projected tombstone remains distinguishable from absence](#tr-read-007--a-projected-tombstone-remains-distinguishable-from-absence)
  - [TR-READ-008 — Absent and impossible keys have one exact-read error shape](#tr-read-008--absent-and-impossible-keys-have-one-exact-read-error-shape)
- [Batch read](#batch-read)
  - [TR-READ-101 — Mixed batch answers every key and preserves absence](#tr-read-101--mixed-batch-answers-every-key-and-preserves-absence)
  - [TR-READ-102 — Top-level batch projection equals an exact read](#tr-read-102--top-level-batch-projection-equals-an-exact-read)
  - [TR-READ-103 — A batch can read a tombstone beside a live entity](#tr-read-103--a-batch-can-read-a-tombstone-beside-a-live-entity)
  - [TR-READ-104 — An unknown selection is refused on both read transports](#tr-read-104--an-unknown-selection-is-refused-on-both-read-transports)
  - [TR-READ-105 — Batch-wide If-None-Match is refused](#tr-read-105--batch-wide-if-none-match-is-refused)
- [Consistency across read routes](#consistency-across-read-routes)
  - [TR-READ-201 — All read routes observe the completed content revision](#tr-read-201--all-read-routes-observe-the-completed-content-revision)
  - [TR-READ-202 — Selected null content remains present on exact read](#tr-read-202--selected-null-content-remains-present-on-exact-read)
  - [TR-READ-203 — Exact keys never fall back to a registered minor version](#tr-read-203--exact-keys-never-fall-back-to-a-registered-minor-version)
- [Conditional reads](#conditional-reads)
  - [TR-READ-301 — An exact ETag survives no-op admission and changes after revision](#tr-read-301--an-exact-etag-survives-no-op-admission-and-changes-after-revision)
  - [TR-READ-302 — Batch revalidation answers each key independently](#tr-read-302--batch-revalidation-answers-each-key-independently)
  - [TR-READ-303 — A validator belongs to one projection](#tr-read-303--a-validator-belongs-to-one-projection)
  - [TR-READ-304 — A base refresh invalidates its derived schema's ETag](#tr-read-304--a-base-refresh-invalidates-its-derived-schemas-etag)

## Exact read

### TR-READ-001 — Default read is document-free managed metadata

**Fixtures:** [person_schema](../fixtures/reading/person_schema.json), [person_instance](../fixtures/reading/person_instance.json), [other_schema](../fixtures/reading/other_schema.json), [other_instance](../fixtures/reading/other_instance.json).

**Given:** target `person_schema` is registered alongside its `person_instance` and the independent `other_schema`/`other_instance` pair.

**When:** GET `person_schema` and `other_schema` by their distinct GTS IDs without `$select`.

**Then:** both `200` JSON bodies contain exactly `gts_id`, deterministic `gts_uuid`, `kind=type_schema`, `lifecycle_status=active`, and managed `origin` with `resource_version=1` and RFC 3339 `created_at`/`updated_at`; no authored or effective document is returned. Each body identifies its requested schema.

### TR-READ-002 — GTS ID and Registry Reference resolve to the same entity

**Fixtures:** [person_schema](../fixtures/reading/person_schema.json), [person_instance](../fixtures/reading/person_instance.json), [other_schema](../fixtures/reading/other_schema.json), [other_instance](../fixtures/reading/other_instance.json).

**Given:** register `person_schema` with `person_instance`, and `other_schema` with `other_instance`. All four keys are distinct.

**When:** GET `person_schema` once by GTS ID and once by its returned `gts_uuid`, then GET `other_schema` by its GTS ID.

**Then:** the two `person_schema` bodies are identical, including `origin` and lifecycle. The `other_schema` body has its own identity and differs.

### TR-READ-003 — Authored content is selected independently

**Fixtures:** [person_schema](../fixtures/reading/person_schema.json), [person_instance](../fixtures/reading/person_instance.json), [other_schema](../fixtures/reading/other_schema.json), [other_instance](../fixtures/reading/other_instance.json).

**Given:** registered `person_schema` with `person_instance`, and `other_schema` with `other_instance`.

**When:** GET all four keys with `$select=content`.

**Then:** each of the four bodies has its own `gts_id`, `gts_uuid`, `kind`, `lifecycle_status`, and whole authored `content` JSON; the two schemas report `kind=type_schema` and the two Instances report `kind=instance`. No body contains `origin` or an unselected schema-only artifact.

### TR-READ-004 — Effective documents are independently selectable

**Fixtures:** [trait_base_schema](../fixtures/reading/trait_base_schema.json), [trait_derived_schema](../fixtures/reading/trait_derived_schema.json), [trait_employee_instance](../fixtures/reading/trait_employee_instance.json), [other_schema](../fixtures/reading/other_schema.json), [other_instance](../fixtures/reading/other_instance.json), [trait_derived_extra_field_instance](../fixtures/reading/trait_derived_extra_field_instance.json).

**Given:** registered `trait_base_schema`, `trait_derived_schema`, and their conforming `trait_employee_instance`, plus the independent `other_schema`/`other_instance` pair. The base contributes required top-level `name`, an open `payload` and trait `category=internal`, constrained to `internal|public`; the derived schema requires `payload.employee_id`. The resolved schema inlines the base in place of its `$ref`, without the base's `$id`, `$schema` and trait keywords.

**When:** GET the derived schema separately with `$select=resolved_schema`, `effective_traits`, and `effective_traits_schema`; also GET `trait_employee_instance` and `other_instance` with `$select=resolved_schema`. In a separate operation, submit `trait_derived_extra_field_instance`, which adds an undeclared top-level `rogue` field, and await its terminal outcome.

**Then:** each derived-schema response includes exactly its named document plus the mandatory `gts_id`, `gts_uuid`, `kind`, and `lifecycle_status`; selecting traits does not transfer `resolved_schema`. The resolved schema requires `payload.employee_id`; effective traits contain `category=internal`, and the effective traits schema constrains it to `internal|public`. Both Instance responses have only their mandatory identity, kind and lifecycle fields, without `resolved_schema`. The valid derived Instance succeeds; the separate extra-field item has `status=failed`, `resource_version=null`, and `error.reason=invalid_value`, and its key remains absent. Together they show that the inherited envelope is closed while `payload` accepts derived fields.

### TR-READ-005 — Provenance is one selected group

**Fixtures:** [person_schema](../fixtures/reading/person_schema.json), [person_instance](../fixtures/reading/person_instance.json), [other_schema](../fixtures/reading/other_schema.json), [other_instance](../fixtures/reading/other_instance.json).

**Given:** register `person_schema`, `person_instance`, `other_schema`, and `other_instance` in one fresh namespace.

**When:** GET both schemas and both Instances with `$select=provenance`.

**Then:** each selected group contains exactly `gts_spec_version`, `gts_impl_version`, and `compat_forced`; both Instances carry an explicit JSON `null` for `compat_forced`, while both schemas carry `false`. No provenance member or owning-gear attribution appears at the entity's top level.

### TR-READ-006 — Inapplicable Instance artifacts are absent

**Fixtures:** [person_schema](../fixtures/reading/person_schema.json), [person_instance](../fixtures/reading/person_instance.json), [other_schema](../fixtures/reading/other_schema.json), [other_instance](../fixtures/reading/other_instance.json).

**Given:** Register `person_schema`, `person_instance`, `other_schema`, and `other_instance` in one fresh namespace. `person_instance` is the target; `other_schema` is a contrasting Type Schema.

**When:** GET `person_instance` with a selection of `resolved_schema,effective_traits,effective_traits_schema`; also GET `other_schema` with `$select=resolved_schema`.

**Then:** the Instance's `200` body contains only `gts_id`, `gts_uuid`, `kind=instance`, and `lifecycle_status`; the three Type Schema-only fields are absent rather than JSON `null`. The distinct schema response contains `resolved_schema`.

### TR-READ-007 — A projected tombstone remains distinguishable from absence

**Fixtures:** [person_schema](../fixtures/reading/person_schema.json), [other_schema](../fixtures/reading/other_schema.json), [other_instance](../fixtures/reading/other_instance.json), [device_schema](../fixtures/reading/device_schema.json), [device_instance](../fixtures/reading/device_instance.json), [person_instance](../fixtures/reading/person_instance.json).

**Given:** `person_schema` is registered beside live `other_schema`/`other_instance` and `device_schema`/`device_instance` pairs. Read person with `$select=content` and retain its ETag, then delete person with a completed operation. `person_instance` is absent, so it cannot block deletion.

**When:** exact-GET the deleted person's key with `$select=content` and its old ETag in `If-None-Match`; repeat with the tombstone's new ETag. GET the live other's key with `$select=content`.

**Then:** the old live ETag yields `200` with a new ETag, authored content, `kind=type_schema`, and `lifecycle_status=deleted` with mandatory identity, even though neither `kind` nor `lifecycle_status` was named in the selection. The tombstone's ETag yields a bodyless `304` carrying that same ETag. The other response has its own content and `lifecycle_status=active`. Exact lookup has no lifecycle filter: it returns a tombstone by key even though default discovery lists only active entities.

### TR-READ-008 — Absent and impossible keys have one exact-read error shape

**Fixtures:** [person_schema](../fixtures/reading/person_schema.json), [person_instance](../fixtures/reading/person_instance.json), [other_schema](../fixtures/reading/other_schema.json), [other_instance](../fixtures/reading/other_instance.json).

**Given:** Register `person_schema`, `person_instance`, `other_schema`, and `other_instance` in one fresh namespace. Prepare an unregistered GTS ID in that namespace, an unregistered UUID, and the impossible key `not-a-gts-id`.

**When:** GET each by key.

**Then:** all three return `404` with `application/problem+json`, equal Problem type/status and a request-specific `instance`; none is silently treated as a successful empty representation. The absent UUID exercises reverse lookup separately from the absent GTS ID.

## Batch read

### TR-READ-101 — Mixed batch answers every key and preserves absence

**Fixtures:** [person_schema](../fixtures/reading/person_schema.json), [person_instance](../fixtures/reading/person_instance.json), [other_schema](../fixtures/reading/other_schema.json), [other_instance](../fixtures/reading/other_instance.json).

**Given:** Register `person_schema`, `person_instance`, `other_schema`, and `other_instance` in one fresh namespace. Prepare an absent valid GTS ID and an unregistered UUID. Request `person_schema` and `person_instance`; leave `other_schema` and `other_instance` unrequested.

**When:** POST `:batchGet` with four keys: absent GTS ID, `person_schema`'s ID, absent UUID, and `person_instance`'s ID.

**Then:** `200` JSON contains exactly the four requested keys, each once; neither unrequested registered key appears. Both absent results are `not_found` without `entity` or `etag`, and found results contain an `etag` and the default document-free entity metadata shape. Neither kind of missing key prevents the other keys from being answered.

### TR-READ-102 — Top-level batch projection equals an exact read

**Fixtures:** [person_schema](../fixtures/reading/person_schema.json), [person_instance](../fixtures/reading/person_instance.json), [other_schema](../fixtures/reading/other_schema.json), [other_instance](../fixtures/reading/other_instance.json).

**Given:** Register `person_schema`, `person_instance`, `other_schema`, and `other_instance` in one fresh namespace. Request `person_schema` and `person_instance`; leave `other_schema` and `other_instance` unrequested.

**When:** POST `:batchGet` with top-level `"$select": "content,provenance"` for both keys, and GET each key with the same selection.

**Then:** the batch has exactly the two requested keys, no other registered key, and each `entity` body equals its exact GET body as a JSON value. Each batch `etag` is byte-identical to that key's exact GET `ETag` under the same selection. Each entity has only the mandatory `gts_id`, `gts_uuid`, `kind`, and `lifecycle_status`, authored `content`, and the `provenance` group.

### TR-READ-103 — A batch can read a tombstone beside a live entity

**Fixtures:** [person_schema](../fixtures/reading/person_schema.json), [other_schema](../fixtures/reading/other_schema.json), [other_instance](../fixtures/reading/other_instance.json), [device_schema](../fixtures/reading/device_schema.json), [device_instance](../fixtures/reading/device_instance.json), [person_instance](../fixtures/reading/person_instance.json).

**Given:** register `person_schema`, `other_schema`/`other_instance`, and `device_schema`/`device_instance` in a fresh namespace. Delete person with a completed operation; the other and device pairs remain active. `person_instance` is absent, so it cannot block deletion.

**When:** batch read `person_schema` and `other_schema` with `"$select": "content"`.

**Then:** exactly the two requested keys are `found`, with no device key; they report `deleted` versus `active` and return their respective authored content.

### TR-READ-104 — An unknown selection is refused on both read transports

**Fixtures:** [person_schema](../fixtures/reading/person_schema.json), [person_instance](../fixtures/reading/person_instance.json), [other_schema](../fixtures/reading/other_schema.json), [other_instance](../fixtures/reading/other_instance.json).

**Given:** Register `person_schema`, `person_instance`, `other_schema`, and `other_instance` in one fresh namespace. `person_schema` is the request target.

**When:** request `$select=contents` on exact GET and as the top-level batch body field in separate requests.

**Then:** both return `400` Problem JSON with a `$select` field violation and `INVALID_SELECT`; neither falls back to the default representation.

### TR-READ-105 — Batch-wide If-None-Match is refused

**Fixtures:** [person_schema](../fixtures/reading/person_schema.json), [person_instance](../fixtures/reading/person_instance.json), [other_schema](../fixtures/reading/other_schema.json), [other_instance](../fixtures/reading/other_instance.json).

**Given:** Register `person_schema`, `person_instance`, `other_schema`, and `other_instance` in one fresh namespace. `person_schema` is the request target.

**When:** POST `:batchGet` with an `If-None-Match` header.

**Then:** `400` Problem JSON names `If-None-Match`. Batch conditions are supplied per item, so this header is refused rather than ignored or applied to every result.

## Consistency across read routes

### TR-READ-201 — All read routes observe the completed content revision

**Fixtures:** [person_schema](../fixtures/reading/person_schema.json), [person_instance](../fixtures/reading/person_instance.json), [other_schema](../fixtures/reading/other_schema.json), [other_instance](../fixtures/reading/other_instance.json), [person_schema_revised](../fixtures/reading/person_schema_revised.json).

**Given:** register mutable, major-only `person_schema` at resource version 1, `person_instance`, and the independent `other_schema`/`other_instance` pair. Read person through exact GET, batchGet and namespace-scoped discovery with `$select=content,origin`, asserting its initial content and version on each route. The discovery result contains exactly all four registered identities and their known content; retain person's GTS ID, UUID and `created_at`.

**When:** submit `person_schema_revised` with a fresh idempotency key, changing only the authored title from `Person` to `Revised Person` under `expected_resource_version=1`, and await a successful item at resource version 2. Repeat all three reads with the same selection, using the retained UUID for exact GET and the GTS ID for batchGet. Start a fresh discovery walk after the operation completes.

**Then:** the target entity on every route returns exactly the mandatory fields, updated authored `content` and managed `origin` with `resource_version=2`. The entity bodies agree and match the independently constructed updated document. GTS ID, UUID, kind, active lifecycle and `created_at` remain unchanged; `updated_at` is valid RFC 3339, agrees across routes and is not earlier than its previous value. Discovery still contains exactly the four expected identities: only person changed, and the other schema and both Instances retain their authored content and resource versions. No route returns the version-1 person content or metadata. No sleep or read retry is needed after successful operation completion.

### TR-READ-202 — Selected null content remains present on exact read

**Fixtures:** [null_schema](../fixtures/reading/null_schema.json), [null_instance](../fixtures/reading/null_instance.json), [other_schema](../fixtures/reading/other_schema.json), [other_instance](../fixtures/reading/other_instance.json).

**Given:** registered `null_schema`, whose authored JSON Schema has `type: "null"`, and its conforming `null_instance`, submitted with an explicitly present `"content": null`. The independent `other_schema` and `other_instance` are also registered in the same namespace.

**When:** exact-GET the null Instance first with `$select=content` and then without selection. Also GET `other_instance` with `$select=content` as a contrasting key.

**Then:** the selected null Instance has exactly the mandatory `gts_id`, `gts_uuid`, `kind=instance`, `lifecycle_status=active`, and a present `content` field whose JSON value is `null`. Its default representation has the default metadata fields and omits `content` entirely. The other Instance returns its distinct key and non-null authored content. The whole-body comparison distinguishes a present `null` from a missing field.

### TR-READ-203 — Exact keys never fall back to a registered minor version

**Fixtures:** [person_minor_schema](../fixtures/reading/person_minor_schema.json), [other_schema](../fixtures/reading/other_schema.json), [other_instance](../fixtures/reading/other_instance.json), [device_schema](../fixtures/reading/device_schema.json), [device_instance](../fixtures/reading/device_instance.json), [person_schema](../fixtures/reading/person_schema.json).

**Given:** registered `person_minor_schema`, naming `person.v1.0~` in the fresh test namespace with its matching authored `$id`, plus `other_schema`/`other_instance` and `device_schema`/`device_instance` pairs. Do not register `person_schema`: the same family's major-only `person.v1~` must remain absent. Registration of all entities has completed successfully.

**When:** exact-GET both full GTS IDs with `$select=content`, then batchGet `[major-only ID, minor-bearing ID]` with the same selection.

**Then:** exact GET of the major-only ID returns `404` Problem JSON, while exact GET of the minor-bearing ID returns `200` with its exact identity and authored content. The batch returns `200` with two results matched by echoed key: `not_found` without `entity` for the major-only key, and `found` with the expected minor entity for the other key. No route synthesizes a major-only entity or treats an exact key as a discovery pattern.

## Conditional reads

### TR-READ-301 — An exact ETag survives no-op admission and changes after revision

**Fixtures:** [person_schema](../fixtures/reading/person_schema.json), [person_instance](../fixtures/reading/person_instance.json), [other_schema](../fixtures/reading/other_schema.json), [other_instance](../fixtures/reading/other_instance.json), [person_schema_revised](../fixtures/reading/person_schema_revised.json).

**Given:** Register `person_schema`, `person_instance`, `other_schema`, and `other_instance` in one fresh namespace. GET `person_schema` and `person_instance` with `$select=content,origin`; retain their ETags and version-1 bodies.

**When:** GET the schema again with the same selection and `If-None-Match`. Re-register identical schema content with `expected_resource_version=1` and await an `unchanged` item; repeat the conditional GET. Then submit `person_schema_revised` with version 1 as its precondition, await a successful version-2 item, and GET both the schema and Instance with their old ETags.

**Then:** both schema reads before the revision return bodyless `304` with the original ETag. After revision the schema returns `200`, the independently expected revised content and origin at version 2, and a different ETag; its new ETag returns bodyless `304`. The unchanged Instance returns bodyless `304` with its original ETag, because its own resource version did not move. Read immediately after each terminal operation without sleeps or retries.

### TR-READ-302 — Batch revalidation answers each key independently

**Fixtures:** [person_schema](../fixtures/reading/person_schema.json), [person_instance](../fixtures/reading/person_instance.json), [other_schema](../fixtures/reading/other_schema.json), [other_instance](../fixtures/reading/other_instance.json), [person_schema_revised](../fixtures/reading/person_schema_revised.json).

**Given:** Register `person_schema`, `person_instance`, `other_schema`, and `other_instance` in one fresh namespace. First batchGet `person_schema` and `other_schema` with top-level `"$select": "content,origin"`; retain each result's `etag`. Revise person using `person_schema_revised`, await success at version 2, and prepare an unregistered key.

**When:** batchGet other with its retained `if_none_match`, person with its now-stale `if_none_match`, `other_instance` without a condition, and the absent key with any retained tag. Build a second request from only the three keys that returned an `etag`, copying each into its next `if_none_match`.

**Then:** the first conditional batch returns HTTP `200`: other is `unchanged` with the same `etag` and no `entity`; person is `found` with its revised document and new `etag`; the unconditioned Instance is `found` with its own content and `etag`; the absent key is `not_found` without either field. The second batch returns HTTP `200` with three `unchanged` results. Match every result by echoed key after checking completeness and uniqueness, since batch order is not contractual; a UUID key is echoed lowercase and hyphenated.

### TR-READ-303 — A validator belongs to one projection

**Fixtures:** [person_schema](../fixtures/reading/person_schema.json), [person_instance](../fixtures/reading/person_instance.json), [other_schema](../fixtures/reading/other_schema.json), [other_instance](../fixtures/reading/other_instance.json).

**Given:** Register `person_schema`, `person_instance`, `other_schema`, and `other_instance` in one fresh namespace. Person and other schema documents differ.

**When:** GET `person_schema` and `person_instance` without `$select` and retain their ETags. GET each key with `$select=content` and `If-None-Match` set to its metadata ETag; repeat each with its content response's ETag. GET `other_schema` with `$select=content` as a contrasting key.

**Then:** both wider reads return `200`, their own full authored content and ETags different from their metadata ETags; their repeats return bodyless `304` carrying the corresponding content ETags. The other schema returns its own document. A metadata validator never declares a wider schema or Instance representation unchanged.

### TR-READ-304 — A base refresh invalidates its derived schema's ETag

**Fixtures:** [trait_base_schema](../fixtures/reading/trait_base_schema.json), [trait_derived_schema](../fixtures/reading/trait_derived_schema.json), [trait_employee_instance](../fixtures/reading/trait_employee_instance.json), [other_schema](../fixtures/reading/other_schema.json), [other_instance](../fixtures/reading/other_instance.json), [trait_base_schema_revised](../fixtures/reading/trait_base_schema_revised.json).

**Given:** registered `trait_base_schema`, `trait_derived_schema`, `trait_employee_instance`, and the independent `other_schema`/`other_instance` pair. GET the derived schema and both controls with `$select=resolved_schema,origin`; retain their ETags. The derived `resolved_schema` requires `payload.employee_id`, and its own `resource_version` is 1.

**When:** submit `trait_base_schema_revised`, changing only the base title under `expected_resource_version=1`, and await a successful item. GET the derived schema and both controls with the same selection and their old ETags; repeat the derived read with its new ETag.

**Then:** the derived old ETag returns `200`: its resolved schema contains the revised base title and still requires `payload.employee_id`, its own `resource_version` remains 1, and its ETag changes. Both independent control reads return bodyless `304` with unchanged ETags. The derived new ETag also returns a bodyless `304`. No read retry is needed after the base operation completes.
