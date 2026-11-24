# 04 Deadline Exceeded

**Category**: `deadline_exceeded`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.deadline_exceeded.v1~`
**HTTP Status**: 504
**Title**: "Deadline Exceeded"
**Use When**: The server did not complete the operation within the allowed time.
**Similar Categories**: `cancelled` — client-initiated cancellation, not server-side timeout
**Resource-scoped error**: yes
**Default Message**: Same as the `detail` parameter passed to the constructor.

## Context Schema

| Field | Type | Description |
|-------|------|-------------|
| `resource_type` | `String` | GTS type identifier of the associated resource |
| `resource_name` | `Option<String>` | Identifier of the associated resource |
| `reason` | `Option<String>` | SDK-defined reason code (e.g., `REQUEST_NOT_SENT`); omitted when not set |
| `extra` | `Option<Object>` | Reserved for derived GTS type extensions (p3+); absent in p1 |

`reason` is a reason code defined in the SDK of the service that emits the error, as a typed enum, not human-readable text (that is `detail`). Producers send only codes from that enum. `with_reason` accepts any string, so the toolkit does not check the code. A reader keeps an unknown code as is and draws no conclusion from it: for example, an unknown code does not show that a non-idempotent request is safe to retry.

## Constructor Example

```rust
use toolkit_canonical_errors::resource_error;

#[resource_error("gts.cf.core.users.user.v1~")]
struct UserResourceError;

let err = UserResourceError::deadline_exceeded("Request timed out").create();

// With an optional reason code
let err = UserResourceError::deadline_exceeded("Request timed out")
    .with_reason("REQUEST_NOT_SENT")
    .create();
```

## JSON Wire — JSON Schema

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "gts://gts.cf.core.errors.err.v1~cf.core.err.deadline_exceeded.v1~",
  "type": "object",
  "allOf": [
    { "$ref": "gts://gts.cf.core.errors.err.v1~" },
    {
      "properties": {
        "type": {
          "const": "gts://gts.cf.core.errors.err.v1~cf.core.err.deadline_exceeded.v1~"
        },
        "title": { "const": "Deadline Exceeded" },
        "status": { "const": 504 },
        "context": {
          "type": "object",
          "required": ["resource_type"],
          "properties": {
            "resource_type": {
              "type": "string",
              "description": "GTS type identifier of the associated resource"
            },
            "resource_name": {
              "type": "string",
              "description": "Identifier of the associated resource (set via with_resource())"
            },
            "reason": {
              "type": "string",
              "description": "SDK-defined reason code (set via with_reason()); omitted when not set"
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
  ]
}
```

## JSON Wire — JSON Example

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.deadline_exceeded.v1~",
  "title": "Deadline Exceeded",
  "status": 504,
  "detail": "Request timed out",
  "context": {
    "resource_type": "gts.cf.core.users.user.v1~"
  }
}
```

With a reason:

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.deadline_exceeded.v1~",
  "title": "Deadline Exceeded",
  "status": 504,
  "detail": "Request timed out",
  "context": {
    "resource_type": "gts.cf.core.users.user.v1~",
    "reason": "REQUEST_NOT_SENT"
  }
}
```
