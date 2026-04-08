# 07 Permission Denied

**Category**: `permission_denied`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~`
**HTTP Status**: 403
**Title**: "Permission Denied"
**Context Type**: `ErrorInfo`
**Use When**: The caller is authenticated but does not have permission for the requested operation.
**Similar Categories**: `unauthenticated` — no valid credentials vs insufficient permissions
**Default Message**: "You do not have permission to perform this operation"

## Context Schema

| Field | Type | Description |
|-------|------|-------------|
| `resource_type` | `Option<String>` | Transport-injected resource GTS type identifier when provided by the canonical error wrapper |
| `reason` | `String` | Machine-readable reason code (e.g., `CROSS_TENANT_ACCESS`, `SCOPE_INSUFFICIENT`) |
| `domain` | `String` | Logical grouping (e.g., `"auth.cyberfabric.io"`) |
| `metadata` | `HashMap<String, String>` | Arbitrary key-value pairs for additional context |
| `details` | `Option<Object>` | Reserved for derived GTS type extensions (p3+); absent in p1 |

> Note: In Rust, `resource_type` is carried on `CanonicalError::PermissionDenied` as an envelope field, not inside `ErrorInfo`. It is injected into the wire `context` object during mapping to `Problem` via `Problem::from_error`. It is not part of the `ErrorInfo` GTS type (`gts.cf.core.errors.error_info.v1~`).

## Constructor Example

```rust
use cf_modkit_errors::{CanonicalError, ErrorInfo};

let err = CanonicalError::permission_denied(
    ErrorInfo::new("CROSS_TENANT_ACCESS", "auth.cyberfabric.io")
);
```

## JSON Wire — JSON Schema

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
  "type": "object",
  "allOf": [
    { "$ref": "gts://gts.cf.core.errors.err.v1~" },
    {
      "properties": {
        "type": {
          "const": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~"
        },
        "title": { "const": "Permission Denied" },
        "status": { "const": 403 },
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
              "description": "Machine-readable reason code (e.g., CROSS_TENANT_ACCESS, SCOPE_INSUFFICIENT)"
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
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
  "title": "Permission Denied",
  "status": 403,
  "detail": "You do not have permission to perform this operation",
  "context": {
    "resource_type": "gts.cf.core.tenants.tenant.v1~",
    "reason": "CROSS_TENANT_ACCESS",
    "domain": "auth.cyberfabric.io",
    "metadata": {}
  }
}
```
