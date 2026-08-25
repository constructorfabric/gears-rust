# Stalled caller does not block upstream's graceful close

## Setup

- WebSocket idle timeout (or graceful server shutdown) is configured as usual.
- Caller stops reading from its socket entirely after the handshake (its TCP
  receive buffer fills and stays full - it never acknowledges anything the
  gateway sends), while the connection itself stays open.

## Steps

1. Establish WebSocket connection, upstream accepts.
2. Caller stops draining its socket (simulates a hung/unresponsive client).
3. Trigger teardown: either let the idle timeout elapse, or initiate graceful
   server shutdown.

## Expected behavior

- Upstream still receives Close 1001 ("Going Away") and has its connection
  half-closed, within `websocket_close_timeout_secs` of teardown starting -
  regardless of whether the caller ever acknowledges its own Close frame.
- The caller being unresponsive costs only the caller's own close attempt the
  grace period; it does not consume the upstream's share of it.

## What to check

- Announcing close to each side is independent - one side stalling must not
  starve the other side's close notification.
