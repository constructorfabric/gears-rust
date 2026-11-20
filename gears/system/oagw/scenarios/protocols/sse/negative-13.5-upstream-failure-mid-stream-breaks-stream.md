# Upstream failure mid-stream breaks the SSE stream

## Setup

SSE route on an upstream that sends some events and then drops the connection without ending the body (no terminating chunk).

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

2. Read the events until the body ends or fails.

## Expected behavior

- The head is `200` with `Content-Type: text/event-stream` and `X-OAGW-Error-Source: upstream`, as sent before the failure.
- The caller gets every event the upstream sent before the failure, unchanged.
- The body to the caller is then aborted, not ended: on HTTP/1.1 the terminating chunk is never sent, so the caller sees a broken stream.
- Nothing is injected after the head: no JSON or problem body, no second status.
- An in-process SDK consumer gets `StreamingError::Stream` as its last item.

`testing/e2e/suites/oagw/test_sse_streaming.py` covers the HTTP side (`/sse/fail` on the mock drops the connection after one event). Rust tests cover the HTTP and SDK sides (`tests/proxy_integration.rs`).
