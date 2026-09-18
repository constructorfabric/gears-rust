# Entity deletion scenarios

These scenarios cover entity deletion end to end on the P0 async admission API:
both deletion routes, dependency-ordered execution, tombstones and the two
refusals a caller is most likely to meet. The local launcher currently binds
Types Registry to **SQLite**; this run does not establish PostgreSQL/MySQL
conformance or tenant/PDP authorization.

Contract sources: [DESIGN — Dependency Graph & Deletion Safety](../../../../../gears/system/types-registry/docs/DESIGN.md#dependency-graph--deletion-safety)
and [P0 tasks](../../../../../gears/system/types-registry/docs/p0/todo.md)
(T20, T20a).

Each scenario below is implemented by one test marked
`@pytest.mark.scenario("TR-DEL-NNN")` in [test_deletion.py](../test_deletion.py).
Complete expected bodies live in the tests as inline Python dictionaries; this
document states intent, not payloads.

The shared execution rules these scenarios rely on — per-test namespaces,
idempotency keys, outcome matching — and the commands that run them are in the
[suite README](../README.md).

## What deletion promises

Three properties distinguish deletion from registration, and the scenarios below
exist to pin them:

- **Two routes, one protocol.** `DELETE {registry_api}/entities/{entity_key}`
  with a required positive `expected_resource_version` query parameter is one
  item's worth of `POST {registry_api}/entities:batchDelete`, whose items are
  `{key, expected_resource_version}`. Both answer `202` with the operation's
  `Location`. `If-Match` is refused rather than ignored.
- **Execution is dependency-ordered; reporting is not.** The worker reads the
  candidates and the edges among them, then deletes dependants before their
  targets, whatever order the request used. Outcomes still come back in
  *request* order, so a caller that deleted by Registry Reference can match
  identifier-keyed outcomes positionally.
- **A precondition failure is an item, not a status code.** A stale
  `expected_resource_version` is reported as a terminal `precondition_failed`
  item on a `completed` operation, never as HTTP `412`.

A successful deletion advances the entity's `resource_version` by one and leaves
a tombstone: `lifecycle_status` becomes `deleted` and the rest of the body —
content, resolved schema, effective traits, creation timestamp — stays exactly
readable until purge. Deletion allocates no revision, so unlike a registration
it adds nothing to the entity's revision history.

## Scenarios

### TR-DEL-001 — Delete one entity and leave a readable tombstone

**Given:** [person_schema.json](../fixtures/deletion/person_schema.json),
registered and read back at `lifecycle_status=active`, `resource_version=1`.

**When:** `DELETE {registry_api}/entities/{gts_id}?expected_resource_version=1`,
then await completion.

**Then:** the sole item succeeds with `resource_version=2` and no error. Reading
the entity again still returns `200`, and its body is the body observed before
the deletion with exactly three fields moved: `lifecycle_status` is `deleted`,
`resource_version` is 2, and `updated_at` falls inside the deletion operation's
start/completion interval.

### TR-DEL-002 — Order dependants before their target within a batch

**Given:** [person_schema.json](../fixtures/deletion/person_schema.json) and
[person_instance.json](../fixtures/deletion/person_instance.json), both
registered. The Instance names that schema as its conforming type, so a stored
dependency edge runs from the Instance to the schema.

**When:** submit both to `:batchDelete` naming **the schema first**, at
`expected_resource_version=1` each, and await completion. Submission order is
the point: executing it literally would refuse the schema for having a live
dependant.

**Then:** both items succeed with `resource_version=2`, and both entities read
back as `deleted`. Ordering resolves the in-batch edge; it adds no refusal.

### TR-DEL-003 — Report outcomes in request order

**Given:** [person_schema.json](../fixtures/deletion/person_schema.json) and
[other_schema.json](../fixtures/deletion/other_schema.json), both registered and
independent of one another, so nothing reorders them for dependency reasons.
`other.v1~` sorts before `person.v1~`.

**When:** submit both to `:batchDelete` naming **`person` first**, and await
completion.

**Then:** the outcomes are `[person, other]` — request order, not identifier
order — each `succeeded` with `resource_version=2`.

### TR-DEL-004 — Report a stale version as a terminal item, not a 412

**Given:** [person_schema.json](../fixtures/deletion/person_schema.json),
registered at `resource_version=1`.

**When:** delete it naming `expected_resource_version=7`, and await completion.

**Then:** the submission is still accepted with `202`, and the operation
completes with one `failed` item whose `error.reason` is `precondition_failed`
and whose `resource_version` is `null`. The entity is untouched: still `active`
at `resource_version=1`.

### TR-DEL-005 — Refuse a deletion that would strand a live dependant

**Given:** [person_schema.json](../fixtures/deletion/person_schema.json) and
[person_instance.json](../fixtures/deletion/person_instance.json), both
registered, as in TR-DEL-002.

**When:** delete **only the schema** at `expected_resource_version=1`, leaving
its Instance live, and await completion.

**Then:** one `failed` item with `error.reason` `has_registered_dependents` and
`resource_version` `null`; the schema is still `active` at `resource_version=1`.
The message states how many dependants, never which ones — the caller may not be
entitled to read them. This is the complement of TR-DEL-002: batch ordering
resolves in-batch dependants and never turns an external one into a blocker.
