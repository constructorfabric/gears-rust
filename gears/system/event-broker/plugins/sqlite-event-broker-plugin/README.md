# SQLite event-broker plugin

`cf-gears-sqlite-event-broker-plugin` (lib `sqlite_event_broker_plugin`) is the
durable storage backend for the `event-broker` gear: the append-only
`(topic, partition)` event log, the per-partition bookkeeping that assigns
sequences, and the retention pass that keeps every partition bounded.

It implements `event_broker_sdk::backend::Backend` (feature 0007) and depends
on the SDK only, never on the gear.

## Lifecycle

Not a ToolKit `RunnableCapability` and not a `Gear`. Following the cluster
plugins, it exposes a `backend::Provider` linked into the build. The gear
builds one backend per `backends` registry entry whose `type` is
`gts.cf.core.events.backend.v1~cf.core.backend.sqlite.v1~`, passing the
settings written beside that `type` (feature 0005 §2.1, feature 0007 §2.7):

```yaml
backends:
  sqlite-main:
    type: "gts.cf.core.events.backend.v1~cf.core.backend.sqlite.v1~"
    path: "~/eb/events.db"   # or ":memory:", the default
```

`path` is the only setting; an unknown key is rejected.

The backend owns no task and no timer. Its retention pass is a trait method the
gear drives on its own tick, so a test forces a pass deterministically instead
of sleeping and hoping a background thread ran.

## Schema

The backend owns its own database, separate from the gear's: building it opens
the event log at `path` and applies its own tables there - `event_broker_event`
and `event_broker_partition_state`. The same database stores each topic's log
id (feature 0006 §2.3).
