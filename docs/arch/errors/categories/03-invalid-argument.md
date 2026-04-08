# 03 Invalid Argument

**Category**: `invalid_argument`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~`
**HTTP Status**: 400
**Title**: "Invalid Argument"
**Context Type**: `Validation`
**Use When**: The client sent an invalid request — malformed fields, bad format, or constraint violations. Independent of system state.
**Similar Categories**: `out_of_range` — value is valid format but outside acceptable range; `failed_precondition` — request is valid but system state prevents it
**Default Message**: "Request validation failed" (FieldViolations) or the format/constraint string

## Context Schema

**Variant: FieldViolations**

Validation:

| Field | Type | Description |
|-------|------|-------------|
| `field_violations` | `Vec<FieldViolation>` | List of per-field validation errors |
| `details` | `Option<Object>` | Reserved for derived GTS type extensions (p3+); absent in p1 |

Field violation:

| Field | Type | Description |
|-------|------|-------------|
| `field` | `String` | Field path (e.g., `"email"`, `"address.zip"`) |
| `description` | `String` | Human-readable explanation |
| `reason` | `String` | Machine-readable reason code (`REQUIRED`, `INVALID_FORMAT`, etc.) |

**Variant: Format** — `{ "format": "<message>", "details": null }`

**Variant: Constraint** — `{ "constraint": "<message>", "details": null }`

## Constructor Example

```rust
use cf_modkit_errors::{CanonicalError, Validation, FieldViolation};

// Field violations:
let err = CanonicalError::invalid_argument(
    Validation::fields(vec![
        FieldViolation::new("email", "must be a valid email address", "INVALID_FORMAT"),
        FieldViolation::new("age", "must be at least 18", "OUT_OF_RANGE"),
    ])
);

// Or format error:
let err = CanonicalError::invalid_argument(
    Validation::format("Request body must be valid JSON")
);
```

## JSON Wire — JSON Schema

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
  "type": "object",
  "allOf": [
    { "$ref": "gts://gts.cf.core.errors.err.v1~" },
    {
      "properties": {
        "type": {
          "const": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~"
        },
        "title": { "const": "Invalid Argument" },
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
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
  "title": "Invalid Argument",
  "status": 400,
  "detail": "Request validation failed",
  "context": {
    "resource_type": "gts.cf.core.users.user.v1~",
    "field_violations": [
      {
        "field": "email",
        "description": "must be a valid email address",
        "reason": "INVALID_FORMAT"
      },
      {
        "field": "age",
        "description": "must be at least 18",
        "reason": "OUT_OF_RANGE"
      }
    ]
  }
}
```
