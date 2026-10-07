# cf-gears-toolkit-trace-context

W3C Trace Context propagation, shared across CF/Gears crates.

This is the single home for the trace-context wire logic that the HTTP client
crate (`cf-gears-toolkit-http`) and the core toolkit (`cf-gears-toolkit`) used to
keep their own drifting copies of:

- `parse_trace_id` — validate a W3C `traceparent` header and lift its trace-id.
- `get_traceparent` — read the raw `traceparent` header value.
- `set_parent_from_headers` — **inbound**: seed a `tracing::Span`'s parent from
  the caller's W3C trace context (via the process-global propagator, `otel`
  feature) and record `trace_id` / `parent.trace_id` on it for log correlation.
- `inject_current_span` — **outbound**: inject the active OpenTelemetry context
  into a request's headers as `traceparent`.
- `current_trace_id` / `current_trace_ids` — read the active OTel span's ids.
  These are live-context readers, so they resolve only with the `otel` feature:
  `current_trace_id` returns `None` without it, and `current_trace_ids` is
  `otel`-only.
- `extract_trace_id` — resolve the wire trace-id: live OTel span → validated
  inbound `traceparent`. `cf-gears-toolkit` re-exports it as
  `toolkit::api::extract_trace_id`.

Keeping one authoritative implementation means the gateway and the gears cannot
drift on what counts as a valid `traceparent`, on which ids a request span
carries, or on how context is propagated on egress.

## Features

- `otel` (off by default): pull in `opentelemetry` + `tracing-opentelemetry` so
  `set_parent_from_headers` continues the caller's trace and
  `inject_current_span` propagates the active context. Without it,
  `set_parent_from_headers` still records the ids for log correlation (fresh
  root) and `inject_current_span` is a no-op.
