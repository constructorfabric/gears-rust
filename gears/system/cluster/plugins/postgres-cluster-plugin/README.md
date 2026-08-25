# cf-postgres-cluster-plugin

The Postgres backend plugin for the `cluster` gear: a native `ClusterCacheBackend`
over a `sqlx::PgPool` plus a native `DistributedLockBackend` over
`pg_advisory_lock`. Recommended for multi-instance, no-K8s deployments.

See [`docs/DESIGN.md`](./docs/DESIGN.md) for the full design and
[`docs/TESTING.md`](./docs/TESTING.md) for the test plan.
