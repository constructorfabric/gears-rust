# 04 Deadline Exceeded

**Category**: `deadline_exceeded`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.deadline_exceeded.v1~`
**HTTP Status**: 504
**Title**: "Deadline Exceeded"
**Context Type**: `RequestInfo`
**Use When**: The server did not complete the operation within the allowed time.
**Similar Categories**: `cancelled` — client-initiated cancellation, not server-side timeout
**Default Message**: "Operation did not complete within the allowed time"

## Context Schema

| Field | Type | Description |
|-------|------|-------------|
| `request_id` | `String` | Identifier of the timed-out request |
| `details` | `Option<Object>` | Reserved for derived GTS type extensions (p3+); absent in p1 |

## Constructor Example

```rust
use cf_modkit_errors::{CanonicalError, RequestInfo};

let err = CanonicalError::deadline_exceeded(
    RequestInfo { request_id: "01JREQ-ABC".to_string() }
);
```

## JSON Wire — JSON Schema

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "gts://gts.cf.core.errors.err.v1~cf.core.err.deadline_exceeded.v1~",
  "type": "object",
  "allOf": [
    { "$ref": "gts://gts.cf.core.errors.err.v1~" },
    {
      "properties": {
        "type": {
          "const": "gts://gts.cf.core.errors.err.v1~cf.core.err.deadline_exceeded.v1~"
        },
        "title": { "const": "Deadline Exceeded" },
        "status": { "const": 504 },
        "context": {
          "type": "object",
          "required": ["request_id"],
          "properties": {
            "resource_type": {
              "type": "string",
              "description": "GTS type identifier of the associated resource (injected when resource_type is set)"
            },
            "request_id": {
              "type": "string",
              "description": "Identifier of the timed-out request"
            },
            "details": {
              "type": ["object", "null"],
              "description": "Reserved for derived GTS type extensions (p3+); absent in p1"
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
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.deadline_exceeded.v1~",
  "title": "Deadline Exceeded",
  "status": 504,
  "detail": "Operation did not complete within the allowed time",
  "context": {
    "resource_type": "gts.cf.core.users.user.v1~",
    "request_id": "01JREQ-ABC"
  }
}
```
