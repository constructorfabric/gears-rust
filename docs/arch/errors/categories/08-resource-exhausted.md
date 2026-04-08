# 08 Resource Exhausted

**Category**: `resource_exhausted`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~`
**HTTP Status**: 429
**Title**: "Resource Exhausted"
**Context Type**: `QuotaFailure`
**Use When**: A quota or rate limit was exceeded.
**Similar Categories**: `service_unavailable` — system overload vs per-caller quota
**Default Message**: "Quota exceeded"

## Context Schema

Quota failure:

| Field | Type | Description |
|-------|------|-------------|
| `violations` | `Vec<QuotaViolation>` | List of quota violations |
| `details` | `Option<Object>` | Reserved for derived GTS type extensions (p3+); absent in p1 |

Quota violation:

| Field | Type | Description |
|-------|------|-------------|
| `subject` | `String` | What the quota applies to (e.g., `"requests_per_minute"`) |
| `description` | `String` | Human-readable explanation |

## Constructor Example

```rust
use cf_modkit_errors::{CanonicalError, QuotaFailure, QuotaViolation};

let err = CanonicalError::resource_exhausted(
    QuotaFailure {
        violations: vec![
            QuotaViolation {
                subject: "requests_per_minute".to_string(),
                description: "Limit of 100 requests per minute exceeded".to_string(),
            }
        ]
    }
);
```

## JSON Wire — JSON Schema

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "gts://gts.cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~",
  "type": "object",
  "allOf": [
    { "$ref": "gts://gts.cf.core.errors.err.v1~" },
    {
      "properties": {
        "type": {
          "const": "gts://gts.cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~"
        },
        "title": { "const": "Resource Exhausted" },
        "status": { "const": 429 },
        "context": {
          "type": "object",
          "required": ["violations"],
          "properties": {
            "violations": {
              "type": "array",
              "items": { "$ref": "#/$defs/QuotaViolation" }
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
  ],
  "$defs": {
    "QuotaViolation": {
      "type": "object",
      "required": ["subject", "description"],
      "properties": {
        "subject": { "type": "string", "description": "What the quota applies to" },
        "description": { "type": "string", "description": "Human-readable explanation" }
      },
      "additionalProperties": false
    }
  }
}
```

## JSON Wire — JSON Example

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~",
  "title": "Resource Exhausted",
  "status": 429,
  "detail": "Quota exceeded",
  "context": {
    "violations": [
      {
        "subject": "requests_per_minute",
        "description": "Limit of 100 requests per minute exceeded"
      }
    ]
  }
}
```
