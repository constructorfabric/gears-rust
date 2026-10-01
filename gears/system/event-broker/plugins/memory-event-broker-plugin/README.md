# memory-event-broker-plugin

An in-process storage backend for the event-broker gear, backend type
`gts.cf.core.events.backend.v1~cf.core.backend.memory.v1~`.

It meets the backend contract (`event_broker_sdk::backend`) without external
infrastructure: events, fence state and heads live in the process and are lost
when it exits. The gear's tests run on it, and the standalone binary uses it
when no durable backend is configured.

| Contract item | This plugin |
|---|---|
| `append` result | `Heads` |
| `dedup()` | `Fenced`: fence state and events change under one lock |
| `watch()` | `Native`, from an in-process broadcast |
| `provision` | always `Ready` |
| `max_events_per_append()` | 1000 |
| Key to partition | `event_broker_sdk::backend::support::partition_for` |
| Retention | its own worker, every `retention_interval_ms`: single events, oldest first, each as soon as it is past a bound - stored longer ago than `max_age` (by `sequence_time`, never the producer's `occurred_at`), or beyond `max_bytes`, which the partition then holds at most; `max_bytes` below one event empties the partition |

## Settings

```yaml
backends:
  local:
    type: "gts.cf.core.events.backend.v1~cf.core.backend.memory.v1~"
    store: "default"              # instances naming one store share its events; default "default"
    retention_interval_ms: 1000   # how often the retention worker runs; default 1000
```
