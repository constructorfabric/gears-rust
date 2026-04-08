# 11 Out of Range

**Category**: `out_of_range`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.out_of_range.v1~`
**HTTP Status**: 400
**Title**: "Out of Range"
**Context Type**: `Validation`
**Use When**: A value is syntactically valid but outside the acceptable range (e.g., page number beyond last page, negative quantity).
**Similar Categories**: `invalid_argument` — bad format vs valid format but out of range
**Default Message**: "Value out of range" (FieldViolations) or the format/constraint string

## Context Schema

**Variant: FieldViolations**

Violations:

| Field | Type | Description |
|-------|------|-------------|
| `field_violations` | `Vec<FieldViolation>` | List of per-field out-of-range errors |
| `details` | `Option<Object>` | Reserved for derived GTS type extensions (p3+); absent in p1 |

Field violation:

| Field | Type | Description |
|-------|------|-------------|
| `field` | `String` | Field path (e.g., `"page"`, `"quantity"`) |
| `description` | `String` | Human-readable explanation |
| `reason` | `String` | Machine-readable reason code (e.g., `OUT_OF_RANGE`) |

**Variant: Format** — `{ "format": "<message>", "details": null }`

**Variant: Constraint** — `{ "constraint": "<message>", "details": null }`

## Constructor Example

```rust
use cf_modkit_errors::{CanonicalError, Validation};

let err = CanonicalError::out_of_range(
    Validation::constraint("Page 50 is beyond the last page (12)")
);
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
          "oneOf": [
            {
              "type": "object",
              "required": ["field_violations"],
              "properties": {
                "resource_type": { "type": "string" },
                "field_violations": {
                  "type": "array",
                  "items": { "$ref": "#/$defs/FieldViolation" }
                },
                "details": { "type": ["object", "null"] }
              },
              "additionalProperties": false
            },
            {
              "type": "object",
              "required": ["format"],
              "properties": {
                "resource_type": { "type": "string" },
                "format": { "type": "string" },
                "details": { "type": ["object", "null"] }
              },
              "additionalProperties": false
            },
            {
              "type": "object",
              "required": ["constraint"],
              "properties": {
                "resource_type": { "type": "string" },
                "constraint": { "type": "string" },
                "details": { "type": ["object", "null"] }
              },
              "additionalProperties": false
            }
          ]
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
  "detail": "Page 50 is beyond the last page (12)",
  "context": {
    "resource_type": "gts.cf.core.users.user.v1~",
    "constraint": "Page 50 is beyond the last page (12)"
  }
}
```
