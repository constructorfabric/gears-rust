# 01 Cancelled

**Category**: `cancelled`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.cancelled.v1~`
**HTTP Status**: 499 (Client Closed Request)
**Title**: "Cancelled"
**Use When**: The client cancelled the request before the server finished processing.
**Similar Categories**: `deadline_exceeded` — server-side timeout, not client-initiated
**Resource-scoped error**: yes
**Default Message**: "Operation cancelled by the client"

## Context Schema

| Field | Type | Description |
|-------|------|-------------|
| `resource_type` | `String` | GTS type identifier of the associated resource |

## Constructor Example

```rust
use toolkit_canonical_errors::resource_error;

#[resource_error("gts.cf.core.users.user.v1~")]
struct UserResourceError;

let err = UserResourceError::cancelled().create();
```

## JSON Wire — JSON Schema

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "gts://gts.cf.core.errors.err.v1~cf.core.err.cancelled.v1~",
  "type": "object",
  "allOf": [
    { "$ref": "gts://gts.cf.core.errors.err.v1~" },
    {
      "properties": {
        "type": {
          "const": "gts://gts.cf.core.errors.err.v1~cf.core.err.cancelled.v1~"
        },
        "title": { "const": "Cancelled" },
        "status": { "const": 499 },
        "context": {
          "type": "object",
          "required": ["resource_type"],
          "properties": {
            "resource_type": {
              "type": "string",
              "description": "GTS type identifier of the associated resource"
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
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.cancelled.v1~",
  "title": "Cancelled",
  "status": 499,
  "detail": "Operation cancelled by the client",
  "context": {
    "resource_type": "gts.cf.core.users.user.v1~"
  }
}
```
