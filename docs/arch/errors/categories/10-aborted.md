# 10 Aborted

**Category**: `aborted`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.aborted.v1~`
**HTTP Status**: 409
**Title**: "Aborted"
**Use When**: The operation was aborted due to a concurrency conflict (optimistic locking failure, transaction conflict). The client can retry.
**Similar Categories**: `already_exists` — duplicate on create vs conflict on update
**Resource-scoped error**: yes
**Default Message**: Same as the `detail` parameter passed to the constructor.

## Context Schema

| Field | Type | Description |
|-------|------|-------------|
| `resource_type` | `String` | GTS type identifier of the associated resource |
| `resource_name` | `Option<String>` | Identifier of the associated resource |
| `reason` | `String` | Machine-readable reason code (e.g., `OPTIMISTIC_LOCK_FAILURE`) |

## Constructor Example

```rust
use toolkit_canonical_errors::resource_error;

#[resource_error("gts.cf.oagw.upstreams.upstream.v1~")]
struct UpstreamResourceError;

let err = UpstreamResourceError::aborted("Operation aborted due to concurrency conflict")
    .with_reason("OPTIMISTIC_LOCK_FAILURE")
    .create();
```

## JSON Wire — JSON Schema

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "gts://gts.cf.core.errors.err.v1~cf.core.err.aborted.v1~",
  "type": "object",
  "allOf": [
    { "$ref": "gts://gts.cf.core.errors.err.v1~" },
    {
      "properties": {
        "type": {
          "const": "gts://gts.cf.core.errors.err.v1~cf.core.err.aborted.v1~"
        },
        "title": { "const": "Aborted" },
        "status": { "const": 409 },
        "context": {
          "type": "object",
          "required": ["resource_type", "reason"],
          "properties": {
            "resource_type": {
              "type": "string",
              "description": "GTS type identifier of the associated resource"
            },
            "resource_name": {
              "type": "string",
              "description": "Identifier of the associated resource (set via with_resource())"
            },
            "reason": {
              "type": "string",
              "description": "Machine-readable reason code (e.g., OPTIMISTIC_LOCK_FAILURE)"
            }
          },
          "additionalProperties": false
        }
      }
    }
  ]
}
```

## JSON Wire — JSON Example

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.aborted.v1~",
  "title": "Aborted",
  "status": 409,
  "detail": "Operation aborted due to concurrency conflict",
  "context": {
    "resource_type": "gts.cf.oagw.upstreams.upstream.v1~",
    "reason": "OPTIMISTIC_LOCK_FAILURE"
  }
}
```
