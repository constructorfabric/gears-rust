# WebSocket frame relayed intact when delivered across multiple reads

## Setup

- Standard WebSocket proxy connection, established and upgraded.

## Steps

1. Client sends one WebSocket frame's header bytes and payload bytes as two
   separate writes, with a short delay between them (simulating ordinary TCP
   segmentation - nothing here should require frame-aligned reads).
2. Upstream reads from its side of the tunnel.

## Expected behavior

- The frame arrives at upstream byte-identical to what was sent, uncorrupted
  and unsplit at any boundary other than the one the sender chose.

## What to check

- The gateway relays raw bytes without needing to see a whole frame at once -
  this must hold no matter how the underlying reads happen to chunk the bytes.
