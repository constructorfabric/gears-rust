# 01 Cancelled

**Category**: `cancelled`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.cancelled.v1~`
**HTTP Status**: 499 (Client Closed Request)
**Title**: "Cancelled"
**Context Type**: `RequestInfo`
**Use When**: The client cancelled the request before the server finished processing.
**Similar Categories**: `deadline_exceeded` — server-side timeout, not client-initiated
**Default Message**: "Operation cancelled by the client"

## Context Schema

| Field | Type | Description |
|-------|------|-------------|
| `details` | `Option<Object>` | Reserved for derived GTS type extensions (p3+); absent in p1 |


## Constructor Example

```rust
use cf_modkit_errors::{CanonicalError, RequestInfo};

let err = CanonicalError::cancelled(
    RequestInfo { request_id: "01JREQ-DEF".to_string() }
);
// CanonicalError::cancelled uses RequestInfo as its context type.
// resource_type and debug_info are optional; the minimal constructor sets both to None.
// When resource_type is set via .with_resource_type("gts.cf..."), it is injected into
// the wire context object during Problem mapping. The JSON example below shows an
// optional resource_type present in context.
```

## JSON Wire — JSON Schema

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "gts://gts.cf.core.errors.err.v1~cf.core.err.cancelled.v1~",
  "type": "object",
  "allOf": [
    { "$ref": "gts://gts.cf.core.errors.err.v1~" },
    {
      "properties": {
        "type": {
          "const": "gts://gts.cf.core.errors.err.v1~cf.core.err.cancelled.v1~"
        },
        "title": { "const": "Cancelled" },
        "status": { "const": 499 },
        "context": {
          "type": "object",
          "required": ["request_id"],
          "properties": {
            "resource_type": {
              "type": "string",
              "description": "GTS type identifier of the associated resource (injected when resource_type is set)"
            },
            "request_id": {
              "type": "string",
              "description": "Identifier of the cancelled request"
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
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.cancelled.v1~",
  "title": "Cancelled",
  "status": 499,
  "detail": "Operation cancelled by the client",
  "context": {
    "resource_type": "gts.cf.oagw.upstreams.upstream.v1~",
    "request_id": "01JREQ-DEF"
  }
}
```
