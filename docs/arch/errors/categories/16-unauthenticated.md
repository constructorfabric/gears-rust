# 16 Unauthenticated

**Category**: `unauthenticated`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~`
**HTTP Status**: 401
**Title**: "Unauthenticated"
**Use When**: The request does not have valid authentication credentials.
**Similar Categories**: `permission_denied` — authenticated but insufficient permissions vs no valid credentials
**Resource-scoped error**: no
**Default Message**: "Authentication required"

## Context Schema

GTS schema ID: `gts.cf.core.errors.error_info.v1~`

| Field | Type | Description |
|-------|------|-------------|
| `reason` | `String` | Machine-readable reason code (e.g., `TOKEN_EXPIRED`, `MISSING_CREDENTIALS`) |
| `extra` | `Option<Object>` | Reserved for derived GTS type extensions (p3+); absent in p1 |

## Constructor Example

```rust
use toolkit_canonical_errors::CanonicalError;

let err = CanonicalError::unauthenticated()
    .with_reason("TOKEN_EXPIRED")
    .create();
```

## JSON Wire — JSON Schema

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "gts://gts.cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~",
  "type": "object",
  "allOf": [
    { "$ref": "gts://gts.cf.core.errors.err.v1~" },
    {
      "properties": {
        "type": {
          "const": "gts://gts.cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~"
        },
        "title": { "const": "Unauthenticated" },
        "status": { "const": 401 },
        "context": {
          "type": "object",
          "required": ["reason"],
          "properties": {
            "reason": {
              "type": "string",
              "description": "Machine-readable reason code (e.g., TOKEN_EXPIRED, MISSING_CREDENTIALS)"
            },
            "extra": {
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
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~",
  "title": "Unauthenticated",
  "status": 401,
  "detail": "Authentication required",
  "context": {
    "reason": "TOKEN_EXPIRED"
  }
}
```
