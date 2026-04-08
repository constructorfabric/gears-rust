# 16 Unauthenticated

**Category**: `unauthenticated`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~`
**HTTP Status**: 401
**Title**: "Unauthenticated"
**Context Type**: `ErrorInfo`
**Use When**: The request does not have valid authentication credentials.
**Similar Categories**: `permission_denied` — authenticated but insufficient permissions vs no valid credentials
**Default Message**: "Authentication required"

## Context Schema

GTS schema ID: `gts.cf.core.errors.error_info.v1~`

| Field | Type | Description |
|-------|------|-------------|
| `reason` | `String` | Machine-readable reason code (e.g., `TOKEN_EXPIRED`, `MISSING_CREDENTIALS`) |
| `domain` | `String` | Logical grouping (e.g., `"auth.cyberfabric.io"`) |
| `metadata` | `HashMap<String, String>` | Arbitrary key-value pairs for additional context |
| `details` | `Option<Object>` | Reserved for derived GTS type extensions (p3+); absent in p1 |

## Rust Definitions and Constructor Example

```rust
use cf_modkit_errors::{CanonicalError, ErrorInfo};
use std::collections::HashMap;

let mut metadata = HashMap::new();
metadata.insert("expires_at".to_string(), "2026-02-25T10:00:00Z".to_string());

let err = CanonicalError::unauthenticated(
    ErrorInfo::new("TOKEN_EXPIRED", "auth.cyberfabric.io")
        .with_metadata(metadata)
);
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
          "required": ["reason", "domain", "metadata"],
          "properties": {
            "resource_type": {
              "type": "string",
              "description": "GTS type identifier of the associated resource (injected when resource_type is set)"
            },
            "reason": {
              "type": "string",
              "description": "Machine-readable reason code (e.g., TOKEN_EXPIRED, MISSING_CREDENTIALS)"
            },
            "domain": {
              "type": "string",
              "description": "Logical grouping (e.g., auth.cyberfabric.io)"
            },
            "metadata": {
              "type": "object",
              "additionalProperties": { "type": "string" },
              "description": "Arbitrary key-value pairs for additional context"
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
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~",
  "title": "Unauthenticated",
  "status": 401,
  "detail": "Authentication required",
  "context": {
    "reason": "TOKEN_EXPIRED",
    "domain": "auth.cyberfabric.io",
    "metadata": {
      "expires_at": "2026-02-25T10:00:00Z"
    }
  }
}
```
