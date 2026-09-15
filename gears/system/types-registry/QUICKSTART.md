# Types Registry - Quickstart

Stores Global Type System (GTS) Type Schemas and Instances with versioning,
dependency checks and schemas materialized at admission.

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

`/v2` manages global platform entities without tenant ownership.
All gear routes are internal (`exposed = false`) but appear in `/cf/docs`.
Exposing mutations requires platform authentication (`X-ToolKit-Internal-Token` /
`PlatformIdentity`) on a separate listener, followed by a PDP decision before dispatch.

Use the gear's internal base URL:

```bash
BASE="$TYPES_REGISTRY_INTERNAL"
```

## Examples

Examples use `cf`, the only platform vendor allowed by default; others need an `allowed_vendors` policy region.

### Register a Type Schema, then poll the outcome

```bash
curl -s -X POST "$BASE/types-registry/v2/entities" \
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
      }'
```

Response: **202 Accepted**, `Location: …/types-registry/v2/operations/{operation_id}`
and an advisory `Retry-After: 1`. Follow the `Location`:

```bash
curl -s "$BASE/types-registry/v2/operations/$OPERATION_ID" | python3 -m json.tool
```

```json
{
    "operation_id": "34af3e4e-4927-4a98-a028-d4c2fe9edc95",
    "kind": "registration",
    "dry_run": false,
    "status": "completed",
    "items": [
        {
            "gts_id": "gts.cf.core.example.event.v1~",
            "status": "succeeded",
            "resource_version": 1,
            "error": null
        }
    ]
}
```

`completed` means all items are terminal; inspect their outcomes. Read the entity's artifacts and `gts_uuid` Registry Reference:

```bash
curl -s "$BASE/types-registry/v2/entities/gts.cf.core.example.event.v1~" \
  | python3 -m json.tool
```

### Rehearse a deletion, then perform it

Dry run checks preconditions, lifecycle and dependants, and persists a pollable prediction
without changing entities or assigning a `resource_version`:

```bash
curl -s -X DELETE \
  "$BASE/types-registry/v2/entities/gts.cf.core.example.event.v1~?expected_resource_version=1&dry_run=true" \
  -H "Idempotency-Key: rehearse-delete-1"
```

To commit, omit `dry_run` and use a new idempotency key. Reusing the dry-run key returns `409`:

```bash
curl -s -X DELETE \
  "$BASE/types-registry/v2/entities/gts.cf.core.example.event.v1~?expected_resource_version=1" \
  -H "Idempotency-Key: delete-1"
```

The entity remains readable with `"lifecycle_status": "deleted"` and an advanced version.
Same-key replay returns **200**, `Idempotency-Replayed: true`, without deleting twice.

Both deletion routes require a positive `expected_resource_version`. Missing, non-numeric
or zero returns `400`; a mismatch returns `202` then item `precondition_failed`.
`If-Match` is rejected because the version check is asynchronous.

Batch deletion accepts a GTS identifier or Registry Reference in each `key`.
Outcomes use GTS identifiers in request order:

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

Unknown Registry References return `404` (no identifier for an item outcome); absent GTS identifiers fail asynchronously.

`dry_run` defaults to `false`: body field here (`"dry_run": true`), query parameter for single deletion.
