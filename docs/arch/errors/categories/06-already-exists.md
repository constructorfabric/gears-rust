# 06 Already Exists

**Category**: `already_exists`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.already_exists.v1~`
**HTTP Status**: 409
**Title**: "Already Exists"
**Context Type**: `ResourceInfo`
**Use When**: The resource the client tried to create already exists.
**Similar Categories**: `aborted` — concurrency conflict on update vs duplicate on create
**Default Message**: "Resource already exists"

## Context Schema

| Field | Type | Description |
|-------|------|-------------|
| `resource_type` | `String` | GTS type identifier of the resource |
| `resource_name` | `String` | Identifier of the duplicate resource |
| `description` | `String` | Human-readable explanation |
| `details` | `Option<Object>` | Reserved for derived GTS type extensions (p3+); absent in p1 |

## Constructor Example

```rust
use cf_modkit_errors::{CanonicalError, ResourceInfo};

let err = CanonicalError::already_exists(
    ResourceInfo::new("gts.cf.core.users.user.v1~", "alice@example.com")
        .with_description("User with this email already exists")
);
```

## JSON Wire — JSON Schema

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "gts://gts.cf.core.errors.err.v1~cf.core.err.already_exists.v1~",
  "type": "object",
  "allOf": [
    { "$ref": "gts://gts.cf.core.errors.err.v1~" },
    {
      "properties": {
        "type": {
          "const": "gts://gts.cf.core.errors.err.v1~cf.core.err.already_exists.v1~"
        },
        "title": { "const": "Already Exists" },
        "status": { "const": 409 },
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
              "description": "Identifier of the duplicate resource"
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
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.already_exists.v1~",
  "title": "Already Exists",
  "status": 409,
  "detail": "Resource already exists",
  "context": {
    "resource_type": "gts.cf.core.users.user.v1~",
    "resource_name": "alice@example.com",
    "description": "Resource already exists"
  }
}
```
