# 11 Out of Range

**Category**: `out_of_range`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.out_of_range.v1~`
**HTTP Status**: 400
**Title**: "Out of Range"
**Use When**: A value is syntactically valid but outside the acceptable range (e.g., age beyond allowed maximum, negative quantity).
**Similar Categories**: `invalid_argument` — bad format vs valid format but out of range
**Resource-scoped error**: yes
**Default Message**: Same as the `detail` parameter passed to the constructor.

## Context Schema

| Field | Type | Description |
|-------|------|-------------|
| `resource_type` | `String` | GTS type identifier of the associated resource |
| `resource_name` | `Option<String>` | Identifier of the associated resource |
| `field_violations` | `Vec<FieldViolation>` | List of per-field out-of-range errors |

Field violation:

| Field | Type | Description |
|-------|------|-------------|
| `field` | `String` | Field path (e.g., `"age"`, `"quantity"`) |
| `description` | `String` | Human-readable explanation |
| `reason` | `String` | Machine-readable reason code (e.g., `OUT_OF_RANGE`) |

## Constructor Example

```rust
use toolkit_canonical_errors::resource_error;

#[resource_error("gts.cf.library.books.book.v1~")]
struct BookResourceError;

let err = BookResourceError::out_of_range("Page out of range")
    .with_field_violation(
        "page",
        "Page 50 is beyond the last page (12)",
        "OUT_OF_RANGE",
    )
    .create();
```

## JSON Wire — JSON Schema

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "gts://gts.cf.core.errors.err.v1~cf.core.err.out_of_range.v1~",
  "type": "object",
  "allOf": [
    { "$ref": "gts://gts.cf.core.errors.err.v1~" },
    {
      "properties": {
        "type": {
          "const": "gts://gts.cf.core.errors.err.v1~cf.core.err.out_of_range.v1~"
        },
        "title": { "const": "Out of Range" },
        "status": { "const": 400 },
        "context": {
          "type": "object",
          "required": ["resource_type", "field_violations"],
          "properties": {
            "resource_type": { "type": "string" },
            "resource_name": { "type": "string" },
            "field_violations": {
              "type": "array",
              "items": { "$ref": "#/$defs/FieldViolation" }
            }
          },
          "additionalProperties": false
        }
      }
    }
  ],
  "$defs": {
    "FieldViolation": {
      "type": "object",
      "required": ["field", "description", "reason"],
      "properties": {
        "field": { "type": "string" },
        "description": { "type": "string" },
        "reason": { "type": "string" }
      },
      "additionalProperties": false
    }
  }
}
```

## JSON Wire — JSON Example

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.out_of_range.v1~",
  "title": "Out of Range",
  "status": 400,
  "detail": "Page out of range",
  "context": {
    "resource_type": "gts.cf.library.books.book.v1~",
    "field_violations": [
      {
        "field": "page",
        "description": "Page 50 is beyond the last page (12)",
        "reason": "OUT_OF_RANGE"
      }
    ]
  }
}
```
