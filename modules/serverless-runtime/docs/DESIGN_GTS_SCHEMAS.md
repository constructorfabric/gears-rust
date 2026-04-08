# GTS Type Schemas — Serverless Runtime

<!--
Companion file to DESIGN.md.
Contains all formal GTS JSON Schema definitions extracted from the design document.
Each schema is preserved exactly as defined in the original design, grouped by entity.
-->

## Shared Components

### OwnerRef

**GTS ID:** `gts.x.core.serverless.owner_ref.v1~`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "gts://gts.x.core.serverless.owner_ref.v1~",
  "title": "Owner Reference",
  "description": "Ownership reference. owner_type determines default visibility: user=private, tenant=tenant-visible, system=platform-provided.",
  "type": "object",
  "properties": {
    "owner_type": {
      "type": "string",
      "enum": [
        "user",
        "tenant",
        "system"
      ]
    },
    "id": {
      "type": "string"
    },
    "tenant_id": {
      "type": "string"
    }
  },
  "required": [
    "owner_type",
    "id",
    "tenant_id"
  ]
}
```

### IOSchema

**GTS ID:** `gts.x.core.serverless.io_schema.v1~`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "gts://gts.x.core.serverless.io_schema.v1~",
  "title": "IO Schema",
  "description": "Input/output contract. params/returns accept JSON Schema, GTS $ref, or null for void.",
  "type": "object",
  "properties": {
    "params": {
      "description": "Input schema. Use $ref with gts:// URI for GTS types. Null or absent for void.",
      "oneOf": [
        {
          "type": "object"
        },
        {
          "type": "null"
        }
      ]
    },
    "returns": {
      "description": "Output schema. Use $ref with gts:// URI for GTS types. Null or absent for void.",
      "oneOf": [
        {
          "type": "object"
        },
        {
          "type": "null"
        }
      ]
    },
    "errors": {
      "type": "array",
      "items": {
        "type": "string",
        "x-gts-ref": "gts.*"
      },
      "description": "GTS error type IDs.",
      "default": []
    }
  }
}
```

### Limits (Base)

**GTS ID:** `gts.x.core.serverless.limits.v1~`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "gts://gts.x.core.serverless.limits.v1~",
  "title": "Function Limits (Base)",
  "description": "Base limits schema. Adapters derive type-specific schemas via GTS inheritance.",
  "type": "object",
  "properties": {
    "timeout_seconds": {
      "type": "integer",
      "minimum": 1,
      "default": 30,
      "description": "Max execution duration in seconds."
    },
    "max_concurrent": {
      "type": "integer",
      "minimum": 1,
      "default": 100,
      "description": "Max concurrent invocations."
    }
  },
  "additionalProperties": true
}
```

### Starlark Adapter Limits

**GTS ID:** `gts.x.core.serverless.limits.v1~x.core.serverless.adapter.starlark.limits.v1~`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "gts://gts.x.core.serverless.limits.v1~x.core.serverless.adapter.starlark.limits.v1~",
  "title": "Starlark Adapter Limits",
  "description": "Limits for Starlark embedded runtime.",
  "allOf": [
    {
      "$ref": "gts://gts.x.core.serverless.limits.v1~"
    },
    {
      "type": "object",
      "properties": {
        "memory_mb": {
          "type": "integer",
          "minimum": 1,
          "maximum": 512,
          "default": 128,
          "description": "Memory allocation in MB."
        },
        "cpu": {
          "type": "number",
          "minimum": 0.1,
          "maximum": 1.0,
          "default": 0.2,
          "description": "CPU allocation in fractional cores."
        }
      }
    }
  ]
}
```

### Lambda Adapter Limits

**GTS ID:** `gts.x.core.serverless.limits.v1~x.core.serverless.adapter.lambda.limits.v1~`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "gts://gts.x.core.serverless.limits.v1~x.core.serverless.adapter.lambda.limits.v1~",
  "title": "Lambda Adapter Limits",
  "description": "Limits for AWS Lambda adapter. CPU is derived from memory tier.",
  "allOf": [
    {
      "$ref": "gts://gts.x.core.serverless.limits.v1~"
    },
    {
      "type": "object",
      "properties": {
        "memory_mb": {
          "type": "integer",
          "minimum": 128,
          "maximum": 10240,
          "default": 128,
          "description": "Memory allocation in MB (CPU scales with memory)."
        },
        "ephemeral_storage_mb": {
          "type": "integer",
          "minimum": 512,
          "maximum": 10240,
          "default": 512,
          "description": "Ephemeral storage in MB."
        }
      }
    }
  ]
}
```

### RetryPolicy

**GTS ID:** `gts.x.core.serverless.retry_policy.v1~`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "gts://gts.x.core.serverless.retry_policy.v1~",
  "title": "Retry Policy",
  "description": "Retry configuration for failed invocations.",
  "type": "object",
  "properties": {
    "max_attempts": {
      "type": "integer",
      "minimum": 0,
      "default": 3
    },
    "initial_delay_ms": {
      "type": "integer",
      "minimum": 0,
      "default": 200
    },
    "max_delay_ms": {
      "type": "integer",
      "minimum": 0,
      "default": 10000
    },
    "backoff_multiplier": {
      "type": "number",
      "minimum": 1.0,
      "default": 2.0
    },
    "non_retryable_errors": {
      "type": "array",
      "items": {
        "type": "string",
        "x-gts-ref": "gts.*"
      }
    }
  },
  "required": [
    "max_attempts"
  ]
}
```

### RateLimit (Base)

**GTS ID:** `gts.x.core.serverless.rate_limit.v1~`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "gts://gts.x.core.serverless.rate_limit.v1~",
  "title": "Rate Limit (Base)",
  "description": "Base rate limiting type. Empty marker — strategy-specific configuration is defined by derived types.",
  "type": "object",
  "additionalProperties": true
}
```

### Token Bucket Rate Limit Config

**GTS ID:** `gts.x.core.serverless.rate_limit.v1~x.core.serverless.rate_limit.token_bucket.v1~`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "gts://gts.x.core.serverless.rate_limit.v1~x.core.serverless.rate_limit.token_bucket.v1~",
  "title": "Token Bucket Rate Limit Config",
  "description": "Config schema for the system-default token bucket rate limiter. Per-second and per-minute limits enforced independently.",
  "type": "object",
  "properties": {
    "max_requests_per_second": {
      "type": "number",
      "minimum": 0,
      "default": 0,
      "description": "Maximum sustained invocations per second. 0 means no per-second limit."
    },
    "max_requests_per_minute": {
      "type": "integer",
      "minimum": 0,
      "default": 0,
      "description": "Maximum sustained invocations per minute. 0 means no per-minute limit."
    },
    "burst_size": {
      "type": "integer",
      "minimum": 1,
      "default": 10,
      "description": "Maximum instantaneous burst for the per-second bucket. Permits short traffic spikes before the per-second rate takes effect."
    }
  }
}
```

## Implementation

### Implementation

**GTS ID:** `gts.x.core.serverless.implementation.v1~`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "gts://gts.x.core.serverless.implementation.v1~",
  "title": "Function Implementation",
  "description": "Implementation definition with explicit adapter for limits validation.",
  "type": "object",
  "properties": {
    "adapter": {
      "type": "string",
      "x-gts-ref": "gts.x.core.serverless.adapter.*",
      "description": "GTS type ID of the adapter (e.g., gts.x.core.serverless.adapter.starlark.v1~)."
    }
  },
  "required": [
    "adapter"
  ],
  "oneOf": [
    {
      "properties": {
        "kind": {
          "const": "code"
        },
        "code": {
          "type": "object",
          "properties": {
            "language": {
              "type": "string",
              "description": "Source language (e.g., starlark, wasm)."
            },
            "source": {
              "type": "string",
              "description": "Inline source code."
            }
          },
          "required": [
            "language",
            "source"
          ]
        }
      },
      "required": [
        "kind",
        "code"
      ]
    },
    {
      "properties": {
        "kind": {
          "const": "workflow_spec"
        },
        "workflow_spec": {
          "type": "object",
          "properties": {
            "format": {
              "type": "string",
              "description": "Workflow format (e.g., serverless-workflow)."
            },
            "spec": {
              "type": "object",
              "description": "Workflow specification object."
            }
          },
          "required": [
            "format",
            "spec"
          ]
        }
      },
      "required": [
        "kind",
        "workflow_spec"
      ]
    },
    {
      "properties": {
        "kind": {
          "const": "adapter_ref"
        },
        "adapter_ref": {
          "type": "object",
          "properties": {
            "definition_id": {
              "type": "string",
              "description": "Adapter-specific definition identifier."
            }
          },
          "required": [
            "definition_id"
          ]
        }
      },
      "required": [
        "kind",
        "adapter_ref"
      ]
    }
  ]
}
```

## Workflow Traits

### WorkflowTraits

**GTS ID:** `gts.x.core.serverless.workflow_traits.v1~`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "gts://gts.x.core.serverless.workflow_traits.v1~",
  "title": "Workflow Traits",
  "description": "Workflow-specific execution traits: compensation, checkpointing, suspension.",
  "type": "object",
  "properties": {
    "compensation": {
      "type": "object",
      "description": "Compensation handlers for saga pattern. Each handler is a function reference or null. Referenced functions receive a CompensationContext (gts.x.core.serverless.compensation_context.v1~) as their input.",
      "properties": {
        "on_failure": {
          "oneOf": [
            {
              "type": "string",
              "x-gts-ref": "gts.x.core.serverless.function.*",
              "description": "GTS ID of function to invoke on workflow failure. Receives CompensationContext as input."
            },
            {
              "type": "null"
            }
          ],
          "default": null,
          "description": "Function to invoke for compensation on failure, or null for no compensation. Invoked with CompensationContext as the single JSON body."
        },
        "on_cancel": {
          "oneOf": [
            {
              "type": "string",
              "x-gts-ref": "gts.x.core.serverless.function.*",
              "description": "GTS ID of function to invoke on workflow cancellation. Receives CompensationContext as input."
            },
            {
              "type": "null"
            }
          ],
          "default": null,
          "description": "Function to invoke for compensation on cancel, or null for no compensation. Invoked with CompensationContext as the single JSON body."
        }
      }
    },
    "checkpointing": {
      "type": "object",
      "properties": {
        "strategy": {
          "enum": [
            "automatic",
            "manual",
            "disabled"
          ],
          "default": "automatic"
        }
      }
    },
    "max_suspension_days": {
      "type": "integer",
      "minimum": 1,
      "default": 30
    }
  },
  "required": [
    "compensation",
    "checkpointing",
    "max_suspension_days"
  ]
}
```

### CompensationContext

**GTS ID:** `gts.x.core.serverless.compensation_context.v1~`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "gts://gts.x.core.serverless.compensation_context.v1~",
  "title": "Compensation Context",
  "description": "Input envelope passed to compensation functions. Delivered as the single JSON body (params) when the runtime invokes an on_failure or on_cancel handler.",
  "type": "object",
  "required": [
    "trigger",
    "original_workflow_invocation_id",
    "failed_step_id",
    "workflow_state_snapshot",
    "timestamp",
    "invocation_metadata"
  ],
  "properties": {
    "trigger": {
      "type": "string",
      "enum": [
        "failure",
        "cancellation"
      ],
      "description": "What caused compensation to start. 'failure' maps to on_failure, 'cancellation' maps to on_cancel."
    },
    "original_workflow_invocation_id": {
      "type": "string",
      "description": "Invocation ID of the failed/cancelled workflow run. Use this to correlate compensation actions with the original execution."
    },
    "failed_step_id": {
      "type": "string",
      "description": "Identifier of the step that failed or was active at cancellation. Adapter-specific granularity. Set to 'unknown' when the adapter does not track step-level state."
    },
    "failed_step_error": {
      "type": "object",
      "description": "Error details for the failed step. Present when trigger is 'failure', absent for 'cancellation'.",
      "properties": {
        "error_type": {
          "type": "string",
          "description": "Categorized error type (e.g., 'timeout', 'runtime_error', 'resource_exhausted')."
        },
        "message": {
          "type": "string",
          "description": "Human-readable error description."
        },
        "error_metadata": {
          "type": "object",
          "additionalProperties": true,
          "description": "Error-type-specific metadata. Structure is defined per error type."
        }
      },
      "required": [
        "error_type",
        "message"
      ]
    },
    "workflow_state_snapshot": {
      "type": "object",
      "description": "Last checkpointed workflow state. Adapter-specific and opaque to the platform. Contains accumulated step results, intermediate data, or adapter-native state. Empty object if failure occurred before the first checkpoint.",
      "additionalProperties": true
    },
    "timestamp": {
      "type": "string",
      "format": "date-time",
      "description": "ISO 8601 timestamp of when compensation was triggered."
    },
    "invocation_metadata": {
      "type": "object",
      "description": "Metadata from the original workflow invocation.",
      "required": [
        "function_id",
        "original_input",
        "tenant_id"
      ],
      "properties": {
        "function_id": {
          "type": "string",
          "x-gts-ref": "gts.x.core.serverless.function.*",
          "description": "GTS ID of the workflow function that failed."
        },
        "original_input": {
          "type": "object",
          "description": "The input parameters (params) the workflow was originally invoked with."
        },
        "tenant_id": {
          "type": "string",
          "description": "Tenant that owns the workflow invocation."
        },
        "correlation_id": {
          "type": "string",
          "description": "Correlation ID from the original invocation's observability context."
        },
        "started_at": {
          "type": "string",
          "format": "date-time",
          "description": "When the original workflow invocation started."
        }
      }
    }
  }
}
```

## Invocation Lifecycle

### InvocationStatus

**GTS ID:** `gts.x.core.serverless.status.v1~`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "gts://gts.x.core.serverless.status.v1~",
  "title": "Invocation Status",
  "description": "Base type for invocation status. Concrete statuses are derived types.",
  "type": "string",
  "enum": [
    "queued",
    "running",
    "suspended",
    "succeeded",
    "failed",
    "canceled",
    "compensating",
    "compensated",
    "dead_lettered"
  ]
}
```

### Error

**GTS ID:** `gts.x.core.serverless.err.v1~`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "gts://gts.x.core.serverless.err.v1~",
  "title": "Serverless Error",
  "description": "Base error type. Concrete errors are derived types.",
  "type": "object",
  "properties": {
    "message": {
      "type": "string",
      "description": "Human-readable error message."
    },
    "category": {
      "type": "string",
      "enum": [
        "retryable",
        "non_retryable",
        "resource_limit",
        "timeout",
        "canceled"
      ],
      "description": "Error category for retry decisions."
    },
    "details": {
      "type": "object",
      "description": "Error-specific structured payload."
    }
  },
  "required": [
    "message",
    "category"
  ]
}
```

### ValidationError

**GTS ID:** `gts.x.core.serverless.err.v1~x.core.serverless.err.validation.v1~`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "gts://gts.x.core.serverless.err.v1~x.core.serverless.err.validation.v1~",
  "title": "Validation Error",
  "description": "Validation error with multiple issues, each with error type and location.",
  "allOf": [
    {
      "$ref": "gts://gts.x.core.serverless.err.v1~"
    },
    {
      "type": "object",
      "properties": {
        "issues": {
          "type": "array",
          "description": "List of validation issues.",
          "minItems": 1,
          "items": {
            "type": "object",
            "description": "A single validation issue with type, location, and message.",
            "properties": {
              "error_type": {
                "type": "string",
                "description": "Specific validation error type (e.g., 'schema_mismatch', 'missing_field', 'invalid_format')."
              },
              "location": {
                "type": "object",
                "description": "Location of the issue in the definition or input.",
                "properties": {
                  "path": {
                    "type": "string",
                    "description": "JSON path to the error location (e.g., '$.traits.limits.timeout_seconds')."
                  },
                  "line": {
                    "type": [
                      "integer",
                      "null"
                    ],
                    "description": "Line number in source code (for code implementations)."
                  },
                  "column": {
                    "type": [
                      "integer",
                      "null"
                    ],
                    "description": "Column number in source code (for code implementations)."
                  }
                },
                "required": [
                  "path"
                ]
              },
              "message": {
                "type": "string",
                "description": "Human-readable description of the issue."
              },
              "suggestion": {
                "type": [
                  "string",
                  "null"
                ],
                "description": "Suggested correction or fix for the issue."
              }
            },
            "required": [
              "error_type",
              "location",
              "message"
            ]
          }
        }
      },
      "required": [
        "issues"
      ]
    }
  ]
}
```

### InvocationTimelineEvent

**GTS ID:** `gts.x.core.serverless.timeline_event.v1~`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "gts://gts.x.core.serverless.timeline_event.v1~",
  "title": "Invocation Timeline Event",
  "description": "A single event in the execution timeline.",
  "type": "object",
  "properties": {
    "at": {
      "type": "string",
      "format": "date-time",
      "description": "Timestamp when the event occurred."
    },
    "event_type": {
      "type": "string",
      "enum": [
        "started",
        "step_started",
        "step_completed",
        "step_failed",
        "step_retried",
        "suspended",
        "resumed",
        "signal_received",
        "checkpoint_created",
        "compensation_started",
        "compensation_completed",
        "compensation_failed",
        "succeeded",
        "failed",
        "canceled",
        "dead_lettered"
      ],
      "description": "Type of timeline event."
    },
    "status": {
      "$ref": "gts://gts.x.core.serverless.status.v1~",
      "description": "Invocation status after this event (short enum value, e.g. 'running')."
    },
    "step_name": {
      "type": [
        "string",
        "null"
      ],
      "description": "Name of the step (for step-related events)."
    },
    "duration_ms": {
      "type": [
        "integer",
        "null"
      ],
      "minimum": 0,
      "description": "Duration of the step or action in milliseconds."
    },
    "message": {
      "type": [
        "string",
        "null"
      ],
      "description": "Human-readable description of the event."
    },
    "details": {
      "type": "object",
      "description": "Event-specific structured data.",
      "default": {}
    }
  },
  "required": [
    "at",
    "event_type",
    "status"
  ]
}
```

## Entity Definitions

### Function (Base Type)

**GTS ID:** `gts.x.core.serverless.function.v1~`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "gts://gts.x.core.serverless.function.v1~",
  "title": "Serverless Function",
  "description": "Base schema for serverless functions (functions and workflows). Identity is the GTS instance address.",
  "type": "object",
  "properties": {
    "version": {
      "type": "string",
      "pattern": "^\\d+\\.\\d+\\.\\d+$"
    },
    "tenant_id": {
      "type": "string"
    },
    "owner": {
      "$ref": "gts://gts.x.core.serverless.owner_ref.v1~"
    },
    "status": {
      "type": "string",
      "enum": [
        "draft",
        "active",
        "deprecated",
        "disabled",
        "archived",
        "deleted"
      ],
      "default": "draft"
    },
    "tags": {
      "type": "array",
      "items": {
        "type": "string"
      },
      "default": []
    },
    "title": {
      "type": "string"
    },
    "description": {
      "type": "string"
    },
    "schema": {
      "$ref": "gts://gts.x.core.serverless.io_schema.v1~"
    },
    "traits": {
      "type": "object",
      "properties": {
        "invocation": {
          "type": "object",
          "properties": {
            "supported": {
              "type": "array",
              "items": {
                "enum": [
                  "sync",
                  "async"
                ]
              }
            },
            "default": {
              "enum": [
                "sync",
                "async"
              ]
            }
          },
          "required": [
            "supported",
            "default"
          ]
        },
        "entrypoint": {
          "type": "boolean",
          "default": true,
          "description": "When true (default), the function can be invoked via external APIs (JSON-RPC, Jobs API). When false, the function can only be invoked internally via r_invoke_v1() — useful for helper functions and shared logic."
        },
        "is_idempotent": {
          "type": "boolean",
          "default": false
        },
        "caching": {
          "type": "object",
          "description": "Response caching policy. Caching is only active when the caller provides an `Idempotency-Key` header AND `max_age_seconds > 0`",
          "properties": {
            "max_age_seconds": {
              "type": "integer",
              "minimum": 0,
              "default": 0,
              "description": "Time-to-live in seconds for cached successful results. `0` disables response caching even when an idempotency key is present."
            }
          }
        },
        "rate_limit": {
          "description": "Optional rate limiting. Null or absent means no rate limiting.",
          "oneOf": [
            {
              "type": "object",
              "required": ["strategy", "config"],
              "properties": {
                "strategy": {
                  "type": "string",
                  "description": "GTS type ID of the rate limiter plugin (derived from gts.x.core.serverless.rate_limit.v1~)."
                },
                "config": {
                  "type": "object",
                  "description": "Strategy-specific configuration. Validated by the resolved plugin against its derived schema.",
                  "additionalProperties": true
                }
              },
              "additionalProperties": false
            },
            { "type": "null" }
          ],
          "default": null
        },
        "limits": {
          "$ref": "gts://gts.x.core.serverless.limits.v1~"
        },
        "retry": {
          "$ref": "gts://gts.x.core.serverless.retry_policy.v1~"
        }
      },
      "required": [
        "invocation",
        "limits",
        "retry"
      ]
    },
    "implementation": {
      "$ref": "gts://gts.x.core.serverless.implementation.v1~"
    },
    "created_at": {
      "type": "string",
      "format": "date-time"
    },
    "updated_at": {
      "type": "string",
      "format": "date-time"
    }
  },
  "required": [
    "version",
    "tenant_id",
    "owner",
    "status",
    "title",
    "schema",
    "traits",
    "implementation"
  ],
  "additionalProperties": true
}
```

### Workflow

**GTS ID:** `gts.x.core.serverless.function.v1~x.core.serverless.workflow.v1~`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "gts://gts.x.core.serverless.function.v1~x.core.serverless.workflow.v1~",
  "title": "Serverless Workflow",
  "description": "Durable, multi-step orchestration with state persistence.",
  "allOf": [
    {
      "$ref": "gts://gts.x.core.serverless.function.v1~"
    },
    {
      "type": "object",
      "properties": {
        "traits": {
          "type": "object",
          "properties": {
            "workflow": {
              "$ref": "gts://gts.x.core.serverless.workflow_traits.v1~"
            }
          },
          "required": [
            "workflow"
          ]
        }
      },
      "required": [
        "traits"
      ]
    }
  ]
}
```

### InvocationRecord

**GTS ID:** `gts.x.core.serverless.invocation.v1~`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "gts://gts.x.core.serverless.invocation.v1~",
  "title": "Invocation Record",
  "description": "Tracks lifecycle of a single function execution.",
  "type": "object",
  "properties": {
    "invocation_id": {
      "type": "string",
      "description": "Opaque unique identifier for this invocation."
    },
    "function_id": {
      "type": "string",
      "x-gts-ref": "gts.x.core.serverless.function.*",
      "description": "GTS ID of the invoked function."
    },
    "function_version": {
      "type": "string",
      "pattern": "^\\d+\\.\\d+\\.\\d+$"
    },
    "tenant_id": {
      "type": "string"
    },
    "status": {
      "$ref": "gts://gts.x.core.serverless.status.v1~",
      "description": "Invocation status (short enum value, e.g. 'running')."
    },
    "mode": {
      "type": "string",
      "enum": [
        "sync",
        "async"
      ]
    },
    "params": {
      "type": "object",
      "description": "Input parameters passed to the function."
    },
    "result": {
      "description": "Execution result (null if not completed or failed).",
      "oneOf": [
        {
          "type": "object"
        },
        {
          "type": "null"
        }
      ]
    },
    "error": {
      "description": "Error details (null if succeeded or still running).",
      "oneOf": [
        {
          "type": "object",
          "properties": {
            "error_type_id": {
              "type": "string",
              "x-gts-ref": "gts.*"
            },
            "message": {
              "type": "string"
            },
            "category": {
              "type": "string",
              "enum": [
                "retryable",
                "non_retryable",
                "resource_limit",
                "timeout",
                "canceled"
              ],
              "description": "Error category for retry decisions."
            },
            "details": {
              "type": "object"
            }
          },
          "required": [
            "error_type_id",
            "message",
            "category"
          ]
        },
        {
          "type": "null"
        }
      ]
    },
    "timestamps": {
      "type": "object",
      "properties": {
        "created_at": {
          "type": "string",
          "format": "date-time"
        },
        "started_at": {
          "type": [
            "string",
            "null"
          ],
          "format": "date-time"
        },
        "suspended_at": {
          "type": [
            "string",
            "null"
          ],
          "format": "date-time"
        },
        "finished_at": {
          "type": [
            "string",
            "null"
          ],
          "format": "date-time"
        }
      },
      "required": [
        "created_at"
      ]
    },
    "observability": {
      "type": "object",
      "properties": {
        "correlation_id": {
          "type": "string"
        },
        "trace_id": {
          "type": "string"
        },
        "span_id": {
          "type": "string"
        },
        "metrics": {
          "type": "object",
          "properties": {
            "duration_ms": {
              "type": [
                "integer",
                "null"
              ]
            },
            "billed_duration_ms": {
              "type": [
                "integer",
                "null"
              ]
            },
            "cpu_time_ms": {
              "type": [
                "integer",
                "null"
              ]
            },
            "memory_limit_mb": {
              "type": [
                "integer",
                "null"
              ]
            },
            "max_memory_used_mb": {
              "type": [
                "integer",
                "null"
              ]
            },
            "step_count": {
              "type": [
                "integer",
                "null"
              ]
            }
          }
        }
      },
      "required": [
        "correlation_id"
      ]
    }
  },
  "required": [
    "invocation_id",
    "function_id",
    "function_version",
    "tenant_id",
    "status",
    "mode",
    "timestamps",
    "observability"
  ]
}
```

## Triggers and Scheduling

### Schedule

**GTS ID:** `gts.x.core.serverless.schedule.v1~`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "gts://gts.x.core.serverless.schedule.v1~",
  "title": "Schedule",
  "description": "Recurring trigger for a function.",
  "type": "object",
  "properties": {
    "schedule_id": {
      "type": "string",
      "description": "Opaque unique identifier for this schedule."
    },
    "tenant_id": {
      "type": "string"
    },
    "function_id": {
      "type": "string",
      "x-gts-ref": "gts.x.core.serverless.function.*",
      "description": "GTS ID of the function to invoke."
    },
    "name": {
      "type": "string",
      "description": "Human-readable schedule name."
    },
    "timezone": {
      "type": "string",
      "default": "UTC",
      "description": "IANA timezone for schedule evaluation."
    },
    "expression": {
      "type": "object",
      "oneOf": [
        {
          "properties": {
            "kind": {
              "const": "cron"
            },
            "value": {
              "type": "string",
              "description": "Cron expression."
            }
          },
          "required": [
            "kind",
            "value"
          ]
        },
        {
          "properties": {
            "kind": {
              "const": "interval"
            },
            "value": {
              "type": "string",
              "description": "ISO 8601 duration (e.g., PT1H)."
            }
          },
          "required": [
            "kind",
            "value"
          ]
        }
      ]
    },
    "input_overrides": {
      "type": "object",
      "description": "Parameters merged with function defaults for each scheduled run.",
      "default": {}
    },
    "missed_policy": {
      "type": "string",
      "enum": [
        "skip",
        "catch_up",
        "backfill"
      ],
      "default": "skip",
      "description": "Policy for missed schedules: skip (ignore), catch_up (execute once), backfill (execute each)."
    },
    "max_catch_up_runs": {
      "type": "integer",
      "minimum": 1,
      "maximum": 100,
      "default": 1,
      "description": "Maximum catch-up or backfill executions when missed_policy is catch_up or backfill."
    },
    "execution_context": {
      "type": "string",
      "enum": ["system", "api_client", "user"],
      "default": "system",
      "description": "Security context for scheduled executions."
    },
    "concurrency_policy": {
      "type": "string",
      "enum": ["allow", "forbid", "replace"],
      "default": "allow",
      "description": "Behavior when previous execution is still running: allow (start new), forbid (skip), replace (cancel previous)."
    },
    "enabled": {
      "type": "boolean",
      "default": true,
      "description": "Whether the schedule is active. When false, no new executions are triggered."
    },
    "next_run_at": {
      "type": [
        "string",
        "null"
      ],
      "format": "date-time"
    },
    "last_run_at": {
      "type": [
        "string",
        "null"
      ],
      "format": "date-time"
    },
    "created_at": {
      "type": "string",
      "format": "date-time"
    },
    "updated_at": {
      "type": "string",
      "format": "date-time"
    }
  },
  "required": [
    "schedule_id",
    "tenant_id",
    "function_id",
    "name",
    "expression"
  ]
}
```

### Trigger

**GTS ID:** `gts.x.core.serverless.trigger.v1~`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "gts://gts.x.core.serverless.trigger.v1~",
  "title": "Trigger",
  "description": "Binds an event type to a function for event-driven invocation.",
  "type": "object",
  "properties": {
    "trigger_id": {
      "type": "string",
      "description": "Opaque unique identifier for this trigger."
    },
    "tenant_id": {
      "type": "string"
    },
    "event_type_id": {
      "type": "string",
      "x-gts-ref": "gts.x.core.events.*",
      "description": "GTS ID of the event type to listen for."
    },
    "event_filter_query": {
      "type": "string",
      "description": "Optional filter expression to match specific events. Syntax TBD during EventBroker implementation."
    },
    "dead_letter_queue": {
      "type": "object",
      "description": "Dead letter queue configuration for failed event processing. DLQ management API is out of scope and will be defined during EventBroker implementation.",
      "properties": {
        "enabled": {
          "type": "boolean",
          "default": true,
          "description": "Whether failed events should be moved to DLQ after retry exhaustion."
        },
        "retry_policy": {
          "$ref": "gts://gts.x.core.serverless.retry_policy.v1~",
          "description": "Retry policy before moving to DLQ. Uses exponential backoff with configurable attempts."
        },
        "dlq_topic": {
          "oneOf": [
            {
              "type": "string",
              "x-gts-ref": "gts.x.core.*",
              "description": "GTS type ID of the topic to publish dead-lettered events to."
            },
            {
              "type": "null"
            }
          ],
          "default": null,
          "description": "Optional topic for routing dead-lettered events, or null for the platform-default DLQ topic. Topic type and management are defined by the EventBroker."
        }
      }
    },
    "function_id": {
      "type": "string",
      "x-gts-ref": "gts.x.core.serverless.function.*",
      "description": "GTS ID of the function to invoke."
    },
    "batch": {
      "type": "object",
      "description": "Event batching configuration. When enabled, multiple events are grouped into a single function invocation.",
      "properties": {
        "enabled": {"type": "boolean", "default": false},
        "max_size": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 100, "description": "Maximum events per batch."},
        "max_wait_ms": {"type": "integer", "minimum": 100, "maximum": 60000, "default": 5000, "description": "Maximum time to wait for a full batch before dispatching."}
      }
    },
    "execution_context": {
      "type": "string",
      "enum": ["system", "event_source"],
      "default": "system",
      "description": "Security context for triggered executions: system (platform identity) or event_source (identity of the event producer)."
    },
    "enabled": {
      "type": "boolean",
      "default": true,
      "description": "Whether the trigger is active. When false, events are not processed."
    },
    "created_at": {
      "type": "string",
      "format": "date-time"
    },
    "updated_at": {
      "type": "string",
      "format": "date-time"
    }
  },
  "required": [
    "trigger_id",
    "tenant_id",
    "event_type_id",
    "function_id"
  ]
}
```

### Webhook Trigger

**GTS ID:** `gts.x.core.serverless.webhook_trigger.v1~`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "gts://gts.x.core.serverless.webhook_trigger.v1~",
  "title": "Webhook Trigger",
  "description": "Exposes an HTTP endpoint for external systems to trigger function executions.",
  "type": "object",
  "properties": {
    "trigger_id": {
      "type": "string",
      "description": "Unique identifier within tenant scope."
    },
    "tenant_id": {
      "type": "string"
    },
    "function_id": {
      "type": "string",
      "x-gts-ref": "gts.x.core.serverless.function.*",
      "description": "GTS ID of the function to invoke."
    },
    "authentication": {
      "type": "object",
      "description": "Authentication configuration for incoming webhook requests.",
      "properties": {
        "type": {
          "type": "string",
          "enum": ["none", "hmac_sha256", "hmac_sha1", "basic", "bearer", "api_key"],
          "description": "Authentication method."
        },
        "secret_ref": {
          "type": "string",
          "description": "Reference to the secret used for authentication."
        }
      },
      "required": ["type"]
    },
    "allowed_sources": {
      "type": "array",
      "items": {"type": "string"},
      "description": "Optional IP CIDR allowlist for source IP restrictions."
    },
    "webhook_url": {
      "type": "string",
      "description": "Generated webhook URL (read-only, assigned by the platform)."
    },
    "enabled": {
      "type": "boolean",
      "default": true
    },
    "created_at": {
      "type": "string",
      "format": "date-time"
    },
    "updated_at": {
      "type": "string",
      "format": "date-time"
    }
  },
  "required": ["trigger_id", "tenant_id", "function_id", "authentication"]
}
```

## Governance

### TenantRuntimePolicy

**GTS ID:** `gts.x.core.serverless.tenant_policy.v1~`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "gts://gts.x.core.serverless.tenant_policy.v1~",
  "title": "Tenant Runtime Policy",
  "description": "Tenant-level governance settings for the serverless runtime.",
  "type": "object",
  "properties": {
    "tenant_id": {
      "type": "string",
      "description": "Tenant identifier (also serves as the policy identity)."
    },
    "enabled": {
      "type": "boolean",
      "default": true,
      "description": "Whether the serverless runtime is enabled for this tenant."
    },
    "quotas": {
      "type": "object",
      "description": "Resource quotas for the tenant.",
      "properties": {
        "max_concurrent_executions": {
          "type": "integer",
          "minimum": 1
        },
        "max_definitions": {
          "type": "integer",
          "minimum": 1
        },
        "max_schedules": {
          "type": "integer",
          "minimum": 0
        },
        "max_triggers": {
          "type": "integer",
          "minimum": 0
        },
        "max_execution_history_mb": {
          "type": "integer",
          "minimum": 1
        },
        "max_memory_per_execution_mb": {
          "type": "integer",
          "minimum": 1
        },
        "max_cpu_per_execution": {
          "type": "number",
          "minimum": 0
        },
        "max_execution_duration_seconds": {
          "type": "integer",
          "minimum": 1
        }
      }
    },
    "retention": {
      "type": "object",
      "description": "Retention policies for execution history and audit logs.",
      "properties": {
        "execution_history_days": {
          "type": "integer",
          "minimum": 1,
          "default": 7
        },
        "audit_log_days": {
          "type": "integer",
          "minimum": 1,
          "default": 90
        }
      }
    },
    "policies": {
      "type": "object",
      "description": "Governance policies.",
      "properties": {
        "allowed_runtimes": {
          "type": "array",
          "items": {
            "type": "string",
            "x-gts-ref": "gts.x.core.serverless.adapter.*"
          },
          "description": "List of allowed adapter GTS type IDs (e.g., gts.x.core.serverless.adapter.starlark.v1~). Validated against implementation.adapter at registration time."
        },
        "require_approval_for_publish": {
          "type": "boolean",
          "default": false,
          "description": "When true, function publishing requires administrative approval."
        },
        "allowed_outbound_domains": {
          "type": "array",
          "items": {"type": "string"},
          "description": "Domain allowlist for outbound HTTP calls (e.g., ['*.example.com', 'api.stripe.com']). Empty array means no outbound calls allowed; null/absent means unrestricted."
        }
      }
    },
    "idempotency": {
      "type": "object",
      "description": "Idempotency configuration for invocations.",
      "properties": {
        "deduplication_window_seconds": {
          "type": "integer",
          "minimum": 60,
          "maximum": 2628000,
          "default": 86400,
          "description": "Duration in seconds to retain idempotency keys for deduplication."
        }
      }
    },
    "defaults": {
      "type": "object",
      "description": "Default limits applied to new functions.",
      "properties": {
        "timeout_seconds": {
          "type": "integer",
          "minimum": 1,
          "default": 30
        },
        "memory_mb": {
          "type": "integer",
          "minimum": 1,
          "default": 128
        },
        "cpu": {
          "type": "number",
          "minimum": 0,
          "default": 0.2
        }
      }
    },
    "created_at": {
      "type": "string",
      "format": "date-time"
    },
    "updated_at": {
      "type": "string",
      "format": "date-time"
    }
  },
  "required": [
    "tenant_id",
    "enabled",
    "quotas",
    "retention"
  ]
}
```
