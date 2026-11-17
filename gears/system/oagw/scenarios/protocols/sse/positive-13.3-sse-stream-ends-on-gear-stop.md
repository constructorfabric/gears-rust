# SSE stream ends when OAGW stops

## Setup

SSE route on an upstream that sends events rarely (well inside `streaming_idle_timeout_secs`).

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

2. After the first event, stop OAGW (graceful shutdown of the server).

## Expected behavior

- The stream to the caller is aborted at once (the response body ends with an error, not a clean end, so the caller can tell the answer was cut): the caller does not wait for the idle timeout, the next upstream event or the end of the server's HTTP drain.
- The upstream stream is closed at once.
- An in-process SDK consumer reading the stream gets `StreamingError::ClosedByGateway` as its last item.
- Open WebSocket relays are closed with Close 1001 (Going Away) on the same shutdown.
- The server's HTTP drain does not wait for the stream: the process exits on its own, without a kill.
- A stream that begins its wait after the shutdown was signalled ends at once as well, also when OAGW started again before the stream was next polled.
- A stream opened before OAGW's first start (the server serves REST before it starts the app gears) ends on shutdown and on stop as well.
- After OAGW starts again, new SSE requests stream normally.

`testing/e2e/suites/oagw/test_sse_shutdown.py` covers the first two bullets and the exit with a private server stopped by SIGTERM under a stream the mock holds open (`/sse/hold`). Rust tests cover the rest (`infra/proxy/streaming.rs`, `gear_tests.rs`, `tests/proxy_integration.rs`).
