//! Atomic platform policy version transitions. This adapter remains unpublished
//! until the full storage plugin contract is implemented.
use super::cursor;
use super::entity::{policy, policy_operation_log, policy_version};
use super::repo::policy_repo as repo;
use crate::infra::outbox::{EnqueueError, NotificationEnqueuer};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use quota_enforcement_sdk::{
    NotificationEvent, PageRequest, PageResult, PolicyDraft, PolicyId, PolicyScope, PolicyUpdate,
    PolicyVersion, PolicyVersionMeta, PolicyVersionState, StorageError, TransitionOutcome,
};
use sea_orm::Set;
use std::sync::Arc;
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, ScopeError};
use toolkit_db::{Db, DbError};
use toolkit_security::SecurityContext;

const LOG_TARGET: &str = "qe.storage";

/// SQL policy adapter, owning the transaction around pointer, version, audit and outbox.
#[derive(Clone)]
pub struct SqlPolicyStore {
    db: Db,
    enqueuer: Arc<dyn NotificationEnqueuer>,
}

#[derive(Debug, thiserror::Error)]
enum TxError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Scope(#[from] ScopeError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Enqueue(#[from] EnqueueError),
}
/// A backend that cannot answer is an outage, and the gear lifts it to 503.
/// Collapsing it into `Internal` would report a transient reachability problem
/// as a 500 the caller must not retry.
///
/// `source` names which database interaction refused, so the log distinguishes
/// a transaction that never opened from a query or an outbox insert that failed
/// inside one.
fn unavailable(
    operation: &'static str,
    source: &'static str,
    error: &dyn std::fmt::Display,
) -> StorageError {
    tracing::warn!(
        target: LOG_TARGET,
        operation,
        source,
        error = %error,
        "policy storage backend call failed"
    );
    StorageError::Unavailable(format!("{operation} failed: {source} unavailable"))
}

/// State the store cannot make sense of. Unlike an outage this will not fix
/// itself, and the detail never leaves the log.
fn corrupt(operation: &'static str, error: &dyn std::fmt::Display) -> StorageError {
    tracing::error!(target: LOG_TARGET, operation, error = %error, "policy storage state is inconsistent");
    StorageError::Internal(format!("{operation} found inconsistent policy state"))
}

fn lift(operation: &'static str, error: TxError) -> StorageError {
    match error {
        TxError::Storage(error) => error,
        // The three ways the database itself can refuse. Each is reachability,
        // so the gear lifts it to 503; the labels keep them apart in the log.
        TxError::Db(error) => unavailable(operation, "transaction", &error),
        TxError::Scope(ScopeError::Db(error)) => unavailable(operation, "query", &error),
        TxError::Enqueue(EnqueueError::Outbox(toolkit_db::outbox::OutboxError::Database(
            error,
        ))) => unavailable(operation, "outbox", &error),
        TxError::Enqueue(EnqueueError::NotBound) => {
            tracing::warn!(
                target: LOG_TARGET,
                operation,
                "notification outbox is not bound; the transition was rolled back"
            );
            StorageError::Unavailable(format!("{operation} failed: outbox is not bound"))
        }
        // Policy rows are platform-plane and every query scopes with
        // `allow_all`, so a scope the ORM refuses is an inconsistency here, not
        // a caller's authorization problem. `ScopeError` is `#[non_exhaustive]`.
        TxError::Scope(error) => corrupt(operation, &error),
        TxError::Json(error) => corrupt(operation, &error),
        TxError::Enqueue(error) => corrupt(operation, &error),
    }
}
/// The events of a creation with the assigned policy id filled in.
///
/// Storage mints the id inside the transaction, so a caller composing a
/// `policy-changed (created)` event must identify the policy actually inserted,
/// even if a stale caller-supplied ID was present. Other event kinds are untouched.
fn with_policy_id(events: &[NotificationEvent], id: &PolicyId) -> Vec<NotificationEvent> {
    events
        .iter()
        .cloned()
        .map(|mut event| {
            if event.kind == quota_enforcement_sdk::NotificationEventKind::PolicyChanged {
                event.policy_id = Some(id.clone());
            }
            event
        })
        .collect()
}

/// The `scope_key` column value of a policy scope. Shared with the
/// consumption store, which selects the policy inside its own transaction.
pub(super) fn scope_key(scope: &PolicyScope) -> String {
    match scope {
        PolicyScope::Global => "global".into(),
        PolicyScope::Metric { metric } => format!("metric={metric}"),
    }
}
/// One stored version row as its `PolicyVersion`. Shared with the consumption
/// store for the same reason as [`scope_key`].
pub(super) fn decode_version(row: &policy_version::Model) -> Result<PolicyVersion, StorageError> {
    decode(row).map_err(|error| match error {
        TxError::Storage(error) => error,
        other => StorageError::Internal(other.to_string()),
    })
}

fn decode(row: &policy_version::Model) -> Result<PolicyVersion, TxError> {
    let mut value: PolicyVersion = serde_json::from_str(&row.payload)?;
    if value.policy_id.as_str() != row.policy_id || i64::from(value.version) != row.version {
        return Err(StorageError::Internal("policy payload identity mismatch".into()).into());
    }
    value.state = match row.state.as_str() {
        "active" => PolicyVersionState::Active,
        "superseded" => PolicyVersionState::Superseded,
        "rolled_back" => PolicyVersionState::RolledBack,
        "deleted" => PolicyVersionState::Deleted,
        _ => return Err(StorageError::Internal("unknown policy state".into()).into()),
    };
    Ok(value)
}
async fn load(
    runner: &impl DBRunner,
    id: &PolicyId,
    version: i64,
) -> Result<PolicyVersion, TxError> {
    let row = repo::version(runner, id.as_str(), version)
        .await?
        .ok_or_else(|| {
            StorageError::Internal("policy pointer references missing version".into())
        })?;
    decode(&row)
}
async fn insert(runner: &impl DBRunner, value: &PolicyVersion) -> Result<(), TxError> {
    repo::insert_version(
        runner,
        policy_version::ActiveModel {
            policy_id: Set(value.policy_id.to_string()),
            version: Set(i64::from(value.version)),
            state: Set("active".into()),
            payload: Set(serde_json::to_string(value)?),
        },
    )
    .await?;
    Ok(())
}
async fn audit(
    runner: &impl DBRunner,
    id: &PolicyId,
    version: u32,
    kind: &str,
    actor: &str,
    comment: Option<String>,
) -> Result<(), TxError> {
    repo::append_audit(
        runner,
        policy_operation_log::ActiveModel {
            id: Set(uuid::Uuid::now_v7().to_string()),
            policy_id: Set(id.to_string()),
            version: Set(i64::from(version)),
            operation: Set(kind.into()),
            actor: Set(actor.into()),
            comment: Set(comment),
            occurred_at: Set(OffsetDateTime::now_utc().to_string()),
        },
    )
    .await?;
    Ok(())
}

/// Keyset cursor of a policy history page: the last returned version number as
/// four big-endian bytes, base64url without padding.
///
/// The quota listing's cursor carries a 16-byte `UUIDv7`, so a cursor minted
/// there fails the width check here instead of quietly restarting the page.
fn encode_cursor(last: u32) -> String {
    URL_SAFE_NO_PAD.encode(last.to_be_bytes())
}

/// The version a cursor names.
///
/// # Errors
/// [`StorageError::InvalidCursor`] when `cursor` is not exactly four base64url bytes.
fn decode_cursor(cursor: &str) -> Result<u32, StorageError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| StorageError::InvalidCursor)?;
    let bytes: [u8; 4] = bytes.try_into().map_err(|_| StorageError::InvalidCursor)?;
    Ok(u32::from_be_bytes(bytes))
}

impl SqlPolicyStore {
    /// Bind the database and same-transaction event enqueuer.
    #[must_use]
    pub fn new(db: Db, enqueuer: Arc<dyn NotificationEnqueuer>) -> Self {
        Self { db, enqueuer }
    }

    /// Create version one at a database-enforced unoccupied scope.
    /// # Errors
    /// Scope occupancy or atomic storage failure; no partial rows or events commit.
    // @cpt-flow:cpt-cf-quota-enforcement-flow-policy-write:p1
    pub async fn create_policy(
        &self,
        ctx: &SecurityContext,
        draft: PolicyDraft,
        events: &[NotificationEvent],
    ) -> Result<PolicyVersion, StorageError> {
        let actor = ctx.subject_id().to_string();
        let enqueuer = self.enqueuer.clone();
        let events = events.to_vec();
        self.db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let id = if matches!(draft.scope, PolicyScope::Global) {
                        PolicyId::global()
                    } else {
                        PolicyId::new(uuid::Uuid::now_v7().to_string())
                    };
                    let header = policy::ActiveModel {
                        id: Set(id.to_string()),
                        scope_key: Set(scope_key(&draft.scope)),
                        active_version: Set(Some(1)),
                        high_water: Set(1),
                    };
                    if let Err(error) = repo::insert_header(tx, header).await {
                        // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-dup-if
                        if error.is_unique_violation() {
                            // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-dup
                            let occupied = StorageError::PolicyScopeOccupied { scope: draft.scope };
                            return Err(occupied.into());
                            // @cpt-end:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-dup
                        }
                        // @cpt-end:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-dup-if
                        return Err(error.into());
                    }
                    let value = PolicyVersion {
                        policy_id: id,
                        version: 1,
                        scope: draft.scope,
                        engine_id: draft.engine_id,
                        engine_config: draft.engine_config,
                        timeout_ms: draft.timeout_ms,
                        description: draft.description,
                        state: PolicyVersionState::Active,
                        created_at: OffsetDateTime::now_utc(),
                        created_by: actor.clone(),
                        comment: draft.comment,
                        schema_snapshot: draft.schema_snapshot,
                    };
                    insert(tx, &value).await?;
                    audit(
                        tx,
                        &value.policy_id,
                        1,
                        "create",
                        &actor,
                        value.comment.clone(),
                    )
                    .await?;
                    enqueuer
                        .enqueue_all(tx, &with_policy_id(&events, &value.policy_id))
                        .await?;
                    Ok(value)
                })
            })
            .await
            .map_err(|error| lift("create policy", error))
    }

    /// Create a version above the high-water mark after locking the header.
    /// # Errors
    /// Unknown/deleted policy, version conflict, exhaustion or atomic storage failure.
    // @cpt-state:cpt-cf-quota-enforcement-state-policy-version:p1
    pub async fn update_policy(
        &self,
        ctx: &SecurityContext,
        id: PolicyId,
        update: PolicyUpdate,
        events: &[NotificationEvent],
    ) -> Result<PolicyVersion, StorageError> {
        let actor = ctx.subject_id().to_string();
        let enqueuer = self.enqueuer.clone();
        let events = events.to_vec();
        self.db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let header = repo::header(tx, id.as_str(), true).await?.ok_or_else(|| {
                        StorageError::PolicyNotFound {
                            policy_id: id.clone(),
                        }
                    })?;
                    let active =
                        header
                            .active_version
                            .ok_or_else(|| StorageError::PolicyDeleted {
                                policy_id: id.clone(),
                            })?;
                    let mut value = load(tx, &id, active).await?;
                    // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-conflict-if
                    if value.version != update.if_match_version {
                        return Err(StorageError::VersionConflict {
                            expected: update.if_match_version,
                            actual: value.version,
                        }
                        .into());
                    }
                    // @cpt-end:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-conflict-if
                    let next = u32::try_from(header.high_water)
                        .ok()
                        .and_then(|number| number.checked_add(1))
                        .ok_or_else(|| StorageError::Internal("policy version exhausted".into()))?;
                    value.version = next;
                    value.created_at = OffsetDateTime::now_utc();
                    value.created_by = actor.clone();
                    value.comment = update.comment;
                    if let Some(engine) = update.engine_id {
                        value.engine_id = engine;
                    }
                    if let Some(config) = update.engine_config {
                        value.engine_config = config;
                    }
                    if let Some(snapshot) = update.schema_snapshot {
                        value.schema_snapshot = snapshot;
                    }
                    if update.timeout_ms.is_some() {
                        value.timeout_ms = update.timeout_ms;
                    }
                    // @cpt-begin:cpt-cf-quota-enforcement-state-policy-version:p1:inst-pvst-supersede
                    repo::set_state(tx, id.as_str(), active, "superseded").await?;
                    // @cpt-end:cpt-cf-quota-enforcement-state-policy-version:p1:inst-pvst-supersede
                    insert(tx, &value).await?;
                    repo::set_pointer(tx, id.as_str(), Some(i64::from(next)), i64::from(next))
                        .await?;
                    audit(tx, &id, next, "update", &actor, value.comment.clone()).await?;
                    enqueuer.enqueue_all(tx, &events).await?;
                    Ok(value)
                })
            })
            .await
            .map_err(|error| lift("update policy", error))
    }

    /// Roll back to a retained non-terminal version.
    ///
    /// Replaying the already-active target reports `NoOp`: no state moves, and
    /// neither an audit row nor an event is written.
    ///
    /// # Errors
    /// Unknown/deleted policy, unknown/terminal target or atomic storage failure.
    // @cpt-flow:cpt-cf-quota-enforcement-flow-policy-rollback:p1
    pub async fn rollback_policy(
        &self,
        ctx: &SecurityContext,
        id: PolicyId,
        target: u32,
        comment: Option<String>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<PolicyVersion>, StorageError> {
        let actor = ctx.subject_id().to_string();
        let enqueuer = self.enqueuer.clone();
        let events = events.to_vec();
        self.db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let header = repo::header(tx, id.as_str(), true).await?.ok_or_else(|| {
                        StorageError::PolicyNotFound {
                            policy_id: id.clone(),
                        }
                    })?;
                    let active =
                        header
                            .active_version
                            .ok_or_else(|| StorageError::PolicyDeleted {
                                policy_id: id.clone(),
                            })?;
                    // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-rollback:p1:inst-prd-unknown-if
                    // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-rollback:p1:inst-prd-unknown
                    let row = repo::version(tx, id.as_str(), i64::from(target))
                        .await?
                        .ok_or_else(|| StorageError::UnknownPolicyVersion {
                            policy_id: id.clone(),
                            version: target,
                        })?;
                    // @cpt-end:cpt-cf-quota-enforcement-flow-policy-rollback:p1:inst-prd-unknown
                    // @cpt-end:cpt-cf-quota-enforcement-flow-policy-rollback:p1:inst-prd-unknown-if
                    let mut value = decode(&row)?;
                    if active == i64::from(target) {
                        return Ok(TransitionOutcome::NoOp(value));
                    }
                    // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-rollback:p1:inst-prd-rb-if
                    // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-rollback:p1:inst-prd-rb
                    if value.state == PolicyVersionState::RolledBack {
                        return Err(StorageError::VersionRolledBack {
                            policy_id: id,
                            version: target,
                        }
                        .into());
                    }
                    // @cpt-end:cpt-cf-quota-enforcement-flow-policy-rollback:p1:inst-prd-rb
                    // @cpt-end:cpt-cf-quota-enforcement-flow-policy-rollback:p1:inst-prd-rb-if
                    if value.state != PolicyVersionState::Superseded {
                        return Err(StorageError::PolicyDeleted { policy_id: id }.into());
                    }
                    // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-rollback:p1:inst-prd-rollback-apply
                    // @cpt-begin:cpt-cf-quota-enforcement-state-policy-version:p1:inst-pvst-rollback
                    repo::set_state(tx, id.as_str(), active, "rolled_back").await?;
                    // @cpt-end:cpt-cf-quota-enforcement-state-policy-version:p1:inst-pvst-rollback
                    // @cpt-begin:cpt-cf-quota-enforcement-state-policy-version:p1:inst-pvst-reactivate
                    repo::set_state(tx, id.as_str(), i64::from(target), "active").await?;
                    // @cpt-end:cpt-cf-quota-enforcement-state-policy-version:p1:inst-pvst-reactivate
                    repo::set_pointer(tx, id.as_str(), Some(i64::from(target)), header.high_water)
                        .await?;
                    audit(tx, &id, target, "rollback", &actor, comment).await?;
                    enqueuer.enqueue_all(tx, &events).await?;
                    value.state = PolicyVersionState::Active;
                    // @cpt-end:cpt-cf-quota-enforcement-flow-policy-rollback:p1:inst-prd-rollback-apply
                    Ok(TransitionOutcome::Applied(value))
                })
            })
            .await
            .map_err(|error| lift("roll back policy", error))
    }

    /// Soft-delete a metric policy.
    ///
    /// A retry against an already-deleted policy reports `NoOp` and writes
    /// neither a second audit row nor a second event.
    ///
    /// # Errors
    /// Unknown policy, protected global policy or atomic storage failure.
    // @cpt-flow:cpt-cf-quota-enforcement-flow-policy-delete:p1
    pub async fn delete_policy(
        &self,
        ctx: &SecurityContext,
        id: PolicyId,
        comment: Option<String>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<()>, StorageError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-delete:p1:inst-prd-global-if
        if id.is_global() {
            // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-delete:p1:inst-prd-global
            return Err(StorageError::CannotDeleteSeededGlobalPolicy);
            // @cpt-end:cpt-cf-quota-enforcement-flow-policy-delete:p1:inst-prd-global
        }
        // @cpt-end:cpt-cf-quota-enforcement-flow-policy-delete:p1:inst-prd-global-if
        let actor = ctx.subject_id().to_string();
        let enqueuer = self.enqueuer.clone();
        let events = events.to_vec();
        self.db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let header = repo::header(tx, id.as_str(), true).await?.ok_or_else(|| {
                        StorageError::PolicyNotFound {
                            policy_id: id.clone(),
                        }
                    })?;
                    // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-delete:p1:inst-prd-replay-if
                    let Some(active) = header.active_version else {
                        // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-delete:p1:inst-prd-replay
                        return Ok(TransitionOutcome::NoOp(()));
                        // @cpt-end:cpt-cf-quota-enforcement-flow-policy-delete:p1:inst-prd-replay
                    };
                    // @cpt-end:cpt-cf-quota-enforcement-flow-policy-delete:p1:inst-prd-replay-if
                    let value = load(tx, &id, active).await?;
                    // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-delete:p1:inst-prd-delete-apply
                    // @cpt-begin:cpt-cf-quota-enforcement-state-policy-version:p1:inst-pvst-delete
                    repo::set_state(tx, id.as_str(), active, "deleted").await?;
                    // @cpt-end:cpt-cf-quota-enforcement-state-policy-version:p1:inst-pvst-delete
                    repo::set_pointer(tx, id.as_str(), None, header.high_water).await?;
                    audit(tx, &id, value.version, "delete", &actor, comment).await?;
                    enqueuer.enqueue_all(tx, &events).await?;
                    // @cpt-end:cpt-cf-quota-enforcement-flow-policy-delete:p1:inst-prd-delete-apply
                    Ok(TransitionOutcome::Applied(()))
                })
            })
            .await
            .map_err(|error| lift("delete policy", error))
    }

    /// Read one retained version.
    /// # Errors
    /// Database or corrupt payload failure.
    pub async fn read_policy_version(
        &self,
        id: &PolicyId,
        version: u32,
    ) -> Result<Option<PolicyVersion>, StorageError> {
        const OPERATION: &str = "read policy version";
        let conn = self
            .db
            .conn()
            .map_err(|error| unavailable(OPERATION, "connection", &error))?;
        repo::version(&conn, id.as_str(), i64::from(version))
            .await
            .map_err(|error| lift(OPERATION, error.into()))?
            .as_ref()
            .map(decode)
            .transpose()
            .map_err(|error| lift(OPERATION, error))
    }
    /// The active version at the exact `scope`. No most-specific fallback: the
    /// fallback to `global` is a selection decision and belongs inside the
    /// evaluation transaction, not in an exact-scope lookup that callers also
    /// use to ask whether a scope is occupied.
    ///
    /// # Errors
    /// Backend unavailability, or a corrupt payload. An unoccupied scope is
    /// `Ok(None)`.
    pub async fn read_policy(
        &self,
        scope: &PolicyScope,
    ) -> Result<Option<PolicyVersion>, StorageError> {
        const OPERATION: &str = "read policy at scope";
        let conn = self
            .db
            .conn()
            .map_err(|error| unavailable(OPERATION, "connection", &error))?;
        repo::active_at_scope(&conn, &scope_key(scope))
            .await
            .map_err(|error| lift(OPERATION, error.into()))?
            .as_ref()
            .map(decode)
            .transpose()
            .map_err(|error| lift(OPERATION, error))
    }

    /// The active version of one policy ID. A deleted policy has none.
    ///
    /// # Errors
    /// Backend unavailability, or a corrupt payload. A missing or deleted ID is
    /// `Ok(None)`.
    pub async fn read_active_policy_by_id(
        &self,
        id: &PolicyId,
    ) -> Result<Option<PolicyVersion>, StorageError> {
        const OPERATION: &str = "read active policy";
        let conn = self
            .db
            .conn()
            .map_err(|error| unavailable(OPERATION, "connection", &error))?;
        Self::decode_active(&conn, id.as_str(), OPERATION).await
    }

    /// Every active policy, for the bootstrap catalogue and engine
    /// compatibility scan. Caller-less by contract: this is an internal
    /// platform read, not a public list API, so no `SecurityContext` narrows it.
    ///
    /// # Errors
    /// Backend unavailability, or a corrupt payload.
    pub async fn read_active_policies(&self) -> Result<Vec<PolicyVersion>, StorageError> {
        const OPERATION: &str = "scan active policies";
        let conn = self
            .db
            .conn()
            .map_err(|error| unavailable(OPERATION, "connection", &error))?;
        // One statement for the whole scan, so readiness judges a single
        // consistent set instead of a sequence of per-policy reads that a
        // concurrent transition could straddle.
        repo::all_active_versions(&conn)
            .await
            .map_err(|error| lift(OPERATION, error.into()))?
            .iter()
            .map(decode)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| lift(OPERATION, error))
    }

    /// One bounded page of a policy's retained version history, ascending by
    /// version and including terminal versions.
    ///
    /// # Errors
    /// [`StorageError::PolicyNotFound`] for an ID that was never created,
    /// [`StorageError::InvalidCursor`] for a cursor this listing did not issue,
    /// backend unavailability, or a corrupt payload.
    pub async fn list_policy_versions(
        &self,
        id: &PolicyId,
        page: PageRequest,
    ) -> Result<PageResult<PolicyVersionMeta>, StorageError> {
        const OPERATION: &str = "list policy versions";
        let conn = self
            .db
            .conn()
            .map_err(|error| unavailable(OPERATION, "connection", &error))?;
        if repo::header(&conn, id.as_str(), false)
            .await
            .map_err(|error| lift(OPERATION, error.into()))?
            .is_none()
        {
            return Err(StorageError::PolicyNotFound {
                policy_id: id.clone(),
            });
        }
        let after = page.cursor.as_deref().map(decode_cursor).transpose()?;
        let limit = cursor::effective_limit(page.limit);
        let mut rows = repo::history(
            &conn,
            id.as_str(),
            i64::from(after.unwrap_or(0)),
            u64::from(limit) + 1,
        )
        .await
        .map_err(|error| lift(OPERATION, error.into()))?;
        let has_more = rows.len() > limit as usize;
        rows.truncate(limit as usize);
        let items = rows
            .iter()
            .map(|row| {
                decode(row).map(|value| PolicyVersionMeta {
                    version: value.version,
                    state: value.state,
                    created_at: value.created_at,
                    created_by: value.created_by,
                    comment: value.comment,
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| lift(OPERATION, error))?;
        let next_cursor = if has_more {
            items.last().map(|meta| encode_cursor(meta.version))
        } else {
            None
        };
        Ok(PageResult { items, next_cursor })
    }

    /// The active version of `id` in one statement.
    ///
    /// Reads `state = 'active'` rather than following the header's
    /// `active_version` pointer. The pointer read and the version read were two
    /// statements, and a transition between them could return a superseded or
    /// deleted version as the active one. `idx_qe_policy_one_active` makes "at
    /// most one active version per policy" a database guarantee, and every
    /// transition moves state and pointer in one transaction, so this single
    /// read agrees with the header by construction.
    async fn decode_active(
        runner: &impl DBRunner,
        id: &str,
        operation: &'static str,
    ) -> Result<Option<PolicyVersion>, StorageError> {
        repo::active_version(runner, id)
            .await
            .map_err(|error| lift(operation, error.into()))?
            .as_ref()
            .map(decode)
            .transpose()
            .map_err(|error| lift(operation, error))
    }
}

/// Forward the domain port to the inherent methods above. The `# Errors`
/// contract lives on the port; the adapter only has to honour it.
#[async_trait::async_trait]
impl crate::domain::ports::PolicyStore for SqlPolicyStore {
    async fn create_policy(
        &self,
        ctx: &SecurityContext,
        draft: PolicyDraft,
        events: &[NotificationEvent],
    ) -> Result<PolicyVersion, StorageError> {
        Self::create_policy(self, ctx, draft, events).await
    }

    async fn update_policy(
        &self,
        ctx: &SecurityContext,
        policy_id: PolicyId,
        update: PolicyUpdate,
        events: &[NotificationEvent],
    ) -> Result<PolicyVersion, StorageError> {
        Self::update_policy(self, ctx, policy_id, update, events).await
    }

    async fn rollback_policy(
        &self,
        ctx: &SecurityContext,
        policy_id: PolicyId,
        target_version: u32,
        comment: Option<String>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<PolicyVersion>, StorageError> {
        Self::rollback_policy(self, ctx, policy_id, target_version, comment, events).await
    }

    async fn delete_policy(
        &self,
        ctx: &SecurityContext,
        policy_id: PolicyId,
        comment: Option<String>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<()>, StorageError> {
        Self::delete_policy(self, ctx, policy_id, comment, events).await
    }

    async fn read_policy(
        &self,
        scope: &PolicyScope,
    ) -> Result<Option<PolicyVersion>, StorageError> {
        Self::read_policy(self, scope).await
    }

    async fn read_active_policy_by_id(
        &self,
        policy_id: &PolicyId,
    ) -> Result<Option<PolicyVersion>, StorageError> {
        Self::read_active_policy_by_id(self, policy_id).await
    }

    async fn read_active_policies(&self) -> Result<Vec<PolicyVersion>, StorageError> {
        Self::read_active_policies(self).await
    }

    async fn read_policy_version(
        &self,
        policy_id: &PolicyId,
        version: u32,
    ) -> Result<Option<PolicyVersion>, StorageError> {
        Self::read_policy_version(self, policy_id, version).await
    }

    async fn list_policy_versions(
        &self,
        policy_id: &PolicyId,
        page: PageRequest,
    ) -> Result<PageResult<PolicyVersionMeta>, StorageError> {
        Self::list_policy_versions(self, policy_id, page).await
    }
}

#[cfg(test)]
#[path = "policy_store_tests.rs"]
mod tests;
