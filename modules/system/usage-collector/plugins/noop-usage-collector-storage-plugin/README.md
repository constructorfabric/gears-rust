# No-op Usage Collector Storage Plugin

> **Development and testing plugin** — discards all usage records. It is not a production storage backend.

No-op implementation of the usage-collector storage plugin contract: the gateway can resolve a plugin instance and call `create_usage_record`, but no data is retained.

## Purpose

Use this plugin when you want the usage-collector module and emitters to run without configuring a real storage implementation. Typical cases:

- Local development and smoke tests
- CI pipelines that exercise the ingestion path only
- Demos where durable usage history is unnecessary

**Do not use when you need metering data to be stored or queried.**

## Configuration

Add the plugin section under your module configuration:

```yaml
modules:
  noop-usage-collector-storage-plugin:
    config:
      vendor: "cyberfabric"
      priority: 100
```

`vendor` must match the usage-collector gateway configuration so the gateway selects this instance.

## Testing

```bash
cargo test -p cf-noop-usage-collector-storage-plugin
```
