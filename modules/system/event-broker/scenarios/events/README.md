# events/

Scenarios for producer-side submission: `POST /v1/events` (single) and `POST /v1/events:batch` (atomic per topic). Covers async (`202`) vs sync (`Sync-Wait`/`?wait=persisted` → `201`), chained / monotonic producer modes, idempotency-key dedup, schema validation, and the rejection paths (`412 SequenceViolation`, `413 BatchTooLarge`, `429 RateLimitExceeded`, mixed-partition batch).

See [../INDEX.md](../INDEX.md#events--post-v1events-post-v1eventsbatch) for the scenario list.
