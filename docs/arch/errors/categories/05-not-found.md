# 05 Not Found

**Category**: `not_found`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~`
**HTTP Status**: 404
**Title**: "Not Found"
**Use When**: The requested resource does not exist or was filtered out by access controls.
**Similar Categories**: `permission_denied` — resource exists but caller lacks access; use `not_found` for DB-filtered 404 to avoid information leakage
**Resource-scoped error**: yes
**Default Message**: Same as the `detail` parameter passed to the constructor.

## Context Schema

| Field | Type | Description |
|-------|------|-------------|
| `resource_type` | `String` | GTS type identifier of the resource |
| `resource_name` | `String` | Identifier of the missing resource |

## Constructor Example

```rust
use toolkit_canonical_errors::resource_error;

#[resource_error("gts.cf.core.users.user.v1~")]
struct UserResourceError;

let err = UserResourceError::not_found("User not found")
    .with_resource("user-123")
    .create();
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
          "required": ["resource_type", "resource_name"],
          "properties": {
            "resource_type": {
              "type": "string",
              "description": "GTS type identifier of the resource"
            },
            "resource_name": {
              "type": "string",
              "description": "Identifier of the missing resource"
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
  "detail": "User not found",
  "context": {
    "resource_type": "gts.cf.core.users.user.v1~",
    "resource_name": "user-123"
  }
}
```
