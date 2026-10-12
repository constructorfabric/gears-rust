//! Scoped audit repo; follows the Products implementation (D-396, D-433).
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-audit-append-only:p1
use super::driver_failure;
use crate::infra::storage::{RepoError, entity::audit_log};
use sea_orm::{EntityTrait, Set};
use time::OffsetDateTime;
use toolkit_db::secure::{AccessScope, DBRunner, SecureInsertExt};
use uuid::Uuid;
/// The fields every audit row carries beside its subject.
#[derive(Clone, Debug)]
pub struct AuditCommon {
    /// Server-minted by the caller, never re-derived here.
    pub audit_id: Uuid,
    /// Owning tenant.
    pub tenant_id: Uuid,
    /// The acting `SecurityContext` subject id.
    pub actor_ref: Uuid,
    /// The audit action token.
    pub action: String,
    /// The kind of thing the entry's subject names.
    pub subject_kind: String,
    /// A free-text reason, where the door supplies one.
    pub reason: Option<String>,
    /// The request's correlation id, minted once at the authoring edge
    /// (D-431) and rendered as text (D-433): every row one request writes
    /// carries the same value. Never `NULL`: a rereserve op's rows carry the
    /// id the op minted (D-431).
    pub correlation_id: Option<String>,
    /// The commit instant, taken as a parameter rather than read from
    /// `OffsetDateTime::now_utc()`.
    pub written_at: OffsetDateTime,
}

/// Write one audit row in the caller's mutation transaction: a door's act on
/// `subject_id`.
///
/// Writes `seal_state = "unsealed"` and leaves `chain_id`, `seq`,
/// `prev_hash` and `row_hash` `NULL` on every call: this gear never seals,
/// chains or verifies (D-433). `error_code`, `attempted_key`, `session_id`
/// and `ceremony_ref` are carried in the DDL and written `NULL`.
///
/// # Errors
/// Returns scoped storage failures, preserving database errors for retry.
pub async fn write_eventless_act_audit(
    runner: &impl DBRunner,
    scope: &AccessScope,
    common: AuditCommon,
    subject_id: Uuid,
    subject_revision: Option<i64>,
) -> Result<(), RepoError> {
    let audit_id = common.audit_id;
    let model = audit_log::ActiveModel {
        audit_id: Set(common.audit_id),
        tenant_id: Set(common.tenant_id),
        actor_ref: Set(common.actor_ref),
        action: Set(common.action),
        subject_kind: Set(common.subject_kind),
        subject_id: Set(Some(subject_id)),
        subject_revision: Set(subject_revision),
        error_code: Set(None),
        attempted_key: Set(None),
        reason: Set(common.reason),
        correlation_id: Set(common.correlation_id),
        written_at: Set(common.written_at),
        session_id: Set(None),
        ceremony_ref: Set(None),
        seal_state: Set("unsealed".to_owned()),
        chain_id: Set(None),
        seq: Set(None),
        prev_hash: Set(None),
        row_hash: Set(None),
    };

    audit_log::Entity::insert(model.clone())
        .secure()
        .scope_with_model(scope, &model)
        .map_err(|e| driver_failure(format!("audit row {audit_id} scope"), e))?
        .exec(runner)
        .await
        .map_err(|e| driver_failure(format!("insert audit row {audit_id}"), e))?;

    Ok(())
}
