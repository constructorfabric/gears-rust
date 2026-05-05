# Usage Collector

> **Gateway module** — hosts the Usage Collector gateway in-process: registers the storage-plugin GTS schema with `types-registry`, resolves the active storage plugin by `vendor`, exposes `dyn UsageCollectorClientV1` in `ClientHub` for the outbox delivery path, and serves a REST API for out-of-process emitters.

ModKit module `usage-collector`: central ingest for usage observations (`UsageRecord`) from the outbox pipeline (`usage-emitter`) and delegation to the selected `UsageCollectorPluginClientV1` implementation.

## Overview

At startup the module:

- Registers `UsageCollectorStoragePluginSpecV1` in the types registry so storage backends can be declared as GTS instances.
- Builds the domain `Service` that lists plugin instances, picks one for the configured `vendor`, and forwards `create_usage_record` calls with a bounded timeout.
- Registers **`UsageCollectorLocalClient`** as `dyn UsageCollectorClientV1` so code in the **same binary** can call `create_usage_record` and `get_module_config` (which filters the metrics whitelist to only the metrics allowed for the calling module) without a network hop.
- Initializes the embedded **`UsageEmitter`** (from `usage-emitter`) and registers it in `ClientHub`; the emitter handles authorization (PDP) and the outbox delivery path before forwarding records to the local client.

Source modules should emit through **`usage-emitter`** (PDP + outbox). This crate implements the gateway side of `UsageCollectorClientV1::create_usage_record` and the storage-plugin selection logic.

## Dependencies

- **At least one storage plugin** — a module that registers `dyn UsageCollectorPluginClientV1` in `ClientHub` for the GTS instance id chosen for your `vendor`. For dev/tests, see **`cf-noop-usage-collector-storage-plugin`**.

## Configuration

`vendor` must match the `vendor` field on the storage plugin GTS content so `choose_plugin_instance` selects the right instance. `plugin_timeout` bounds each `create_usage_record` call; exceeded waits become `UsageCollectorError::PluginTimeout`. `emitter` holds nested `UsageEmitterConfig` for outbox and authorization tuning. `metrics` is a whitelist of allowed metric names, each with a `kind` (`gauge` or `counter`) and an optional `modules` list (if absent, all modules may emit that metric).

```yaml
modules:
  usage-collector:
    config:
      vendor: "cyberfabric"
      plugin_timeout: "5s"
      emitter:
        # UsageEmitterConfig fields (outbox, auth tuning)
      metrics:
        "storage.bytes_used":
          kind: "gauge"
          modules: ["storage-service"]   # omit to allow all modules
        "api.requests":
          kind: "counter"
```

## Testing

```bash
cargo test -p cf-usage-collector
```
