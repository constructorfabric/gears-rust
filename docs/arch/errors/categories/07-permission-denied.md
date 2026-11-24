# 07 Permission Denied

**Category**: `permission_denied`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~`
**HTTP Status**: 403
**Title**: "Permission Denied"
**Use When**: The caller is authenticated but does not have permission for the requested operation.
**Similar Categories**: `unauthenticated` — no valid credentials vs insufficient permissions
**Resource-scoped error**: yes
**Default Message**: "You do not have permission to perform this operation"

## Context Schema

| Field | Type | Description |
|-------|------|-------------|
| `resource_type` | `String` | Transport-injected resource GTS type identifier when provided by the canonical error wrapper |
| `reason` | `String` | Machine-readable reason code (e.g., `CROSS_TENANT_ACCESS`) |

> Note: In Rust, `resource_type` is carried on `CanonicalError::PermissionDenied` as an envelope field, not inside `PermissionDeniedV1`. It is injected into the wire `context` object during mapping to `Problem` via `Problem::from_error`.

## Constructor Example

```rust
use toolkit_canonical_errors::resource_error;

#[resource_error("gts.cf.core.tenants.tenant.v1~")]
struct TenantResourceError;

let err = TenantResourceError::permission_denied()
    .with_reason("CROSS_TENANT_ACCESS")
    .create();
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
          "required": ["resource_type", "reason"],
          "properties": {
            "resource_type": {
              "type": "string",
              "description": "GTS type identifier of the resource"
            },
            "reason": {
              "type": "string",
              "description": "Machine-readable reason code (e.g., CROSS_TENANT_ACCESS)"
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
    "reason": "CROSS_TENANT_ACCESS"
  }
}
```
