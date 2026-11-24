# 12 Unimplemented

**Category**: `unimplemented`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.unimplemented.v1~`
**HTTP Status**: 501
**Title**: "Unimplemented"
**Use When**: The requested operation is recognized but not implemented (e.g., a planned feature, an unsupported protocol variant).
**Similar Categories**: `internal` — bug vs intentionally unimplemented
**Resource-scoped error**: yes
**Default Message**: Same as the `detail` parameter passed to the constructor.

## Context Schema

| Field | Type | Description |
|-------|------|-------------|
| `resource_type` | `String` | GTS type identifier of the associated resource |
| `resource_name` | `Option<String>` | Identifier of the associated resource |

## Constructor Example

```rust
use toolkit_canonical_errors::resource_error;

#[resource_error("gts.cf.core.users.user.v1~")]
struct UserResourceError;

let err = UserResourceError::unimplemented("This operation is not implemented").create();
```

## JSON Wire — JSON Schema

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "gts://gts.cf.core.errors.err.v1~cf.core.err.unimplemented.v1~",
  "type": "object",
  "allOf": [
    { "$ref": "gts://gts.cf.core.errors.err.v1~" },
    {
      "properties": {
        "type": {
          "const": "gts://gts.cf.core.errors.err.v1~cf.core.err.unimplemented.v1~"
        },
        "title": { "const": "Unimplemented" },
        "status": { "const": 501 },
        "context": {
          "type": "object",
          "required": ["resource_type"],
          "properties": {
            "resource_type": {
              "type": "string",
              "description": "GTS type identifier of the associated resource"
            },
            "resource_name": {
              "type": "string",
              "description": "Identifier of the associated resource (set via with_resource())"
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
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.unimplemented.v1~",
  "title": "Unimplemented",
  "status": 501,
  "detail": "This operation is not implemented",
  "context": {
    "resource_type": "gts.cf.core.users.user.v1~"
  }
}
```
