# Usage Collector SDK

Transport-agnostic contracts for the usage-collector module family.

## What this crate provides

| Item | Description |
|------|-------------|
| `UsageCollectorClientV1` | Ingest trait implemented by client modules (`usage-collector`, `usage-collector-rest-client`). **Never registered in `ClientHub`** to prevent unauthorized usage emission. |
| `UsageCollectorPluginClientV1` | Storage-plugin trait implemented by backend plugins (`create_usage_record`). |
| `UsageRecord`, `UsageKind` | Ingest-side models. `UsageRecord` fields are public for direct construction, serde, and tests; it is an **unvalidated data carrier** — emitters enforce invariants (see [Validation contract](#validation-contract)). |
| `ModuleConfig`, `AllowedMetric` | Per-module configuration returned by `get_module_config()`; `AllowedMetric` holds a metric name and its `UsageKind`. |
| `UsageCollectorError` | Canonical error type shared by both traits). |
| `UsageRecordError` | Resource-scoped error builder for usage record operations. |
| `UsageCollectorPluginSpecV1` | GTS schema for storage plugin registration. |

## Usage

### Querying module config

Source modules can fetch their allowed metrics at init time via `UsageCollectorClientV1`:

```rust
use usage_collector_sdk::UsageCollectorClientV1;

let config = collector.get_module_config("my_module").await?;
for metric in &config.allowed_metrics {
    println!("{}: {:?}", metric.name, metric.kind);
}
```

### Building a `UsageRecord` directly

For tests, plugins, or offline construction, set fields on `UsageRecord` directly (public struct fields). **Source modules should not do this in production** — use the `usage-emitter` crate, which performs the validation listed under [Validation contract](#validation-contract) below.

```rust
use chrono::Utc;
use usage_collector_sdk::{Subject, UsageKind, UsageRecord};
use uuid::Uuid;

let record = UsageRecord {
    module: "my_module".to_owned(),
    tenant_id: Uuid::new_v4(),
    metric: "requests".to_owned(),
    kind: UsageKind::Counter,
    value: 1.0,
    resource_id: Uuid::new_v4(),
    resource_type: "resource_type".to_owned(),
    subject: Some(Subject::with_type(Uuid::new_v4(), "user")),
    idempotency_key: Uuid::new_v4().to_string(),
    timestamp: Utc::now(),
    metadata: None,
};
```

### Implementing a storage plugin

```rust
use async_trait::async_trait;
use modkit_odata::Page;
use usage_collector_sdk::{UsageCollectorError, UsageCollectorPluginClientV1, UsageRecord};

struct MyStoragePlugin { /* ... */ }

#[async_trait]
impl UsageCollectorPluginClientV1 for MyStoragePlugin {
    async fn create_usage_record(
        &self,
        record: UsageRecord,
    ) -> Result<(), UsageCollectorError> {
        // implementation goes here
        Ok(())
    }
}
```

## Validation contract

`UsageRecord` is an unvalidated data carrier. The type does not enforce:

- finite `value` (NaN / ±∞ pass through — some storage backends reject them on the wire);
- `metadata` size — the limit is configured by the collector and returned via `get_module_config` as `ModuleConfig.max_metadata_bytes`; the emitter enforces it (a value of `0` disables metadata entirely);
- `idempotency_key` length or format.

The **emitter is the validation gateway** — `usage-emitter` rejects records that violate these invariants before they reach `UsageCollectorClientV1`. Storage plugins MAY perform additional defensive checks but should not rely on this type to enforce them.

Fields are deliberately kept public (no builder) to keep the construction surface minimal for serde, tests, and plugin code; the trade-off is that consumers MUST route production emission through `usage-emitter`.

## Security invariant

`UsageCollectorClientV1` is **never** registered in `ClientHub`. It is passed directly to the emitter via constructor, ensuring the sole path to the collector is through a PDP-authorized emitter.
