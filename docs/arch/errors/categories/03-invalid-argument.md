# 03 Invalid Argument

**Category**: `invalid_argument`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~`
**HTTP Status**: 400
**Title**: "Invalid Argument"
**Use When**: The client sent an invalid request — malformed fields, bad format, or constraint violations. Independent of system state.
**Similar Categories**: `out_of_range` — value is valid format but outside acceptable range; `failed_precondition` — request is valid but system state prevents it
**Resource-scoped error**: yes
**Default Message**: "Request validation failed" (FieldViolations) or the format/constraint string

## Context Schema

**Variant: FieldViolations**

InvalidArgument:

| Field | Type | Description |
|-------|------|-------------|
| `resource_type` | `String` | GTS type identifier of the associated resource |
| `resource_name` | `Option<String>` | Identifier of the associated resource |
| `field_violations` | `Vec<FieldViolation>` | List of per-field validation errors |

Field violation:

| Field | Type | Description |
|-------|------|-------------|
| `field` | `String` | Field path (e.g., `"email"`, `"address.zip"`) |
| `description` | `String` | Human-readable explanation |
| `reason` | `String` | Machine-readable reason code (`REQUIRED`, `INVALID_EMAIL_FORMAT`, etc.) |

**Variant: Format**

| Field | Type | Description |
|-------|------|-------------|
| `resource_type` | `String` | GTS type identifier of the associated resource |
| `resource_name` | `Option<String>` | Identifier of the associated resource |
| `format` | `String` | Human-readable format error message |

**Variant: Constraint**

| Field | Type | Description |
|-------|------|-------------|
| `resource_type` | `String` | GTS type identifier of the associated resource |
| `resource_name` | `Option<String>` | Identifier of the associated resource |
| `constraint` | `String` | Human-readable constraint violation message |

## Constructor Examples

**FieldViolations** — per-field validation errors:

```rust
use toolkit_canonical_errors::resource_error;

#[resource_error("gts.cf.core.users.user.v1~")]
struct UserResourceError;

let err = UserResourceError::invalid_argument()
    .with_field_violation("email", "must be a valid email address", "INVALID_EMAIL_FORMAT")
    .with_field_violation("phone", "must match E.164 format (+1234567890)", "INVALID_PHONE_FORMAT")
    .create();
```

**Format** — malformed input (e.g. unparseable body):

```rust
let err = UserResourceError::invalid_argument()
    .with_format("request body is not valid JSON")
    .create();
```

**Constraint** — structural constraint violation:

```rust
let err = UserResourceError::invalid_argument()
    .with_constraint("at most 10 tags allowed per resource")
    .create();
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
            },
            {
              "type": "object",
              "required": ["resource_type", "format"],
              "properties": {
                "resource_type": { "type": "string" },
                "resource_name": { "type": "string" },
                "format": { "type": "string" }
              },
              "additionalProperties": false
            },
            {
              "type": "object",
              "required": ["resource_type", "constraint"],
              "properties": {
                "resource_type": { "type": "string" },
                "resource_name": { "type": "string" },
                "constraint": { "type": "string" }
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
        "field": "phone",
        "description": "must match E.164 format (+1234567890)",
        "reason": "INVALID_FORMAT"
      }
    ]
  }
}
```
