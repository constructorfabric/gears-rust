# cf-gears-postgres-event-broker-plugin

The PostgreSQL storage backend for the `event-broker` gear: a `backend::Backend` over a database the plugin owns, with segmented per-partition event tables, fenced deduplication and `LISTEN/NOTIFY` head watch.

See [`docs/DESIGN.md`](./docs/DESIGN.md) for the design.
