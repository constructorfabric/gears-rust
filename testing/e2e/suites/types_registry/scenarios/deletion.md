# Entity deletion scenarios

End-to-end deletion scenarios cover both routes, dependency ordering, tombstones
and common refusals. The local launcher uses **SQLite**; these tests do not prove
PostgreSQL/MySQL or tenant/PDP behavior.

Contract sources: [DESIGN — Dependency Graph & Deletion Safety](../../../../../gears/system/types-registry/docs/DESIGN.md#dependency-graph--deletion-safety)
and [P0 tasks](../../../../../gears/system/types-registry/docs/p0/todo.md)
(T20, T20a).

Each heading maps to `@pytest.mark.scenario("TR-DEL-NNN")` in
[test_deletion.py](../test_deletion.py); tests contain complete expected bodies.

The shared execution rules these scenarios rely on — per-test namespaces,
idempotency keys, outcome matching — and the commands that run them are in the
[suite README](../README.md).

## What deletion promises

- Single and batch routes require positive `expected_resource_version`, return
  `202` plus `Location`, and reject `If-Match`.
- Execution deletes dependants first; outcomes remain in request order.
- Stale versions become terminal `precondition_failed` items, not HTTP `412`.

Deletion increments `resource_version` and leaves a readable tombstone without
creating a revision.

## Scenarios

### TR-DEL-001 — Delete one entity and leave a readable tombstone

**Given:** [person_schema.json](../fixtures/deletion/person_schema.json),
registered and read back at `lifecycle_status=active`, `resource_version=1`.

**When:** `DELETE {registry_api}/entities/{gts_id}?expected_resource_version=1`,
then await completion.

**Then:** the item succeeds at version 2. The tombstone preserves the prior body,
except for lifecycle, version and `updated_at`.

### TR-DEL-002 — Order dependants before their target within a batch

**Given:** a registered [schema](../fixtures/deletion/person_schema.json) and its
conforming [Instance](../fixtures/deletion/person_instance.json).

**When:** batch-delete both at version 1, naming **the schema first**.

**Then:** both succeed at version 2 and read back as deleted.

### TR-DEL-003 — Report outcomes in request order

**Given:** independent `person` and `other` schemas; `other` sorts first.

**When:** submit both to `:batchDelete` naming **`person` first**, and await
completion.

**Then:** outcomes are `[person, other]` in request order, both at version 2.

### TR-DEL-004 — Report a stale version as a terminal item, not a 412

**Given:** [person_schema.json](../fixtures/deletion/person_schema.json),
registered at `resource_version=1`.

**When:** delete it naming `expected_resource_version=7`, and await completion.

**Then:** submission returns `202`; the item fails with `precondition_failed` and
no version. The entity remains active at version 1.

### TR-DEL-005 — Refuse a deletion that would strand a live dependant

**Given:** [person_schema.json](../fixtures/deletion/person_schema.json) and
[person_instance.json](../fixtures/deletion/person_instance.json), both
registered, as in TR-DEL-002.

**When:** delete **only the schema** at `expected_resource_version=1`, leaving
its Instance live, and await completion.

**Then:** the item fails with `has_registered_dependents`, no version, and only a
dependant count. The schema remains active at version 1.
