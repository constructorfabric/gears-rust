# 12 Unimplemented

**Category**: `unimplemented`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.unimplemented.v1~`
**HTTP Status**: 501
**Title**: "Unimplemented"
**Context Type**: `ErrorInfo`
**Use When**: The requested operation is recognized but not implemented (e.g., a planned feature, an unsupported protocol variant).
**Similar Categories**: `internal` — bug vs intentionally unimplemented
**Default Message**: "This operation is not implemented"

## Context Schema

| Field | Type | Description |
|-------|------|-------------|
| `reason` | `String` | Machine-readable reason code (e.g., `GRPC_ROUTING`) |
| `domain` | `String` | Logical grouping (e.g., `"cf.oagw"`) |
| `metadata` | `HashMap<String, String>` | Arbitrary key-value pairs for additional context |
| `details` | `Option<Object>` | Reserved for derived GTS type extensions (p3+); absent in p1 |

## Constructor Example

```rust
use cf_modkit_errors::{CanonicalError, ErrorInfo};

let err = CanonicalError::unimplemented(
    ErrorInfo::new("GRPC_ROUTING", "cf.oagw")
);
```

## JSON Wire — JSON Schema

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "gts://gts.cf.core.errors.err.v1~cf.core.err.unimplemented.v1~",
  "type": "object",
  "allOf": [
    { "$ref": "gts://gts.cf.core.errors.err.v1~" },
    {
      "properties": {
        "type": {
          "const": "gts://gts.cf.core.errors.err.v1~cf.core.err.unimplemented.v1~"
        },
        "title": { "const": "Unimplemented" },
        "status": { "const": 501 },
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
              "description": "Machine-readable reason code (e.g., GRPC_ROUTING)"
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
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.unimplemented.v1~",
  "title": "Unimplemented",
  "status": 501,
  "detail": "This operation is not implemented",
  "context": {
    "resource_type": "gts.cf.oagw.upstreams.upstream.v1~",
    "reason": "GRPC_ROUTING",
    "domain": "cf.oagw",
    "metadata": {}
  }
}
```
