# SSE idle timeout ends the stream with an error

## Setup

- OAGW `streaming_idle_timeout_secs` set short (e.g. 1).
- SSE route on an upstream that sends one event and then stays silent with the connection open.

## Steps

1. Start an SSE request:

   ```http
   POST /api/oagw/v1/proxy/<alias>/v1/chat/completions HTTP/1.1
   Host: oagw.example.com
   Authorization: Bearer <tenant-token>
   Accept: text/event-stream
   Content-Type: application/json

   {"stream": true}
   ```

2. Read the first event, then wait past the idle timeout without the upstream sending anything.

## Expected behavior

- The head is `200` with `Content-Type: text/event-stream`; the first event arrives unchanged.
- After the idle timeout the body to the caller is aborted, not ended: on HTTP/1.1 the terminating chunk is never sent, so the caller sees an incomplete body and cannot take the cut stream for a complete one.
- Nothing is written after the last event: no JSON error, no status change (the head is already sent).
- The gateway closes the upstream connection.
- The stream is not cut before the idle timeout.
- An in-process SDK consumer gets `StreamingError::IdleTimeout` as its last item; a raw body consumer can downcast the body error to `StreamIdleTimeout`.

`testing/e2e/suites/oagw/test_sse_streaming.py` covers the HTTP side with a private server whose idle timeout is 1 s. Rust tests cover the SDK error and the timer (`infra/proxy/streaming.rs`, `tests/proxy_integration.rs`).
