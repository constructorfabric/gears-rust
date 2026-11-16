# 08 Resource Exhausted

**Category**: `resource_exhausted`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~`
**HTTP Status**: 429
**Title**: "Resource Exhausted"
**Use When**: A quota or rate limit was exceeded.
**Similar Categories**: `service_unavailable` — system overload vs per-caller quota
**Resource-scoped error**: yes
**Default Message**: Same as the `detail` parameter passed to the constructor.

## Context Schema

Quota failure:

| Field | Type | Description |
|-------|------|-------------|
| `resource_type` | `String` | GTS type identifier of the associated resource |
| `resource_name` | `Option<String>` | Identifier of the associated resource |
| `violations` | `Vec<QuotaViolation>` | List of quota violations |
| `extra` | `Option<Object>` | Reserved for derived GTS type extensions (p3+); absent in p1 |

Quota violation:

| Field | Type | Description |
|-------|------|-------------|
| `subject` | `String` | What the quota applies to (e.g., `"requests_per_minute"`) |
| `description` | `String` | Human-readable explanation |
| `retry_after_seconds` | `Option<u64>` | Minimum seconds to wait before retrying; set when the limit replenishes (rate limits), absent for hard exhaustion |

## HTTP Headers

When at least one violation carries `retry_after_seconds`, the HTTP response also carries `Retry-After` with the largest of those values, so a client that waits that long has waited for every violated limit. With no hint on any violation, there is no `Retry-After` header.

In Rust the same value is `CanonicalError::retry_after_seconds()`; SDK projections that expose a retry delay use it (ADR-0005).

## Constructor Example

```rust
use toolkit_canonical_errors::resource_error;

#[resource_error("gts.cf.core.users.user.v1~")]
struct UserResourceError;

let err = UserResourceError::resource_exhausted("Quota exceeded")
    .with_quota_violation(
        "requests_per_minute",
        "Limit of 100 requests per minute exceeded",
    )
    .with_quota_violation_retry_after_seconds(30)
    .create();
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
          "required": ["resource_type", "violations"],
          "properties": {
            "resource_type": {
              "type": "string",
              "description": "GTS type identifier of the associated resource"
            },
            "resource_name": {
              "type": "string",
              "description": "Identifier of the associated resource (set via with_resource())"
            },
            "violations": {
              "type": "array",
              "items": { "$ref": "#/$defs/QuotaViolation" }
            },
            "extra": {
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
        "description": { "type": "string", "description": "Human-readable explanation" },
        "retry_after_seconds": {
          "type": "integer",
          "minimum": 0,
          "description": "Minimum seconds to wait before retrying; absent for hard exhaustion"
        }
      },
      "additionalProperties": false
    }
  }
}
```

## JSON Wire — JSON Example

Sent with the header `Retry-After: 30`.

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~",
  "title": "Resource Exhausted",
  "status": 429,
  "detail": "Quota exceeded",
  "context": {
    "resource_type": "gts.cf.core.users.user.v1~",
    "violations": [
      {
        "subject": "requests_per_minute",
        "description": "Limit of 100 requests per minute exceeded",
        "retry_after_seconds": 30
      }
    ]
  }
}
```
