# Entity registration scenarios

End-to-end registration scenarios cover HTTP submission, outbox admission,
persisted outcomes and reads. The local launcher uses **SQLite**; these tests do
not prove PostgreSQL/MySQL or tenant/PDP behavior.

Each heading maps to `@pytest.mark.scenario("TR-REG-NNN")` in
[test_registration.py](../test_registration.py). Collection rejects unknown IDs;
tests contain complete expected bodies.

The shared execution rules these scenarios rely on — per-test namespaces,
idempotency keys, outcome matching — and the commands that run them are in the
[suite README](../README.md).

## Scenarios

### TR-REG-001 — Create a Type Schema

**Given:** a new dependency-free [person schema](../fixtures/registration/person_schema.json).

**When:** submit this item alone and await completion.

**Then:**

1. The item succeeds at resource version 1.
2. Reading by GTS ID returns the submitted Type Schema and materialized traits.
3. Reading by `gts_uuid` returns the same body and timestamps.

### TR-REG-002 — Register an Instance in a later operation

**Given:** a [schema](../fixtures/registration/person_schema.json) and conforming
[Instance](../fixtures/registration/person_instance.json).

**When:**

1. Submit and complete the schema operation.
2. Submit and complete the Instance under a new key.

**Then:** the Instance succeeds at version 1 and reads back with its exact content;
schema-only artifacts are `null`.

### TR-REG-003 — Register an Instance before its schema in one batch

**Given:** the same schema and Instance, both absent.

**When:** submit `[person_instance, person_schema]` in one batch.

**Then:** both succeed at version 1 and read back with the expected kind and content.

This checks batch-level ordering, not every graph-ordering case.

### TR-REG-004 — Preserve partial success and structured failures

**Given:**

- A: valid, independent [person schema](../fixtures/registration/person_schema.json).
- B: [missing_ref_schema.json](../fixtures/registration/missing_ref_schema.json),
  referencing absent `absent.v1~`.
- C: [blocked_instance.json](../fixtures/registration/blocked_instance.json), an
  Instance conforming to B.

**When:** submit exactly `[C, B, A]` in one request and await completion.

**Then:**

| Item | Status | Resource version | Error reason |
|---|---|---|---|
| A | succeeded | 1 | No error |
| B | failed | null | dependency_not_found |
| C | failed | null | blocked_by_dependency |

B also reports `dependency_kind=ref` and the missing ID. A is readable; B and C
return RFC-9457 `404` responses.

### TR-REG-005 — Refuse a Type Schema whose `$id` does not name its item

**Given:** the [person schema](../fixtures/registration/person_schema.json) and its conforming [Instance](../fixtures/registration/person_instance.json), both absent, with the schema's `content.$id` replaced by one of: absent, a non-string, a malformed URI, or the `gts://` URI of another Type Schema.

**When:** submit `[person_instance, person_schema]` in one request.

**Then:**

1. The request is refused synchronously with `400` RFC-9457 `invalid_argument` naming the schema's `gts_id` as `resource_name`, with one field violation on `entity`, reason `VALIDATION_FAILED`, whose description names the expected `gts://<gts_id>`.
2. No `202`, operation or `Location` is returned, so nothing is admitted.
3. Neither the schema nor its valid batch neighbour, the Instance, is readable; both return `404`.
