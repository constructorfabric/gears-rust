# 09 Failed Precondition

**Category**: `failed_precondition`
**GTS ID**: `gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~`
**HTTP Status**: 400
**Title**: "Failed Precondition"
**Use When**: The request is valid but the system is not in the required state to perform it (e.g., deleting a non-empty directory, operating on a resource in the wrong lifecycle state).
**Similar Categories**: `invalid_argument` — request itself is bad vs system state prevents it
**Resource-scoped error**: yes
**Default Message**: "Operation precondition not met"

## Context Schema

Precondition failure:

| Field | Type | Description |
|-------|------|-------------|
| `resource_type` | `String` | GTS type identifier of the associated resource |
| `resource_name` | `Option<String>` | Identifier of the associated resource |
| `violations` | `Vec<PreconditionViolation>` | List of precondition violations |

Precondition violation:

| Field | Type | Description |
|-------|------|-------------|
| `type` | `String` | Precondition category (`STATE`, `TOS`, `VERSION`) |
| `subject` | `String` | What failed the check |
| `description` | `String` | How to resolve the failure |

## Constructor Example

```rust
use toolkit_canonical_errors::resource_error;

#[resource_error("gts.cf.core.tenants.tenant.v1~")]
struct TenantResourceError;

let err = TenantResourceError::failed_precondition()
    .with_precondition_violation(
        "tenant.users",
        "Tenant must have zero active users before deletion",
        "STATE",
    )
    .create();
```

## JSON Wire — JSON Schema

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
  "type": "object",
  "allOf": [
    { "$ref": "gts://gts.cf.core.errors.err.v1~" },
    {
      "properties": {
        "type": {
          "const": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~"
        },
        "title": { "const": "Failed Precondition" },
        "status": { "const": 400 },
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
              "items": { "$ref": "#/$defs/PreconditionViolation" }
            }
          },
          "additionalProperties": false
        }
      }
    }
  ],
  "$defs": {
    "PreconditionViolation": {
      "type": "object",
      "required": ["type", "subject", "description"],
      "properties": {
        "type": { "type": "string", "description": "Precondition category (STATE, TOS, VERSION)" },
        "subject": { "type": "string", "description": "What failed the check" },
        "description": { "type": "string", "description": "How to resolve the failure" }
      },
      "additionalProperties": false
    }
  }
}
```

## JSON Wire — JSON Example

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
  "title": "Failed Precondition",
  "status": 400,
  "detail": "Operation precondition not met",
  "context": {
    "resource_type": "gts.cf.core.tenants.tenant.v1~",
    "violations": [
      {
        "type": "STATE",
        "subject": "tenant.users",
        "description": "Tenant must have zero active users before deletion"
      }
    ]
  }
}
```
