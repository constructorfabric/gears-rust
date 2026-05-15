//! Standalone fluent builder for [`UsageRecord`].
//!
//! [`UsageRecordBuilder`] is intentionally "dumb": it holds optional fields, exposes
//! `with_*` setters, and assembles a [`UsageRecord`] via [`Self::build`] without
//! consulting any [`UsageEmitter`](crate::UsageEmitter) state.
//!
//! Two construction paths are supported:
//!
//! - [`UsageEmitter::usage_record_builder`](crate::UsageEmitter::usage_record_builder)
//!   returns a builder with `module`, `tenant_id`, `resource_id` / `resource_type`,
//!   `subject` (when present on the authorized handle), `metric` (with `kind`
//!   resolved from the authorized allowed-metrics list), and `value` already
//!   populated.
//! - [`UsageRecordBuilder::new`] starts an empty builder for callers that need to
//!   assemble a record outside of an authorized handle (e.g. tests).
//!
//! Either way, the resulting [`UsageRecord`] is then passed to
//! [`UsageEmitter::enqueue`](crate::UsageEmitter::enqueue) or
//! [`UsageEmitter::enqueue_in`](crate::UsageEmitter::enqueue_in)
//! for validation and outbox enqueue.

use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;
use usage_collector_sdk::error::UsageRecordError;
use usage_collector_sdk::models::{Subject, UsageKind, UsageRecord};
use uuid::Uuid;

use crate::error::UsageEmitterError;

/// Fluent builder that assembles a [`UsageRecord`] from independent setters.
///
/// The builder performs no authorization, no metric-allowed-list checking, and no
/// transactional work. Validation happens at
/// [`UsageEmitter::enqueue`](crate::UsageEmitter::enqueue) /
/// [`enqueue_in`](crate::UsageEmitter::enqueue_in) time.
#[derive(Debug, Default, Clone)]
pub struct UsageRecordBuilder {
    module: Option<String>,
    tenant_id: Option<Uuid>,
    metric: Option<String>,
    kind: Option<UsageKind>,
    value: Option<f64>,
    resource_id: Option<Uuid>,
    resource_type: Option<String>,
    subject: Option<Subject>,
    idempotency_key: Option<String>,
    timestamp: Option<DateTime<Utc>>,
    metadata: Option<JsonValue>,
}

impl UsageRecordBuilder {
    /// Starts an empty builder. All required fields must be set via `with_*` before
    /// calling [`Self::build`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the source module name.
    #[must_use]
    pub fn with_module(mut self, module: impl Into<String>) -> Self {
        self.module = Some(module.into());
        self
    }

    /// Sets the tenant that owns the usage observation.
    #[must_use]
    pub fn with_tenant_id(mut self, tenant_id: Uuid) -> Self {
        self.tenant_id = Some(tenant_id);
        self
    }

    /// Sets the metric name and its kind (gauge vs counter).
    #[must_use]
    pub fn with_metric(mut self, name: impl Into<String>, kind: UsageKind) -> Self {
        self.metric = Some(name.into());
        self.kind = Some(kind);
        self
    }

    /// Sets the numeric value.
    #[must_use]
    pub fn with_value(mut self, value: f64) -> Self {
        self.value = Some(value);
        self
    }

    /// Sets the metered resource (instance id + logical type).
    #[must_use]
    pub fn with_resource(mut self, id: Uuid, resource_type: impl Into<String>) -> Self {
        self.resource_id = Some(id);
        self.resource_type = Some(resource_type.into());
        self
    }

    /// Sets the subject (user or service) acting on the resource.
    ///
    /// Subject is optional and may be omitted entirely by simply not calling
    /// this setter; the resulting record carries `subject = None`, and the
    /// factory's `.authorize()` step omits `SUBJECT_ID` / `SUBJECT_TYPE`
    /// from the PDP request accordingly. Construct the [`Subject`] via
    /// [`Subject::new`] (id only) or [`Subject::with_type`] (id + logical
    /// type) before passing it in.
    #[must_use]
    pub fn with_subject(mut self, subject: Subject) -> Self {
        self.subject = Some(subject);
        self
    }

    /// Sets a caller-provided idempotency key.
    ///
    /// Required (non-empty) for counter metrics — enforced by
    /// [`UsageEmitter::enqueue_in`](crate::UsageEmitter::enqueue_in).
    /// Optional for gauge metrics; when omitted or blank, the emitter generates a UUID
    /// at enqueue time.
    #[must_use]
    pub fn with_idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = Some(key.into());
        self
    }

    /// Sets the observation timestamp. If omitted, [`Utc::now`] is used at [`Self::build`] time.
    #[must_use]
    pub fn with_timestamp(mut self, timestamp: DateTime<Utc>) -> Self {
        self.timestamp = Some(timestamp);
        self
    }

    /// Sets optional metadata JSON. The serialized-size limit is enforced by the
    /// emitter at enqueue time using the value `ModuleConfig.max_metadata_bytes`
    /// returned by `get_module_config`.
    #[must_use]
    pub fn with_metadata(mut self, metadata: JsonValue) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Assembles a [`UsageRecord`].
    ///
    /// # Errors
    ///
    /// Returns [`UsageEmitterError::InvalidArgument`] when any required field
    /// (`module`, `tenant_id`, `metric`, `value`, `resource_id`, `resource_type`)
    /// is missing; the error's constraint message enumerates the missing fields.
    pub fn build(self) -> Result<UsageRecord, UsageEmitterError> {
        let Self {
            module,
            tenant_id,
            metric,
            kind,
            value,
            resource_id,
            resource_type,
            subject,
            idempotency_key,
            timestamp,
            metadata,
        } = self;

        match (
            module,
            tenant_id,
            metric,
            kind,
            value,
            resource_id,
            resource_type,
        ) {
            (
                Some(module),
                Some(tenant_id),
                Some(metric),
                Some(kind),
                Some(value),
                Some(resource_id),
                Some(resource_type),
            ) => Ok(UsageRecord {
                module,
                tenant_id,
                metric,
                kind,
                value,
                resource_id,
                resource_type,
                subject,
                idempotency_key: idempotency_key.unwrap_or_default(),
                timestamp: timestamp.unwrap_or_else(Utc::now),
                metadata,
            }),
            (module, tenant_id, metric, kind, value, resource_id, resource_type) => {
                let mut missing: Vec<&'static str> = Vec::new();
                if module.is_none() {
                    missing.push("module");
                }
                if tenant_id.is_none() {
                    missing.push("tenant_id");
                }
                if metric.is_none() || kind.is_none() {
                    missing.push("metric");
                }
                if value.is_none() {
                    missing.push("value");
                }
                if resource_id.is_none() {
                    missing.push("resource_id");
                }
                if resource_type.is_none() {
                    missing.push("resource_type");
                }
                Err(UsageRecordError::invalid_argument()
                    .with_constraint(format!(
                        "UsageRecordBuilder is missing required field(s): {}",
                        missing.join(", ")
                    ))
                    .create())
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "usage_record_builder_tests.rs"]
mod usage_record_builder_tests;
