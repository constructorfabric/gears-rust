use std::sync::Arc;
use std::time::Instant;

use modkit_db::Db;
use modkit_db::outbox::Outbox;
use modkit_db::secure::DBRunner;
use tracing::{debug, error};
use usage_collector_sdk::error::UsageRecordError;
use usage_collector_sdk::models::{AllowedMetric, Subject, UsageKind, UsageRecord};
use uuid::Uuid;

use crate::config::UsageEmitterConfig;
use crate::domain::usage_record_builder::UsageRecordBuilder;
use crate::error::UsageEmitterError;

/// Upper bound on free-form string fields inside a [`UsageRecord`] enqueued by
/// the emitter. Without this, a caller (REST gateway or in-process module) could
/// emit a multi-megabyte `module` / `metric` / `resource_type` / `idempotency_key` /
/// `subject.type` that would be serialized into the outbox payload, persisted, and
/// fanned out to every downstream consumer. 4 KiB is generous for legitimate
/// identifiers (UUIDs, dotted names) and small enough that nothing downstream is
/// asked to store a pathological value.
///
/// Enforced at the emitter rather than at the REST DTO boundary because the
/// emitter is the only common gate — in-process modules construct
/// [`UsageRecord`]s and call [`UsageEmitter::enqueue`]/[`enqueue_in`] without
/// going through the REST layer.
const MAX_RECORD_FIELD_LEN: usize = 4096;

/// Emitter state after successful PDP authorization; call [`Self::enqueue`] or
/// [`Self::enqueue_in`] on the returned handle.
///
/// Constructed via [`crate::UsageEmitterFactory::authorize`]; callers cannot forge handles.
pub struct UsageEmitter {
    pub(crate) config: Arc<UsageEmitterConfig>,
    pub(crate) db: Db,
    pub(crate) outbox: Arc<Outbox>,
    pub(crate) module: String,
    pub(crate) tenant_id: Uuid,
    pub(crate) resource_id: Uuid,
    pub(crate) resource_type: String,
    pub(crate) allowed_metrics: Vec<AllowedMetric>,
    pub(crate) max_metadata_bytes: u32,
    pub(crate) subject: Option<Subject>,
    pub(crate) issued_at: Instant,
}

impl UsageEmitter {
    /// Enqueue a usage record using this emitter's database connection.
    ///
    /// # Errors
    ///
    /// Returns [`UsageEmitterError`] when obtaining a connection fails, the authorization handle
    /// expired, or the outbox enqueue fails.
    pub async fn enqueue(&self, record: UsageRecord) -> Result<(), UsageEmitterError> {
        // Outbox carve-out: `modkit_db::outbox::Outbox::enqueue` is framework-internal
        // transactional messaging that targets `modkit_outbox_incoming`/`_outbox`. Those tables
        // have no tenant or resource columns, so `AccessScope`/`SecureConn` filtering does not
        // apply; the outbox API takes any `DBRunner` directly. Using `db.conn()` here is
        // intentional and consistent with the modkit-db outbox contract.
        let conn = self.db.conn().map_err(|e| {
            UsageEmitterError::internal(format!("{:#}", anyhow::Error::new(e))).create()
        })?;
        self.enqueue_in(&conn, record).await
    }

    /// Enqueue a usage record on the given database runner (connection or transaction).
    ///
    /// # Errors
    ///
    /// Returns [`UsageEmitterError`] when the authorization handle expired, the record does not
    /// match the authorized tenant/resource, the metric is not allowed for this module, a counter
    /// record has a negative value, or the outbox enqueue fails.
    #[tracing::instrument(
        skip_all,
        fields(
            module = %self.module,
            tenant_id = %self.tenant_id,
            resource_id = %self.resource_id,
            resource_type = %self.resource_type,
            metric = %record.metric,
        ),
    )]
    pub async fn enqueue_in(
        &self,
        db: &(dyn DBRunner + Sync),
        mut record: UsageRecord,
    ) -> Result<(), UsageEmitterError> {
        Self::validate_field_lengths(&record)?;
        self.validate_authorization_freshness()?;
        self.validate_authorized_tenant(&record)?;
        self.validate_authorized_resource(&record)?;
        // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-6
        self.validate_authorized_module(&record)?;
        self.validate_authorized_subject(&record)?;
        // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-6
        self.validate_allowed_metric(&record)?;
        self.validate_metric_kind(&record)?;
        Self::validate_counter_value(&record)?;
        Self::validate_counter_idempotency_key(&record)?;
        self.validate_metadata_size(&record)?;
        // Generate a UUID idempotency key for gauge records when the caller
        // omitted (or blanked) one. Counter records keep an empty key here so
        // `validate_counter_idempotency_key` can reject them with the canonical
        // `InvalidArgument` error.
        Self::generate_gauge_idempotency_key(&mut record);

        // Fold the full 128-bit tenant UUID down to 32 bits via XOR before
        // taking the partition modulo. Using only the leading byte is uniform
        // for v4 but clusters under v7 (time-prefixed), where IDs minted in
        // the same ms share leading bytes and would all land on the same
        // partition.
        let n = record.tenant_id.as_u128();
        #[allow(clippy::cast_possible_truncation)]
        let folded = (n as u32) ^ ((n >> 32) as u32) ^ ((n >> 64) as u32) ^ ((n >> 96) as u32);
        let partition = folded % u32::from(self.config.outbox_partition_count);

        // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-8
        let payload = serde_json::to_vec(&record).map_err(|e| {
            UsageEmitterError::internal(format!(
                "payload serialization failed: {:#}",
                anyhow::Error::new(e)
            ))
            .create()
        })?;
        // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-8

        // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-9
        self.outbox
            .enqueue(
                db,
                &self.config.outbox_queue,
                partition,
                payload,
                "usage-collector.record.v1",
            )
            .await
            .map_err(|e| {
                // The canonical `ServiceUnavailable` error has no `#[source]` slot, so
                // wrapping the outbox error in `Problem.detail` strips the original
                // `#[source]` chain (sqlx, sea-orm, etc.). Log the original error here
                // so operators retain the full chain for debugging.
                error!(error = ?e, "outbox enqueue failed");
                UsageEmitterError::service_unavailable()
                    .with_detail(format!("outbox error: {:#}", anyhow::Error::new(e)))
                    .create()
            })?;
        // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-9

        debug!(%self.tenant_id, "usage record enqueued");

        // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-10
        Ok(())
        // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-10
    }

    fn validate_field_lengths(record: &UsageRecord) -> Result<(), UsageEmitterError> {
        fn check(name: &str, value: &str) -> Result<(), UsageEmitterError> {
            if value.len() > MAX_RECORD_FIELD_LEN {
                return Err(UsageRecordError::invalid_argument()
                    .with_constraint(format!(
                        "{name} length {} exceeds the {MAX_RECORD_FIELD_LEN}-byte limit",
                        value.len(),
                    ))
                    .create());
            }
            Ok(())
        }
        check("module", &record.module)?;
        check("metric", &record.metric)?;
        check("resource_type", &record.resource_type)?;
        check("idempotency_key", &record.idempotency_key)?;
        if let Some(s) = &record.subject
            && let Some(ty) = &s.r#type
        {
            check("subject.type", ty)?;
        }
        Ok(())
    }

    fn validate_authorization_freshness(&self) -> Result<(), UsageEmitterError> {
        // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-1
        let elapsed = self.issued_at.elapsed();
        // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-1

        // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-2
        if elapsed > self.config.authorization_max_age {
            // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-2a
            return Err(UsageEmitterError::unauthenticated()
                .with_reason("emit authorization token has expired")
                .create());
            // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-2a
        }
        // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-2
        Ok(())
    }

    fn validate_authorized_tenant(&self, record: &UsageRecord) -> Result<(), UsageEmitterError> {
        if record.tenant_id != self.tenant_id {
            return Err(UsageRecordError::permission_denied()
                .with_reason("usage record tenant_id does not match authorized tenant")
                .create());
        }
        Ok(())
    }

    fn validate_authorized_resource(&self, record: &UsageRecord) -> Result<(), UsageEmitterError> {
        if record.resource_id != self.resource_id {
            return Err(UsageRecordError::permission_denied()
                .with_reason("usage record resource_id does not match authorized resource")
                .create());
        }

        if record.resource_type != self.resource_type {
            return Err(UsageRecordError::permission_denied()
                .with_reason("usage record resource_type does not match authorized resource")
                .create());
        }

        Ok(())
    }

    fn validate_authorized_module(&self, record: &UsageRecord) -> Result<(), UsageEmitterError> {
        if record.module != self.module {
            return Err(UsageRecordError::permission_denied()
                .with_reason("record module does not match authorized token")
                .create());
        }
        Ok(())
    }

    fn validate_authorized_subject(&self, record: &UsageRecord) -> Result<(), UsageEmitterError> {
        if record.subject != self.subject {
            return Err(UsageRecordError::permission_denied()
                .with_reason("record subject does not match authorized token")
                .create());
        }
        Ok(())
    }

    fn validate_allowed_metric(&self, record: &UsageRecord) -> Result<(), UsageEmitterError> {
        // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-3
        let metric_allowed = self.allowed_metrics.iter().any(|m| m.name == record.metric);
        // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-3

        // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-4
        if !metric_allowed {
            // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-4a
            return Err(UsageRecordError::permission_denied()
                .with_reason(format!(
                    "metric not allowed for this module: {}",
                    record.metric
                ))
                .create());
            // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-4a
        }
        // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-4
        Ok(())
    }

    fn validate_metric_kind(&self, record: &UsageRecord) -> Result<(), UsageEmitterError> {
        if let Some(allowed) = self
            .allowed_metrics
            .iter()
            .find(|m| m.name == record.metric)
            && allowed.kind != record.kind
        {
            return Err(UsageRecordError::invalid_argument()
                .with_constraint(format!(
                    "metric '{}' expects kind {:?} but record specifies {:?}",
                    record.metric, allowed.kind, record.kind
                ))
                .create());
        }
        Ok(())
    }

    fn validate_counter_value(record: &UsageRecord) -> Result<(), UsageEmitterError> {
        if !record.value.is_finite() {
            return Err(UsageRecordError::invalid_argument()
                .with_constraint(format!("value must be finite, got: {}", record.value))
                .create());
        }
        if record.kind == UsageKind::Counter && record.value < 0.0 {
            return Err(UsageRecordError::invalid_argument()
                .with_constraint(format!(
                    "counter value must be non-negative, got: {}",
                    record.value
                ))
                .create());
        }
        Ok(())
    }

    fn validate_counter_idempotency_key(record: &UsageRecord) -> Result<(), UsageEmitterError> {
        // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-5
        if record.kind == UsageKind::Counter && record.idempotency_key.trim().is_empty() {
            // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-5a
            return Err(UsageRecordError::invalid_argument()
                .with_constraint("counter records require a non-empty idempotency_key")
                .create());
            // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-5a
        }
        // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-5
        Ok(())
    }

    fn validate_metadata_size(&self, record: &UsageRecord) -> Result<(), UsageEmitterError> {
        // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-7
        if let Some(ref metadata) = record.metadata {
            let len = serde_json::to_vec(metadata)
                .map_err(|e| {
                    UsageRecordError::invalid_argument()
                        .with_constraint(format!("metadata is not serializable: {e}"))
                        .create()
                })?
                .len();
            if len > self.max_metadata_bytes as usize {
                // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-7a
                return Err(UsageRecordError::invalid_argument()
                    .with_constraint(format!(
                        "metadata byte length {len} exceeds the {limit}-byte limit",
                        limit = self.max_metadata_bytes
                    ))
                    .create());
                // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-7a
            }
        }
        // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-7
        Ok(())
    }

    /// Replaces a blank/whitespace idempotency key on gauge records with a freshly
    /// generated UUID; counter records pass through unchanged so the counter-key
    /// invariant is enforced by [`UsageEmitter::validate_counter_idempotency_key`].
    fn generate_gauge_idempotency_key(record: &mut UsageRecord) {
        // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-5b
        if record.kind == UsageKind::Gauge && record.idempotency_key.trim().is_empty() {
            record.idempotency_key = Uuid::new_v4().to_string();
        }
        // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue:p1:inst-enq-5b
    }

    /// Starts a [`UsageRecordBuilder`] prefilled from this authorization handle.
    ///
    /// The returned builder has `module`, `tenant_id`, `resource_id`,
    /// `resource_type`, `subject`, `metric`, `kind`, and
    /// `value` already set. The caller may then chain optional setters
    /// ([`UsageRecordBuilder::with_idempotency_key`],
    /// [`UsageRecordBuilder::with_timestamp`], [`UsageRecordBuilder::with_metadata`])
    /// and call [`UsageRecordBuilder::build`] to obtain a [`UsageRecord`] suitable
    /// for [`Self::enqueue`] / [`Self::enqueue_in`].
    ///
    /// # Errors
    ///
    /// Returns [`UsageEmitterError::PermissionDenied`] when `metric` is not in the
    /// allowed-metrics list for this module — propagating the same metric-allowed
    /// invariant as [`Self::enqueue_in`] so callers fail early rather than after
    /// building the record.
    pub fn usage_record_builder(
        &self,
        metric: impl Into<String>,
        value: f64,
    ) -> Result<UsageRecordBuilder, UsageEmitterError> {
        let metric = metric.into();
        let Some(allowed) = self.allowed_metrics.iter().find(|m| m.name == metric) else {
            return Err(UsageRecordError::permission_denied()
                .with_reason(format!("metric not allowed for this module: {metric}"))
                .create());
        };

        let mut builder = UsageRecordBuilder::new()
            .with_module(self.module.clone())
            .with_tenant_id(self.tenant_id)
            .with_resource(self.resource_id, self.resource_type.clone())
            .with_metric(metric, allowed.kind)
            .with_value(value);
        if let Some(s) = &self.subject {
            builder = builder.with_subject(s.clone());
        }
        Ok(builder)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "emitter_tests.rs"]
mod emitter_tests;
