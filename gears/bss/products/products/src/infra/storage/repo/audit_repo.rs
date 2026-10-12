//! @cpt-dod:cpt-cf-bss-products-dod-audit-append-only:p1
//! Audit row construction, on the caller's runner (P-D-193, P-D-200).
use super::driver_failure;
use crate::infra::storage::{RepoError, entity::audit_log};
use bss_products_sdk::models::Lifecycle;
use sea_orm::{EntityTrait, Set};
use time::OffsetDateTime;
use toolkit_db::secure::{AccessScope, DBRunner, SecureInsertExt, SecureInsertManyExt};
use uuid::Uuid;

/// The actor of an act no caller asked for: the orphan-fence expiry every SKU read runs
/// (P-D-189, P-D-213). The nil uuid — the subject of `SecurityContext::anonymous()`, the
/// platform's system context — which no principal carries: `require_authenticated` refuses a nil
/// subject.
pub const SYSTEM_ACTOR: Uuid = Uuid::nil();

/// The SKU lifecycle an audited act found and the one it left (P-D-213): `from_lifecycle` and
/// `to_lifecycle`. Both are read in the act's own transaction, so a row says what the act did,
/// not what a later act made of it; an act that moves nothing stamps the same lifecycle twice.
/// `from` is `None` on a create (the SKU did not exist), `to` on a draft delete (it no longer
/// does), and both on a row whose act concerns no SKU (category, reference and policy acts).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LifecycleMove {
    pub from: Option<Lifecycle>,
    pub to: Option<Lifecycle>,
}
impl LifecycleMove {
    /// A row whose act concerns no SKU.
    pub const NONE: Self = Self {
        from: None,
        to: None,
    };
    /// An act that found the SKU in `from` and left it in `to`.
    #[must_use]
    pub const fn between(from: Lifecycle, to: Lifecycle) -> Self {
        Self {
            from: Some(from),
            to: Some(to),
        }
    }
}
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
    /// The request's correlation, a `text` column (P-D-200). Products writes
    /// `None` on every row: this gear establishes no request correlation.
    pub correlation_id: Option<String>,
    /// The act's instant, taken by the writer and passed in rather than read from
    /// `OffsetDateTime::now_utc()` here; not the commit (P-D-213).
    pub written_at: OffsetDateTime,
    /// The SKU lifecycle move the act made (P-D-213); [`LifecycleMove::NONE`] when it concerns no
    /// SKU.
    pub lifecycle: LifecycleMove,
}

/// Write one audit row in the caller's mutation transaction: a door's act on
/// `subject_id` (P-D-193).
///
/// Writes `seal_state = "unsealed"` and leaves `chain_id`, `seq`,
/// `prev_hash` and `row_hash` `NULL` on every call, unconditionally: this
/// gear never seals, chains or verifies (P-D-200). `error_code`,
/// `attempted_key`, `session_id` and `ceremony_ref` are carried in the DDL
/// and written `NULL`: no door writes a refusal, keyed, elevated-read or
/// ceremony row.
///
/// # Errors
/// Returns scoped storage failures.
pub async fn write_eventless_act_audit(
    runner: &impl DBRunner,
    scope: &AccessScope,
    common: AuditCommon,
    subject_id: Uuid,
    subject_revision: Option<i64>,
) -> Result<(), RepoError> {
    let audit_id = common.audit_id;
    let model = row(common, subject_id, subject_revision);

    audit_log::Entity::insert(model.clone())
        .secure()
        .scope_with_model(scope, &model)
        .map_err(|e| driver_failure(format!("audit row {audit_id} scope"), e))?
        .exec(runner)
        .await
        .map_err(|e| driver_failure(format!("insert audit row {audit_id}"), e))?;

    Ok(())
}

/// The most rows one multi-row audit `INSERT` carries: 21 binds a row keeps a statement under
/// `SQLite`'s 32 766 and Postgres's 65 535 bound parameters.
pub const AUDIT_ROWS_PER_INSERT: usize = 1000;

/// Write several acts' audit rows in the caller's transaction as ONE multi-row `INSERT` per
/// [`AUDIT_ROWS_PER_INSERT`] rows (P-D-211: the orphan-fence expiry's rows, however many fences
/// it lifted), each as [`write_eventless_act_audit`] writes one. Every row must belong to
/// `tenant`: the batch is checked row by row before it is written. Nothing to write is no
/// statement.
///
/// # Errors
/// Returns scoped storage failures, and [`RepoError::Db`] for a row of another tenant.
pub async fn write_eventless_act_audits(
    runner: &impl DBRunner,
    tenant: Uuid,
    rows: Vec<(AuditCommon, Uuid, Option<i64>)>,
) -> Result<(), RepoError> {
    if let Some((stray, ..)) = rows.iter().find(|(c, ..)| c.tenant_id != tenant) {
        return Err(RepoError::Db(format!(
            "audit row {} names tenant {}, not the batch's {tenant}",
            stray.audit_id, stray.tenant_id
        )));
    }
    let mut models: Vec<audit_log::ActiveModel> = rows
        .into_iter()
        .map(|(common, subject_id, subject_revision)| row(common, subject_id, subject_revision))
        .collect();
    let scope = AccessScope::for_tenant(tenant);
    while !models.is_empty() {
        let rest = models.split_off(models.len().min(AUDIT_ROWS_PER_INSERT));
        audit_log::Entity::insert_many(std::mem::replace(&mut models, rest))
            .secure()
            .scope_unchecked(&scope)
            .map_err(|e| driver_failure("audit rows scope".into(), e))?
            .exec(runner)
            .await
            .map_err(|e| driver_failure("insert audit rows".into(), e))?;
    }
    Ok(())
}

/// One audit row as every writer stores it (see [`write_eventless_act_audit`]).
fn row(
    common: AuditCommon,
    subject_id: Uuid,
    subject_revision: Option<i64>,
) -> audit_log::ActiveModel {
    audit_log::ActiveModel {
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
        from_lifecycle: Set(common.lifecycle.from.map(|l| l.as_str().to_owned())),
        to_lifecycle: Set(common.lifecycle.to.map(|l| l.as_str().to_owned())),
    }
}

#[cfg(test)]
#[path = "audit_repo_tests.rs"]
mod audit_repo_tests;
