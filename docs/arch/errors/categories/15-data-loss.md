# 15 Data Loss

**Category**: `data_loss`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.data_loss.v1~`
**HTTP Status**: 500
**Title**: "Data Loss"
**Context Type**: `ResourceInfo`
**Use When**: Unrecoverable data loss or corruption detected.
**Similar Categories**: `internal` — transient infrastructure failure vs permanent data loss
**Default Message**: "Data loss detected"

## Context Schema

| Field | Type | Description |
|-------|------|-------------|
| `resource_type` | `String` | GTS type identifier of the affected resource |
| `resource_name` | `String` | Identifier of the affected resource |
| `description` | `String` | Human-readable explanation |
| `details` | `Option<Object>` | Reserved for derived GTS type extensions (p3+); absent in p1 |

## Constructor Example

```rust
use cf_modkit_errors::{CanonicalError, ResourceInfo};

let err = CanonicalError::data_loss(
    ResourceInfo::new("gts.cf.core.files.file.v1~", "01JFILE-ABC")
        .with_description("Checksum mismatch detected")
);

// Or via resource-scoped macro:
// FileResourceError::data_loss("01JFILE-ABC")
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
          "required": ["resource_type", "resource_name", "description"],
          "properties": {
            "resource_type": {
              "type": "string",
              "description": "GTS type identifier of the affected resource"
            },
            "resource_name": {
              "type": "string",
              "description": "Identifier of the affected resource"
            },
            "description": {
              "type": "string",
              "description": "Human-readable explanation"
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
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.data_loss.v1~",
  "title": "Data Loss",
  "status": 500,
  "detail": "Data loss detected",
  "context": {
    "resource_type": "gts.cf.core.files.file.v1~",
    "resource_name": "01JFILE-ABC",
    "description": "Data loss detected"
  }
}
```
