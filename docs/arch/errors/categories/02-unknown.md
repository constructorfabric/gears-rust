# 02 Unknown

**Category**: `unknown`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.unknown.v1~`
**HTTP Status**: 500
**Title**: "Unknown"
**Use When**: An error occurred that does not match any other canonical category. Prefer a more specific category when possible.
**Similar Categories**: `internal` — known infrastructure failure vs truly unknown error
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

let err = UserResourceError::unknown("Unexpected response from payment provider").create();
```

## JSON Wire — JSON Schema

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "gts://gts.cf.core.errors.err.v1~cf.core.err.unknown.v1~",
  "type": "object",
  "allOf": [
    { "$ref": "gts://gts.cf.core.errors.err.v1~" },
    {
      "properties": {
        "type": {
          "const": "gts://gts.cf.core.errors.err.v1~cf.core.err.unknown.v1~"
        },
        "title": { "const": "Unknown" },
        "status": { "const": 500 },
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
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.unknown.v1~",
  "title": "Unknown",
  "status": 500,
  "detail": "Unexpected response from payment provider",
  "context": {
    "resource_type": "gts.cf.core.users.user.v1~"
  }
}
```
