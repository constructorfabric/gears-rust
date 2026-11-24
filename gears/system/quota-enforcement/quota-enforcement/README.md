# Quota Enforcement

Main gear of the Quota Enforcement (QE) system. This crate holds the
foundation slice: PDP-gated admission, plugin binding, cluster coordination,
fail-closed bootstrap, readiness reporting, and the mount point of the REST
surface in `api-gateway`.

## What the foundation delivers

- **Admission.** One `admit()` boundary runs the public shape check, calls
  `authz-resolver` through `PolicyEnforcer`, and passes the returned
  `AccessScope` through unchanged. Denial, compile failure, and an unreachable
  PDP all fail closed. QE keeps no decision cache.
- **Plugin binding.** The storage plugin is selected by vendor through the
  types registry and resolved as a scoped `ClientHub` client. Nothing is
  cached before bootstrap succeeds.
- **Client contracts.** The gear provides `QuotaManagerApi`,
  `QuotaEnforcementApi` and `QuotaOperatorApi` through `#[toolkit::provides]`:
  in process by default, or a REST client when `clients.provides.<contract>` says
  so. It also hosts their internal REST routes under `/quota-enforcement/v1`,
  served from its own domain service beside the public API.
- **Quota Resolver backend.** The gear publishes a plugin instance of the
  Quota Resolver (`gts.cf.toolkit.plugins.plugin.v1~cf.core.quota_resolver.plugin.v1~`)
  at `init`, so the resolver's gateway, which runs in the same process, finds QE
  by vendor and priority. The plugin type, the quota subject base types and the
  metric base type are published by the resolver gear, so a host that links QE
  links `quota-resolver` too; the two have no start order. Metric types, derived
  from `gts.cf.core.quota.metric.v1~`, and quota subject types, derived from
  `gts.cf.core.quota.subj.v1~`, are registered in the types registry by the
  modules that own them; QE reads them there.
- **Cluster coordination.** The sweeper singletons run under the platform
  `cluster` gear's leader election (ADR-0006). The gear depends on
  `cluster-sdk` only, defines the typed profile `quota-enforcement`, and
  resolves the leader-election facade in its lifecycle entry with the
  `Linearizable` requirement. The adapter owns each election watch, so a
  graceful shutdown resigns and a successor is elected without a TTL wait.
- **Bootstrap.** In the lifecycle entry, before the ready signal: resolve the
  storage plugin, run its `bootstrap()`, resolve the cluster election, and
  confirm the PDP client. Any failure keeps the gear out of rotation and names
  the dependency in `/health`. Once ready, the health check also relays the
  cluster SDK's requirements verdict.
- **Telemetry conventions.** Instruments come from the PRD section 5.16
  catalogue only. Label values are closed enums. No tenant, subject, quota,
  policy, key, token, or caller identifier is a label.

Operational routes land with their features. The route registration function
exists so every feature mounts through the same entry point.

## Configuration

```yaml
gears:
  quota-enforcement:
    config:
      storage_vendor: "constructorfabric"   # selects the storage plugin
      election:
        ttl_secs: 30                        # leadership claim TTL (cluster default)
        max_missed_renewals: 2              # renewal failures before loss (cluster default)
      sweeper_stop_timeout_secs: 10         # budget for a sweep body to stop
      pdp_deadline_ms: 250                  # budget for one PDP evaluation; overrun is 503
      metrics:
        prefix: ""                          # empty: catalogue names verbatim
      catalog:
        read_timeout_ms: 2000               # one types-registry read of a metric or subject type
        read_interval_secs: 60              # scheduled re-read; a revised metric type reaches every replica within it
        unknown_ttl_secs: 10                # how long "the registry does not know this type" is remembered
        unknown_capacity: 10000             # how many such answers are remembered
      resolver_plugin:
        vendor: "constructorfabric"         # vendor of the plugin instance QE publishes for the Quota Resolver
        priority: 100                       # lower wins when the resolver finds several of that vendor
      quotas:
        bulk_max_items: 50                  # most items one bulk create, update, or deactivate
                                            # may carry; 1..=500, and more than 500 is always refused
      notifications:
        max_attempts: 10                    # failed deliveries before an event is dead-lettered
        sink_timeout_ms: 2000               # budget for one sink to take one event

  # The election backend is the operator's choice in the cluster gear's
  # profile YAML. QE code does not change with the backend.
  cluster:
    config:
      profiles:
        quota-enforcement:
          cache: { provider: standalone }   # one process; `postgres` for multi-instance
```

The gear owns no database. The storage plugin declares `db` and gets its own
binding.

## Deployment shapes

The gear declares no `deps = [cluster]` edge (cluster DESIGN section 3.17.7).
The embedded binary links the `cluster` gear, a provider plugin, and
`grpc-hub`. The remote image enables this crate's `grpc-client` feature and
links none of them. Start ordering comes from the cluster gear's `system`
tier, readiness gating from the SDK-submitted consumer registration.

## Runtime state of the foundation

The SQL storage plugin publishes its `QuotaEnforcementStoragePluginV1`
client and plugin instance at `init`, and bootstrap selects it by vendor. The
bootstrap path is exercised in tests against the SDK's complete in-memory
storage double, against the published SQL plugin over SQLite, and against a
real cluster over the standalone backend.

## Design source

`gears/system/quota-enforcement/docs/features/foundation.md` and
`docs/DESIGN.md`, sections 3.2 (Gateway, CoordinationAdapter), 3.3 (Cluster
Coordination), and 3.7 (Bootstrap seeded state).
