# transports/

Scenarios for the two consumption transports: `GET /v1/events:stream` (`multipart/mixed`, default) and `GET /v1/events:sse` (browser-native `text/event-stream`). Covers frame ordering (one event per part), heartbeat cadence, topology frames on rebalance, the `409 PositionsNotSet` backstop, lifecycle errors (`404`/`410`), and content-negotiation guardrails (`406`).

See [../INDEX.md](../INDEX.md#transports--get-v1eventsstream-multipart-get-v1eventssse) for the scenario list.
