# 13 Internal

**Category**: `internal`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.internal.v1~`
**HTTP Status**: 500
**Title**: "Internal"
**Context Type**: `DebugInfo`
**Use When**: A known infrastructure failure occurred (database error, serialization bug, etc.). The detail in production is generic; diagnostics are in logs via `trace_id`.
**Similar Categories**: `unknown` — truly unknown error vs known infrastructure failure
**Default Message**: "An internal error occurred. Please retry later."

## Context Schema

| Field | Type | Description |
|-------|------|-------------|
| `message` | `String` | Human-readable debug message (generic in production) |
| `stack_entries` | `Vec<String>` | Stack trace entries (empty in production) |
| `details` | `Option<Object>` | Reserved for derived GTS type extensions (p3+); absent in p1 |

## Rust Definitions and Constructor Example

```rust
use cf_modkit_errors::{CanonicalError, DebugInfo};

// From a database error via ? operator:
let user = db.find_user(&id).await?;  // DbErr auto-converts to CanonicalError::Internal

// Or explicit construction:
let err = CanonicalError::internal(
    DebugInfo::new("Database connection pool exhausted")
);
```

## JSON Wire — JSON Schema

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
  "type": "object",
  "allOf": [
    { "$ref": "gts://gts.cf.core.errors.err.v1~" },
    {
      "properties": {
        "type": {
          "const": "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~"
        },
        "title": { "const": "Internal" },
        "status": { "const": 500 },
        "context": {
          "type": "object",
          "required": ["message", "stack_entries"],
          "properties": {
            "resource_type": {
              "type": "string",
              "description": "GTS type identifier of the associated resource (injected when resource_type is set)"
            },
            "message": {
              "type": "string",
              "description": "Human-readable debug message (generic in production)"
            },
            "stack_entries": {
              "type": "array",
              "items": { "type": "string" },
              "description": "Stack trace entries (empty in production)"
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
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
  "title": "Internal",
  "status": 500,
  "detail": "An internal error occurred. Please retry later.",
  "context": {
    "resource_type": "gts.cf.core.tenants.tenant.v1~",
    "message": "An internal error occurred. Please retry later.",
    "stack_entries": []
  }
}
```
