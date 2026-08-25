// Created: 2026-08-13 by Constructor Tech
//! A tracing-backed stand-in for the platform audit destination.

use async_trait::async_trait;
use tracing::info;

use crate::audit::{AuditEmitter, AuditRecord, AuditValue};
use crate::domain::error::DomainError;

/// Records audit entries to structured tracing.
///
/// # This is not the platform Audit Subsystem
///
/// DESIGN.md §4.2 requires a **synchronous, fail-closed** write to the
/// platform's immutable external Audit Subsystem, and §4.3 serves per-`(setting,
/// scope)` history by querying it on the canonical resource id. A log line is
/// neither immutable nor queryable that way, so this does not satisfy
/// `cpt-cf-settings-service-dod-category-management-audit`, which stays open.
///
/// It exists because no audit client exists anywhere in the workspace — the
/// subsystem is external (`Container_Ext` in the C4 model) and nothing binds to
/// it. Without an emitter the mutation endpoints could not be exercised at all,
/// and an endpoint nobody can call is not one anybody has verified.
///
/// # Confidentiality is weaker here than in the real destination
///
/// Secret-classified values are safe by construction: [`AuditValue::Masked`]
/// carries no payload, so there is nothing to print. **`pii`-classified values
/// are not** — DESIGN masks only the `secret` class, so a PII value travels as
/// [`AuditValue::Clear`] and would be written in full.
///
/// That is acceptable for categories, whose fields are a key, a name, a
/// description, a sort weight and an icon. It stops being acceptable when
/// `setting_values` arrives in entry 2.5: **this emitter must not carry stored
/// setting values**, because logs are shipped, aggregated and retained under a
/// weaker policy than the audit trail.
pub struct TracingAuditEmitter;

/// Render a value for the log, keeping a masked one masked.
fn render(value: Option<&AuditValue>) -> String {
    match value {
        None => "<absent>".to_owned(),
        Some(AuditValue::Masked) => "<masked>".to_owned(),
        Some(AuditValue::Clear(v)) => v.to_string(),
    }
}

#[async_trait]
impl AuditEmitter for TracingAuditEmitter {
    async fn audit(&self, record: AuditRecord) -> Result<(), DomainError> {
        // Structured fields rather than one formatted string, so the entry can
        // be filtered and reshaped by a collector — and so a real emitter later
        // carries the same field names.
        info!(
            audit.resource = %record.resource,
            audit.actor = %record.actor,
            audit.action = %record.action,
            audit.outcome = ?record.outcome,
            audit.request_id = %record.request_id,
            audit.pre = %render(record.pre_image.as_ref()),
            audit.post = %render(record.post_image.as_ref()),
            "settings mutation recorded"
        );
        // Infallible today. The signature stays fallible because the contract is
        // fail-closed: when a real destination is bound, a failed write must
        // fail the mutation, and every call site already propagates it.
        Ok(())
    }
}

#[cfg(test)]
#[path = "audit_emitter_tests.rs"]
mod audit_emitter_tests;
