# Types Registry - Quickstart

Stores versioned GTS Type Schemas and Instances, validates dependencies and
materializes schemas during admission.

Features:

- Asynchronous writes: submit, then poll per-entity outcomes
- Optimistic concurrency via `expected_resource_version`
- Single/batch deletion, blocked by live direct registered dependants
- Dry run on every mutation; entity state stays unchanged
- Required `Idempotency-Key`; replay returns the same operation, changed content conflicts
- Reads by GTS identifier or Registry Reference UUID, including tombstones

Full API documentation: <http://127.0.0.1:8087/cf/docs>

## Surfaces

| Base path | What it is |
|---|---|
| `/types-registry/v1` | Legacy in-memory API; removed after consumer migration |
| `/types-registry/v2` | Database-backed async API below; promoted to `/v1` after migration |

`/v2` manages global platform entities. Routes are internal (`exposed = false`)
but appear in `/cf/docs`. External mutation access requires platform
authentication and PDP authorization before dispatch.

Use the gear's internal base URL:

```bash
BASE="$TYPES_REGISTRY_INTERNAL"
```

## Examples

Examples use `cf`, the only vendor allowed by default.

### Register a Type Schema, then poll the outcome

```bash
RECEIPT=$(curl -s -X POST "$BASE/types-registry/v2/entities" \
  -H "Idempotency-Key: register-example-event-1" \
  -H "Content-Type: application/json" \
  -d '{
        "items": [{
          "gts_id": "gts.cf.core.example.event.v1~",
          "content": {
            "$id": "gts://gts.cf.core.example.event.v1~",
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": { "email": { "type": "string" } }
          }
        }]
      }')
```

The **202 Accepted** response includes `Location`, `Retry-After: 1` and the operation ID:

```bash
OPERATION_ID=$(printf '%s' "$RECEIPT" |
  python3 -c 'import json,sys; print(json.load(sys.stdin)["operation_id"])')
```

Follow the `Location`:

```bash
curl -s "$BASE/types-registry/v2/operations/$OPERATION_ID" | python3 -m json.tool
```

The completed response contains a `succeeded` item at `resource_version: 1`.

`completed` means every item is terminal. Read the entity and its `gts_uuid`:

```bash
curl -s "$BASE/types-registry/v2/entities/gts.cf.core.example.event.v1~" \
  | python3 -m json.tool
```

### Rehearse a deletion, then perform it

Dry run persists a pollable prediction without changing entities:

```bash
curl -s -X DELETE \
  "$BASE/types-registry/v2/entities/gts.cf.core.example.event.v1~?expected_resource_version=1&dry_run=true" \
  -H "Idempotency-Key: rehearse-delete-1"
```

To commit, omit `dry_run` and use a new key; reusing the dry-run key returns `409`:

```bash
curl -s -X DELETE \
  "$BASE/types-registry/v2/entities/gts.cf.core.example.event.v1~?expected_resource_version=1" \
  -H "Idempotency-Key: delete-1"
```

The tombstone remains readable with an incremented version. Same-key replay returns
**200** and `Idempotency-Replayed: true`.

Deletion requires a positive `expected_resource_version`. Invalid values return
`400`; mismatches become asynchronous `precondition_failed` outcomes. `If-Match`
is rejected.

Batch deletion accepts either key form and returns GTS-ID outcomes in request order:

```bash
curl -s -X POST "$BASE/types-registry/v2/entities:batchDelete" \
  -H "Idempotency-Key: delete-batch-1" \
  -H "Content-Type: application/json" \
  -d '{
        "items": [
          { "key": "gts.cf.core.example.event.v1~", "expected_resource_version": 1 },
          { "key": "d226dd5b-14c8-56da-a718-9cf29becaba1", "expected_resource_version": 2 }
        ]
      }'
```

Unknown Registry References return `404`; absent GTS IDs fail asynchronously.

`dry_run` defaults to `false`: a body field for batches and query parameter for single deletion.
