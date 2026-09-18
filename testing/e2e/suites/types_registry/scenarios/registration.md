# Entity registration scenarios

These scenarios cover entity registration end to end on the implemented P0
async admission API: real HTTP, outbox dispatch, worker admission, persisted
outcomes and entity reads. The local launcher currently binds Types Registry to
**SQLite**; this run does not establish PostgreSQL/MySQL conformance or
tenant/PDP authorization.

Each scenario below is implemented by one test marked
`@pytest.mark.scenario("TR-REG-NNN")` in [test_registration.py](../test_registration.py).
Collection rejects a marker whose ID is not a heading here, and the terminal
summary prints the real outcome per ID — an unimplemented scenario shows as
`no test collected`. Complete expected bodies live in the tests as inline Python
dictionaries; this document states intent, not payloads.

The shared execution rules these scenarios rely on — per-test namespaces,
idempotency keys, outcome matching — and the commands that run them are in the
[suite README](../README.md).

## Scenarios

### TR-REG-001 — Create a Type Schema

**Given:** [person_schema.json](../fixtures/registration/person_schema.json), a new
dependency-free Draft-07 object schema requiring a string `name`.

**When:** submit this item alone and await completion.

**Then:**

1. The sole item succeeds with resource version 1 and no error.
2. Reading the entity by GTS ID returns the submitted content and
   `kind=type_schema`, with `effective_traits={}` and a complete
   `effective_traits_schema`.
3. Reading by the returned `gts_uuid` returns exactly the same body, original
   timestamps included.

### TR-REG-002 — Register an Instance in a later operation

**Given:** [person_schema.json](../fixtures/registration/person_schema.json) and
[person_instance.json](../fixtures/registration/person_instance.json). The Instance
names that schema as its conforming type; its value is `{"name": "Alice"}`.

**When:**

1. Submit the schema alone and await its successful completion. Separate
   operations have no ordering guarantee, so completion of the prerequisite —
   not merely its acceptance — is required before step 2.
2. Submit the Instance alone under a new key, then await completion.

**Then:** the sole Instance item succeeds with resource version 1. Reading it
returns `kind=instance` and the exact fixture content, with `resolved_schema`,
`effective_traits` and `effective_traits_schema` all JSON `null`.

### TR-REG-003 — Register an Instance before its schema in one batch

**Given:** the same [schema](../fixtures/registration/person_schema.json) and
[Instance](../fixtures/registration/person_instance.json), both absent in a fresh
test namespace.

**When:** submit exactly `[person_instance, person_schema]` in one request and
await completion. Do not pre-register the schema.

**Then:** there are exactly two outcomes, both `succeeded` with resource
version 1, and both entities read back with their expected kinds and fixture
contents.

This is a representative check that a batch is admitted as a unit regardless of
item order, not an exhaustive test of the graph-ordering algorithm.

### TR-REG-004 — Preserve partial success and structured failures

**Given:**

- A: [person_schema.json](../fixtures/registration/person_schema.json), independent
  and valid.
- B: [missing_ref_schema.json](../fixtures/registration/missing_ref_schema.json),
  whose `allOf.$ref` names `absent.v1~` in this test's namespace. That target is
  neither stored nor included in the batch.
- C: [blocked_instance.json](../fixtures/registration/blocked_instance.json), an
  Instance whose conforming Type Schema is B.

**When:** submit exactly `[C, B, A]` in one request and await completion.

**Then:**

| Item | Status | Resource version | Error reason |
|---|---|---|---|
| A | succeeded | 1 | No error |
| B | failed | null | dependency_not_found |
| C | failed | null | blocked_by_dependency |

B's error also carries `dependency_kind=ref` and a `dependency_id` equal to the
missing target from its fixture. A reads back with the submitted content; B and
C each return HTTP 404 with `application/problem+json` and body `status=404`.
