# 02 Unknown

**Category**: `unknown`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.unknown.v1~`
**HTTP Status**: 500
**Title**: "Unknown"
**Context Type**: `DebugInfo`
**Use When**: An error occurred that does not match any other canonical category. Prefer a more specific category when possible.
**Similar Categories**: `internal` — known infrastructure failure vs truly unknown error
**Default Message**: Same as the `detail` parameter passed to the constructor.

## Context Schema

| Field | Type | Description |
|-------|------|-------------|
| `detail` | `String` | Human-readable debug message (generic in production) |
| `stack_entries` | `Vec<String>` | Stack trace entries (empty in production) |
| `details` | `Option<Object>` | Reserved for derived GTS type extensions (p3+); absent in p1 |

## Constructor Example

```rust
use cf_modkit_errors::CanonicalError;

let err = CanonicalError::unknown("Unexpected response from payment provider");
// Creates DebugInfo internally with the detail string
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
          "required": ["detail", "stack_entries"],
          "properties": {
            "resource_type": {
              "type": "string",
              "description": "GTS type identifier of the associated resource (injected when resource_type is set)"
            },
            "detail": {
              "type": "string",
              "description": "Human-readable debug message (generic in production)"
            },
            "stack_entries": {
              "type": "array",
              "items": { "type": "string" },
              "description": "Stack trace entries (empty in production)"
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
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.unknown.v1~",
  "title": "Unknown",
  "status": 500,
  "detail": "Unexpected response from payment provider",
  "context": {
    "detail": "Unexpected response from payment provider",
    "stack_entries": []
  }
}
```
