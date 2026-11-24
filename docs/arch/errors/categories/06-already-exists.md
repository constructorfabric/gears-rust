# 06 Already Exists

**Category**: `already_exists`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.already_exists.v1~`
**HTTP Status**: 409
**Title**: "Already Exists"
**Use When**: The resource the client tried to create already exists.
**Similar Categories**: `aborted` — concurrency conflict on update vs duplicate on create
**Resource-scoped error**: yes
**Default Message**: Same as the `detail` parameter passed to the constructor.

## Context Schema

| Field | Type | Description |
|-------|------|-------------|
| `resource_type` | `String` | GTS type identifier of the resource |
| `resource_name` | `String` | Identifier of the duplicate resource |

## Constructor Example

```rust
use toolkit_canonical_errors::resource_error;

#[resource_error("gts.cf.core.users.user.v1~")]
struct UserResourceError;

let err = UserResourceError::already_exists("User already exists")
    .with_resource("alice@example.com")
    .create();
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
          "required": ["resource_type", "resource_name"],
          "properties": {
            "resource_type": {
              "type": "string",
              "description": "GTS type identifier of the resource"
            },
            "resource_name": {
              "type": "string",
              "description": "Identifier of the duplicate resource"
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
  "detail": "User already exists",
  "context": {
    "resource_type": "gts.cf.core.users.user.v1~",
    "resource_name": "alice@example.com"
  }
}
```
