// Created: 2026-08-13 by Constructor Tech
//! A tracing-backed interim for the gear-local audit store.

use async_trait::async_trait;
use tracing::info;

use crate::audit::{AuditEmitter, AuditRecord, AuditValue};
use crate::domain::error::DomainError;

/// Records audit entries to structured tracing.
///
/// # This is not the audit store
///
/// DESIGN.md §4.2 *Audit Emitter* requires, in R1, a **gear-local sink** that
/// writes the record inside the mutation's own transaction, fail-closed, into the
/// `audit_records` table this gear owns; §4.3 serves per-`(setting, scope)`
/// history from that table on the canonical resource id. The platform Audit
/// Subsystem is R2, reached through the platform outbox. A log line is neither
/// transactional nor queryable, so this does not satisfy
/// `cpt-cf-settings-service-dod-category-management-audit`, which stays open
/// until the table and its emitter land.
///
/// It exists so the mutation endpoints can be exercised before then — an
/// endpoint nobody can call is not one anybody has verified.
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
