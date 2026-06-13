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
- Body is an RFC-9457 Problem Details object with these members:
  - `type` (URI identifying the problem class)
  - `title` (short, stable, human-readable summary — matches the wire error name)
  - `status` (HTTP status code, integer)
  - `detail` (human-readable, request-specific explanation)
  - `instance` (URI/path of the specific occurrence)
  - plus problem-specific extension members (e.g., `valid_range`, `retry_after_secs`, `unseeded`, `expected_previous`)

```http
HTTP/1.1 404 Not Found
Content-Type: application/problem+json

{
  "type": "https://errors.cf.core/events/consumer-group-not-found",
  "title": "ConsumerGroupNotFound",
  "status": 404,
  "detail": "consumer group gts.cf.core.events.consumer_group.v1~deadbeef-... not found",
  "instance": "/v1/consumer_groups/gts.cf.core.events.consumer_group.v1~deadbeef-..."
}
```

## Conventions

- `title` mirrors the wire-level error name (`ConsumerGroupNotFound`, `InvalidInitialPosition`, …).
- No internal implementation detail (stack traces, internal hostnames) appears in `detail`.
- The shorthand `PD` used throughout the scenarios means "a body of exactly this shape".

<!-- No "## Side effects" — defines the envelope; the triggering call's effects belong to that call's scenario. -->
