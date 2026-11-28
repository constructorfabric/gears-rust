# 15 Data Loss

**Category**: `data_loss`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.data_loss.v1~`
**HTTP Status**: 500
**Title**: "Data Loss"
**Use When**: Unrecoverable data loss or corruption detected.
**Similar Categories**: `internal` — transient infrastructure failure vs permanent data loss
**Resource-scoped error**: yes
**Default Message**: Same as the `detail` parameter passed to the constructor.

## Context Schema

| Field | Type | Description |
|-------|------|-------------|
| `resource_type` | `String` | GTS type identifier of the affected resource |
| `resource_name` | `String` | Identifier of the affected resource |

## Constructor Example

```rust
use toolkit_canonical_errors::resource_error;

#[resource_error("gts.cf.core.files.file.v1~")]
struct FileResourceError;

let err = FileResourceError::data_loss("Data loss detected")
    .with_resource("01JFILE-ABC")
    .create();
```

## JSON Wire — JSON Schema

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "gts://gts.cf.core.errors.err.v1~cf.core.err.data_loss.v1~",
  "type": "object",
  "allOf": [
    { "$ref": "gts://gts.cf.core.errors.err.v1~" },
    {
      "properties": {
        "type": {
          "const": "gts://gts.cf.core.errors.err.v1~cf.core.err.data_loss.v1~"
        },
        "title": { "const": "Data Loss" },
        "status": { "const": 500 },
        "context": {
          "type": "object",
          "required": ["resource_type", "resource_name"],
          "properties": {
            "resource_type": {
              "type": "string",
              "description": "GTS type identifier of the affected resource"
            },
            "resource_name": {
              "type": "string",
              "description": "Identifier of the affected resource"
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
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.data_loss.v1~",
  "title": "Data Loss",
  "status": 500,
  "detail": "Data loss detected",
  "context": {
    "resource_type": "gts.cf.core.files.file.v1~",
    "resource_name": "01JFILE-ABC"
  }
}
```
