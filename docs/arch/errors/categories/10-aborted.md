# 10 Aborted

**Category**: `aborted`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.aborted.v1~`
**HTTP Status**: 409
**Title**: "Aborted"
**Context Type**: `ErrorInfo`
**Use When**: The operation was aborted due to a concurrency conflict (optimistic locking failure, transaction conflict). The client can retry.
**Similar Categories**: `already_exists` — duplicate on create vs conflict on update
**Default Message**: "Operation aborted due to concurrency conflict"

## Context Schema

| Field | Type | Description |
|-------|------|-------------|
| `reason` | `String` | Machine-readable reason code (e.g., `OPTIMISTIC_LOCK_FAILURE`) |
| `domain` | `String` | Logical grouping (e.g., `"cf.oagw"`) |
| `metadata` | `HashMap<String, String>` | Arbitrary key-value pairs for additional context |
| `details` | `Option<Object>` | Reserved for derived GTS type extensions (p3+); absent in p1 |

## Rust Definitions and Constructor Example

```rust
use cf_modkit_errors::{CanonicalError, ErrorInfo};
use std::collections::HashMap;

let mut metadata = HashMap::new();
metadata.insert("expected_version".to_string(), "3".to_string());
metadata.insert("actual_version".to_string(), "5".to_string());

let err = CanonicalError::aborted(
    ErrorInfo::new("OPTIMISTIC_LOCK_FAILURE", "cf.oagw")
        .with_metadata(metadata)
);
```

## JSON Wire — JSON Schema

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "gts://gts.cf.core.errors.err.v1~cf.core.err.aborted.v1~",
  "type": "object",
  "allOf": [
    { "$ref": "gts://gts.cf.core.errors.err.v1~" },
    {
      "properties": {
        "type": {
          "const": "gts://gts.cf.core.errors.err.v1~cf.core.err.aborted.v1~"
        },
        "title": { "const": "Aborted" },
        "status": { "const": 409 },
        "context": {
          "type": "object",
          "required": ["reason", "domain", "metadata"],
          "properties": {
            "resource_type": {
              "type": "string",
              "description": "GTS type identifier of the associated resource (injected when resource_type is set)"
            },
            "reason": {
              "type": "string",
              "description": "Machine-readable reason code (e.g., OPTIMISTIC_LOCK_FAILURE)"
            },
            "domain": {
              "type": "string",
              "description": "Logical grouping (e.g., cf.oagw)"
            },
            "metadata": {
              "type": "object",
              "additionalProperties": { "type": "string" },
              "description": "Arbitrary key-value pairs for additional context"
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
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.aborted.v1~",
  "title": "Aborted",
  "status": 409,
  "detail": "Operation aborted due to concurrency conflict",
  "context": {
    "resource_type": "gts.cf.oagw.upstreams.upstream.v1~",
    "reason": "OPTIMISTIC_LOCK_FAILURE",
    "domain": "cf.oagw",
    "metadata": {
      "expected_version": "3",
      "actual_version": "5"
    }
  }
}
```
