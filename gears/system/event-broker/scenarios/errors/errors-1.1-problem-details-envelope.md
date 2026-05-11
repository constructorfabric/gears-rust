# RFC-9457 Problem Details envelope

The canonical error envelope used by every `4xx` / `5xx` response. All error scenarios in other areas reference this shape rather than restating it.

## Request

Any request that triggers an error (here, a representative `404`):

```http
GET /v1/consumer_groups/gts.cf.core.events.consumer_group.v1~deadbeef-0000-0000-0000-000000000000 HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
```

## Expected response

- A `4xx`/`5xx` status with `Content-Type: application/problem+json`.
- Body is an RFC-9457 Problem Details object produced by `toolkit-canonical-errors`. Members:
  - `type` — canonical GTS category URI: `gts://gts.cf.core.errors.err.v1~cf.core.err.<category>.v1~`
  - `title` — category title (e.g. `"Not Found"`, `"Invalid Argument"`, `"Failed Precondition"`)
  - `status` — HTTP status code, integer
  - `detail` — human-readable, request-specific explanation
  - `instance` — URI/path of the specific occurrence (middleware-injected)
  - `trace_id` — distributed tracing correlation (middleware-injected)
  - `context` — category-specific structured payload; always present (may be `{}`); domain identity expressed here via `resource_type`/`resource_name` or violation arrays

```http
HTTP/1.1 404 Not Found
Content-Type: application/problem+json

{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
  "title": "Not Found",
  "status": 404,
  "detail": "consumer group gts.cf.core.events.consumer_group.v1~deadbeef-... not found",
  "instance": "/v1/consumer_groups/gts.cf.core.events.consumer_group.v1~deadbeef-...",
  "trace_id": "<request-trace-id>",
  "context": {
    "resource_type": "gts.cf.core.events.consumer_group.v1~",
    "resource_name": "gts.cf.core.events.consumer_group.v1~deadbeef-..."
  }
}
```

## Conventions

- `type` is always a canonical category URI — never a domain-specific instance path.
- `title` is the canonical category title (`"Not Found"`, `"Invalid Argument"`, etc.), not a domain error slug.
- `context` shape varies by category; see `DESIGN.md §cpt-cf-evbk-interface-error-codes` for the full table.
- No internal implementation detail (stack traces, internal hostnames) appears in `detail` or `context`.
- The shorthand `PD` used throughout the scenarios means "a body of exactly this shape".

<!-- No "## Side effects" — defines the envelope; the triggering call's effects belong to that call's scenario. -->
