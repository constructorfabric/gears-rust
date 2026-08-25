# Fragmented message of arbitrary size is relayed without a per-message cap

## Setup

- Standard WebSocket proxy connection. No per-message or per-frame size limit
  exists to configure (removed - see
  `../../../docs/features/0007-cpt-cf-oagw-feature-streaming.md`).

## Steps

1. Client sends one logical message as many small continuation frames whose
   combined size is large (e.g., well beyond any size that used to be
   configurable via the old `websocket_max_frame_size_bytes` setting).
2. Upstream receives the reassembled stream.

## Expected behavior

- The full message is relayed successfully; no Close 1009 is ever sent, at any
  point, regardless of total size.

## What to check

- This is intentional, current behavior, not a gap: size limiting was already
  bypassable by fragmentation before this setting was removed (a single-frame
  limit was never a real message-size limit), so this scenario documents the
  behavior change rather than testing a regression.
