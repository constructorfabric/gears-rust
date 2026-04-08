# 05 Not Found

**Category**: `not_found`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~`
**HTTP Status**: 404
**Title**: "Not Found"
**Context Type**: `ResourceInfo`
**Use When**: The requested resource does not exist or was filtered out by access controls.
**Similar Categories**: `permission_denied` — resource exists but caller lacks access; use `not_found` for DB-filtered 404 to avoid information leakage
**Default Message**: "Resource not found"

## Context Schema

| Field | Type | Description |
|-------|------|-------------|
| `resource_type` | `String` | GTS type identifier of the resource |
| `resource_name` | `String` | Identifier of the missing resource |
| `description` | `String` | Human-readable explanation |
| `details` | `Option<Object>` | Reserved for derived GTS type extensions (p3+); absent in p1 |

## Constructor Example

```rust
use cf_modkit_errors::{CanonicalError, ResourceInfo};

// Direct construction:
let err = CanonicalError::not_found(
    ResourceInfo::new("gts.cf.core.users.user.v1~", "user-123")
);

// Or via resource-scoped macro:
#[resource_error("gts.cf.core.users.user.v1~")]
struct UserResourceError;

let err = UserResourceError::not_found("user-123");
// Auto-creates ResourceInfo and sets resource_type
```

## JSON Wire — JSON Schema

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
  "type": "object",
  "allOf": [
    { "$ref": "gts://gts.cf.core.errors.err.v1~" },
    {
      "properties": {
        "type": {
          "const": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~"
        },
        "title": { "const": "Not Found" },
        "status": { "const": 404 },
        "context": {
          "type": "object",
          "required": ["resource_type", "resource_name", "description"],
          "properties": {
            "resource_type": {
              "type": "string",
              "description": "GTS type identifier of the resource"
            },
            "resource_name": {
              "type": "string",
              "description": "Identifier of the missing resource"
            },
            "description": {
              "type": "string",
              "description": "Human-readable explanation"
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
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
  "title": "Not Found",
  "status": 404,
  "detail": "Resource not found",
  "context": {
    "resource_type": "gts.cf.core.users.user.v1~",
    "resource_name": "user-123",
    "description": "Resource not found"
  }
}
```
