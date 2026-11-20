# WebSocket connection idle timeout enforced

## Setup

- OAGW `websocket_idle_timeout_secs` set short (e.g. 1).
- WebSocket route on an echo upstream.

## Steps

1. Establish a WebSocket connection through OAGW and exchange one message.
2. Send no frames in either direction for longer than the idle timeout.

## Expected behavior

- The gateway sends Close 1001 (Going Away) with reason `Going Away` to the caller; the upstream gets the same Close frame.
- After the Close frame the gateway half-closes the caller's leg (EOF).
- A bare EOF without the Close frame is a failure: the caller's stack would report 1006 (Abnormal Closure), which reads as a dropped connection.
- The Close reason carries no gateway internals.

`testing/e2e/suites/oagw/test_websocket.py` covers the caller side with a private server whose idle timeout is 1 s. Rust tests check the exact Close frame and the EOF after it (`tests/proxy_integration.rs`).
