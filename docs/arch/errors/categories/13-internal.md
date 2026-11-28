# 13 Internal

**Category**: `internal`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.internal.v1~`
**HTTP Status**: 500
**Title**: "Internal"
**Use When**: A known infrastructure failure occurred (database error, serialization bug, etc.). The detail in production is generic; diagnostics are in logs via `trace_id`.
**Similar Categories**: `unknown` — truly unknown error vs known infrastructure failure
**Resource-scoped error**: no
**Default Message**: "An internal error occurred. Please retry later."

## Context Schema

Empty object (`{}`); `InternalV1::description` is `#[serde(skip)]` (`libs/toolkit-canonical-errors/src/context.rs`).

## Constructor Example

```rust
use toolkit_canonical_errors::CanonicalError;

let err = CanonicalError::internal("An internal error occurred. Please retry later.").create();
```

## JSON Wire — JSON Schema

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
  "type": "object",
  "allOf": [
    { "$ref": "gts://gts.cf.core.errors.err.v1~" },
    {
      "properties": {
        "type": {
          "const": "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~"
        },
        "title": { "const": "Internal" },
        "status": { "const": 500 },
        "context": {
          "type": "object",
          "properties": {
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
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
  "title": "Internal",
  "status": 500,
  "detail": "An internal error occurred. Please retry later.",
  "context": {
  }
}
```
