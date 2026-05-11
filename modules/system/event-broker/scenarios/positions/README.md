# positions/

Scenarios for `POST /v1/subscriptions/{id}/positions` — the SEEK endpoint serving two roles: (1) pre-stream seed of the starting position per assigned partition, and (2) forward-only ack during streaming. Integer values are last-processed offsets (broker emits from `offset + 1`); `"earliest"` / `"latest"` are server-resolved sentinels. Covers valid-range rejection (`400 InvalidInitialPosition`), backward-seek rejection during streaming (`409 SeekBackwardNotAllowed`), and unassigned-partition rejection (`409 PartitionNotAssigned`).

See [../INDEX.md](../INDEX.md#positions--post-v1subscriptionsidpositions-seek--forward-ack) for the scenario list.
