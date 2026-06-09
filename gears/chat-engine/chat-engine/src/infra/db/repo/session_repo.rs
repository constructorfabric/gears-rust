//! `sessions` repository.
//!
//! Owns CRUD for the `sessions` table:
//! - tenant + user scoped reads (`find_by_id`, `list_paginated`)
//! - lifecycle-state writes (`update_lifecycle_state`, `soft_delete`,
//!   `hard_delete`)
//! - metadata/capabilities writes
//!
//! All queries filter by `tenant_id` AND `user_id` from the caller's
//! [`crate::api::auth::Identity`] (sourced from the JWT — never from the
//! request body). Cross-tenant misses surface as `Ok(None)` so handlers can
//! map to HTTP 404 (anti-enumeration rule, per ADR-0021).
//!
//! Hard delete uses a SERIALIZABLE transaction that cascades messages and
//! reactions before removing the session row. The cascade is bounded to the
//! caller's `(tenant_id, user_id)` scope so a malicious request cannot
//! delete rows it doesn't own.
//
// @cpt-cf-chat-engine-session-repo:p4
// @cpt-cf-chat-engine-adr-session-deletion-strategy:p4
// @cpt-cf-chat-engine-adr-session-metadata:p4

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use toolkit_db::odata::{LimitCfg, paginate_odata};
use toolkit_db::secure::{
    AccessScope, DBRunner, SecureDeleteExt, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
    TxConfig,
};
use toolkit_odata::{ODataQuery, Page, SortDir};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait};
use serde_json::Value as JsonValue;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::ChatEngineError;
use crate::domain::session::LifecycleState;
use crate::infra::db::entity::message::{self as message_entity, Entity as MessageEntity};
use crate::infra::db::entity::message_reaction::{
    self as reaction_entity, Entity as ReactionEntity,
};
use crate::infra::db::entity::session::{self as session_entity, Entity as SessionEntity};
use crate::infra::db::odata_mapper::{SessionODataMapper, SessionQueryFilterField};
use crate::infra::db::repo::ChatEngineDb;

/// Default soft-delete grace period applied when the session-type doesn't
/// declare an explicit [`crate::domain::retention::RetentionPolicy`]. Mirrors
/// the PRD default (30 days).
pub const DEFAULT_SOFT_DELETE_RETENTION_DAYS: i64 = 30;

/// Maximum results returned per page.
pub const MAX_PAGE_SIZE: u32 = 100;

/// Default page size used when the caller does not supply `?limit=`.
pub const DEFAULT_PAGE_SIZE: u32 = 20;

/// Typed answer returned by [`SessionRepo::check_session_scope`].
///
/// Hand-rolled string scoping in service code is a footgun: any caller
/// that reaches a row via [`SessionRepo::find_by_session_id_unscoped`]
/// must remember to compare `tenant_id` / `user_id` against the caller's
/// identity, AND must avoid leaking other fields. This enum closes that
/// gap — the only way to learn ownership is to ask the repo, and the
/// repo only hands back a row when scoping matches. Callers that need
/// to distinguish 403 vs 404 read the discriminant directly.
#[derive(Debug, Clone)]
pub enum SessionScopeCheck {
    /// Session exists AND is owned by `(tenant_id, user_id)`. The row is
    /// exposed because the caller proved ownership.
    Owned(session_entity::Model),
    /// Session exists but lives in a different tenant. Maps to HTTP 403.
    WrongTenant,
    /// Session exists in this tenant but belongs to a different user.
    /// Maps to HTTP 404 (anti-enumeration, ADR-0021) — the caller MUST
    /// NOT distinguish this from `NotFound` on the wire.
    WrongUser,
    /// Session id does not resolve to any row, or the row is in the
    /// `HardDeleted` lifecycle state.
    NotFound,
}

/// Outcome of a `delete_session` call — carries the lifecycle change applied
/// so the handler can choose 200 vs 204 accordingly.
#[derive(Debug, Clone)]
pub enum DeleteOutcome {
    /// Soft delete: lifecycle flipped to `soft_deleted`, `scheduled_hard_delete_at`
    /// set to `now() + retention_days`. The session row is still readable.
    Soft {
        /// Final session state after the soft-delete write.
        session: session_entity::Model,
    },
    /// Hard delete: session + messages + reactions removed physically.
    Hard,
}

/// Repository surface for the `sessions` table.
///
/// Trait is object-safe so services can hold `Arc<dyn SessionRepo>` and unit
/// tests can swap a mock without touching a real database.
#[async_trait]
pub trait SessionRepo: Send + Sync {
    /// Insert a new session row. Returns the persisted model so the service
    /// layer can read back DB-applied defaults.
    async fn insert(
        &self,
        model: session_entity::ActiveModel,
    ) -> Result<session_entity::Model, ChatEngineError>;

    /// Find a session by id within the caller's `(tenant_id, user_id)` scope.
    /// Returns `Ok(None)` for missing rows, cross-tenant misses, and rows in
    /// the `hard_deleted` lifecycle state — the handler maps all three to
    /// HTTP 404.
    async fn find_by_id(
        &self,
        tenant_id: &str,
        user_id: &str,
        session_id: Uuid,
    ) -> Result<Option<session_entity::Model>, ChatEngineError>;

    /// `OData`-paginated list scoped to `(tenant_id, user_id)`. Excludes
    /// `hard_deleted` rows. Honours `$filter` / `$orderby` / `$top` from
    /// `query`; when `$orderby` is omitted the default order is
    /// `(created_at DESC, session_id DESC)`. Tenant / user scoping is pinned
    /// from the caller's identity and is never read from `$filter`.
    async fn list_paginated(
        &self,
        tenant_id: &str,
        user_id: &str,
        query: &ODataQuery,
    ) -> Result<Page<session_entity::Model>, ChatEngineError>;

    /// Replace the session's `metadata` JSONB and bump `updated_at`. Caller
    /// is responsible for ownership / lifecycle-state preconditions; this
    /// method only touches the two columns.
    async fn update_metadata(
        &self,
        tenant_id: &str,
        user_id: &str,
        session_id: Uuid,
        metadata: Option<JsonValue>,
    ) -> Result<session_entity::Model, ChatEngineError>;

    /// Replace `enabled_capabilities` JSONB and bump `updated_at`.
    async fn update_capabilities(
        &self,
        tenant_id: &str,
        user_id: &str,
        session_id: Uuid,
        capabilities: Option<JsonValue>,
    ) -> Result<session_entity::Model, ChatEngineError>;

    /// Move the session to a new lifecycle state. Callers MUST have already
    /// validated the transition via `LifecycleState::can_transition_to` (see
    /// `domain::session::ensure_can_transition`).
    async fn update_lifecycle_state(
        &self,
        tenant_id: &str,
        user_id: &str,
        session_id: Uuid,
        new_state: LifecycleState,
    ) -> Result<session_entity::Model, ChatEngineError>;

    /// Apply the soft-delete transition: set `lifecycle_state = soft_deleted`,
    /// `deleted_at = now`, `scheduled_hard_delete_at = now + retention`.
    async fn soft_delete(
        &self,
        tenant_id: &str,
        user_id: &str,
        session_id: Uuid,
        retention_days: i64,
    ) -> Result<session_entity::Model, ChatEngineError>;

    /// Cascade-delete messages + reactions inside a SERIALIZABLE transaction,
    /// then remove the session row. Returns `Ok(true)` when the session was
    /// found and removed; `Ok(false)` when no matching row existed.
    async fn hard_delete(
        &self,
        tenant_id: &str,
        user_id: &str,
        session_id: Uuid,
    ) -> Result<bool, ChatEngineError>;

    /// Phase 8 hook (retention cleanup). List `active` sessions in the
    /// tenant ordered by `session_id`, returning at most `limit` rows whose
    /// `session_id` is strictly greater than `after` (pass `None` to start at
    /// the beginning). The retention scheduler pages through a large tenant
    /// in bounded batches across ticks via this cursor instead of loading
    /// the entire active set into memory; archived / soft-deleted rows are
    /// owned by the deletion-grace flow (ADR-0021).
    ///
    /// Default impl returns an empty list so test mocks that don't care
    /// about retention keep compiling.
    async fn list_active_sessions_for_tenant(
        &self,
        tenant_id: &str,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<session_entity::Model>, ChatEngineError> {
        let _ = (tenant_id, after, limit);
        Ok(Vec::new())
    }

    /// Phase 8 hook (retention cleanup). Return the distinct `tenant_id`
    /// values that currently own at least one `active` session. The
    /// retention scheduler uses this to enumerate real tenants before
    /// calling [`Self::list_active_sessions_for_tenant`] for each, so the
    /// scheduler does not need an external tenant directory.
    ///
    /// Default impl returns an empty list so test mocks that don't care
    /// about retention keep compiling.
    async fn list_tenants_with_active_sessions(
        &self,
    ) -> Result<Vec<String>, ChatEngineError> {
        Ok(Vec::new())
    }

    /// Phase 8 hook (overflow recovery). Look up a session by primary key
    /// only — NOT scoped by tenant/user.
    ///
    /// **This method is an internal building block for
    /// [`Self::check_session_scope`]. Service code MUST NOT call it
    /// directly — every existing production call site has been migrated
    /// to `check_session_scope` (which returns a typed scope
    /// discriminant and never exposes a foreign row) or to the scoped
    /// [`Self::find_by_id`].** The method is retained on the trait so
    /// `check_session_scope`'s default impl can reuse a single SQL
    /// lookup across the four discriminant branches.
    ///
    /// The default impl returns `None` so existing test mocks compile.
    async fn find_by_session_id_unscoped(
        &self,
        session_id: Uuid,
    ) -> Result<Option<session_entity::Model>, ChatEngineError> {
        let _ = session_id;
        Ok(None)
    }

    /// Resolve a session id under the caller's `(tenant_id, user_id)`
    /// scope and report ownership as a typed discriminant. This is the
    /// **only** API service code should use when it needs to distinguish
    /// cross-tenant from cross-user access — the unscoped lookup is no
    /// longer reachable through a public method that returns the row.
    ///
    /// The default impl issues an unscoped lookup and discriminates
    /// in-process; production [`SeaSessionRepo`] uses the same path.
    async fn check_session_scope(
        &self,
        tenant_id: &str,
        user_id: &str,
        session_id: Uuid,
    ) -> Result<SessionScopeCheck, ChatEngineError> {
        let Some(row) = self.find_by_session_id_unscoped(session_id).await? else {
            return Ok(SessionScopeCheck::NotFound);
        };
        if row.tenant_id != tenant_id {
            return Ok(SessionScopeCheck::WrongTenant);
        }
        if row.user_id != user_id {
            return Ok(SessionScopeCheck::WrongUser);
        }
        Ok(SessionScopeCheck::Owned(row))
    }

    /// Phase 10 hook (session sharing). Look up a session by its
    /// `share_token` column — NOT scoped by tenant/user because the share
    /// path is unauthenticated. Excludes `hard_deleted` rows (default impl
    /// always returns `None` so existing test mocks compile).
    async fn find_by_share_token(
        &self,
        share_token: &str,
    ) -> Result<Option<session_entity::Model>, ChatEngineError> {
        let _ = share_token;
        Ok(None)
    }

    /// Phase 10 hook (session sharing). Atomically replace the
    /// `share_token` column AND the persisted `metadata` JSONB. Used to
    /// (1) issue a new token while writing `share_expires_at` into
    /// metadata, and (2) revoke a token while removing the same metadata
    /// key. Bumps `updated_at`.
    ///
    /// Default impl returns `Internal` so existing test mocks compile;
    /// the SeaORM impl overrides this.
    async fn update_share_token(
        &self,
        tenant_id: &str,
        user_id: &str,
        session_id: Uuid,
        share_token: Option<String>,
        metadata: Option<JsonValue>,
    ) -> Result<session_entity::Model, ChatEngineError> {
        let _ = (tenant_id, user_id, session_id, share_token, metadata);
        Err(ChatEngineError::internal(
            "update_share_token not implemented for this repository",
        ))
    }
}

/// Sea-ORM-backed implementation of [`SessionRepo`].
///
/// Holds the toolkit-db `DBProvider` so every method runs against the same
/// connection the migration runner used. `sessions` carries `String`
/// tenant/user identifiers rather than `Uuid`, so the entity is marked
/// `#[secure(unrestricted)]` and scoping stays explicit in the per-method
/// `.filter(Column::TenantId.eq(...))` clauses. The `.secure()` /
/// `.scope_with(...)` wrappers exist purely to give us a `&impl DBRunner`
/// execution path; a follow-up that lifts the columns to `Uuid` can
/// replace the manual filters with proper `AccessScope` constraints.
pub struct SeaSessionRepo {
    db: Arc<ChatEngineDb>,
}

impl SeaSessionRepo {
    #[must_use]
    pub fn new(db: Arc<ChatEngineDb>) -> Self {
        Self { db }
    }

    /// Build the `(session_id, tenant_id, user_id)` AND filter every scoped
    /// write reuses. Centralising it avoids drift between read and update
    /// paths.
    fn owned_filter(
        session_id: Uuid,
        tenant_id: &str,
        user_id: &str,
    ) -> Condition {
        Condition::all()
            .add(session_entity::Column::SessionId.eq(session_id))
            .add(session_entity::Column::TenantId.eq(tenant_id.to_owned()))
            .add(session_entity::Column::UserId.eq(user_id.to_owned()))
    }

    /// Re-read a session by `(tenant_id, user_id, session_id)` after a
    /// scoped write. Returns `NotFound` when the row vanished — useful as a
    /// safety net for the write paths.
    async fn read_owned<R: DBRunner>(
        runner: &R,
        tenant_id: &str,
        user_id: &str,
        session_id: Uuid,
    ) -> Result<session_entity::Model, ChatEngineError> {
        let scope = AccessScope::allow_all();
        let row = SessionEntity::find()
            .secure()
            .scope_with(&scope)
            .filter(Self::owned_filter(session_id, tenant_id, user_id))
            .one(runner)
            .await?;
        let row = row.ok_or_else(|| ChatEngineError::not_found("session", session_id))?;
        if row.lifecycle_state == LifecycleState::HardDeleted.as_str() {
            return Err(ChatEngineError::not_found("session", session_id));
        }
        Ok(row)
    }
}

#[async_trait]
impl SessionRepo for SeaSessionRepo {
    async fn insert(
        &self,
        model: session_entity::ActiveModel,
    ) -> Result<session_entity::Model, ChatEngineError> {
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let inserted = SessionEntity::insert(model)
            .secure()
            .scope_unchecked(&scope)?
            .exec_with_returning(&conn)
            .await?;
        Ok(inserted)
    }

    async fn find_by_id(
        &self,
        tenant_id: &str,
        user_id: &str,
        session_id: Uuid,
    ) -> Result<Option<session_entity::Model>, ChatEngineError> {
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let row = SessionEntity::find()
            .secure()
            .scope_with(&scope)
            .filter(Self::owned_filter(session_id, tenant_id, user_id))
            .one(&conn)
            .await?;

        Ok(row.filter(|m| m.lifecycle_state != LifecycleState::HardDeleted.as_str()))
    }

    async fn list_paginated(
        &self,
        tenant_id: &str,
        user_id: &str,
        query: &ODataQuery,
    ) -> Result<Page<session_entity::Model>, ChatEngineError> {
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();

        // Tenant / user scoping and the hard-delete exclusion come from the
        // caller's identity, never from the OData `$filter`, so they live in
        // the base query rather than the caller-controlled clause.
        let base_query = SessionEntity::find()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(session_entity::Column::TenantId.eq(tenant_id.to_owned()))
                    .add(session_entity::Column::UserId.eq(user_id.to_owned()))
                    .add(
                        session_entity::Column::LifecycleState
                            .ne(LifecycleState::HardDeleted.as_str().to_string()),
                    ),
            );

        // Default-recent posture: when the caller supplies neither a cursor
        // nor `$orderby`, sort by `created_at DESC`. `session_id` is appended
        // as the unique tiebreaker by `paginate_odata`, yielding the legacy
        // `(created_at DESC, session_id DESC)` total order.
        let query = if query.cursor.is_none() && query.order.is_empty() {
            let mut adjusted = query.clone();
            adjusted.order = adjusted.order.ensure_tiebreaker("created_at", SortDir::Desc);
            Cow::Owned(adjusted)
        } else {
            Cow::Borrowed(query)
        };

        let limit_cfg = LimitCfg {
            default: u64::from(DEFAULT_PAGE_SIZE),
            max: u64::from(MAX_PAGE_SIZE),
        };

        paginate_odata::<SessionQueryFilterField, SessionODataMapper, _, _, _, _>(
            base_query,
            &conn,
            query.as_ref(),
            ("session_id", SortDir::Desc),
            limit_cfg,
            std::convert::identity,
        )
        .await
        .map_err(map_odata_err)
    }

    async fn update_metadata(
        &self,
        tenant_id: &str,
        user_id: &str,
        session_id: Uuid,
        metadata: Option<JsonValue>,
    ) -> Result<session_entity::Model, ChatEngineError> {
        // Read-then-update inside one transaction so the read enforces
        // ownership before the write touches the row, and so concurrent
        // writers can't slip a different tenant in between the two.
        let tenant_id = tenant_id.to_owned();
        let user_id = user_id.to_owned();
        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    let _existing = Self::read_owned(tx, &tenant_id, &user_id, session_id).await?;
                    let now = OffsetDateTime::now_utc();
                    let scope = AccessScope::allow_all();
                    SessionEntity::update_many()
                        .secure()
                        .scope_with(&scope)
                        .filter(Self::owned_filter(session_id, &tenant_id, &user_id))
                        .col_expr(
                            session_entity::Column::Metadata,
                            Expr::value(metadata.clone()),
                        )
                        .col_expr(session_entity::Column::UpdatedAt, Expr::value(now))
                        .exec(tx)
                        .await?;
                    Self::read_owned(tx, &tenant_id, &user_id, session_id).await
                })
            })
            .await
    }

    async fn update_capabilities(
        &self,
        tenant_id: &str,
        user_id: &str,
        session_id: Uuid,
        capabilities: Option<JsonValue>,
    ) -> Result<session_entity::Model, ChatEngineError> {
        let tenant_id = tenant_id.to_owned();
        let user_id = user_id.to_owned();
        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    let _existing = Self::read_owned(tx, &tenant_id, &user_id, session_id).await?;
                    let now = OffsetDateTime::now_utc();
                    let scope = AccessScope::allow_all();
                    SessionEntity::update_many()
                        .secure()
                        .scope_with(&scope)
                        .filter(Self::owned_filter(session_id, &tenant_id, &user_id))
                        .col_expr(
                            session_entity::Column::EnabledCapabilities,
                            Expr::value(capabilities.clone()),
                        )
                        .col_expr(session_entity::Column::UpdatedAt, Expr::value(now))
                        .exec(tx)
                        .await?;
                    Self::read_owned(tx, &tenant_id, &user_id, session_id).await
                })
            })
            .await
    }

    async fn update_lifecycle_state(
        &self,
        tenant_id: &str,
        user_id: &str,
        session_id: Uuid,
        new_state: LifecycleState,
    ) -> Result<session_entity::Model, ChatEngineError> {
        let tenant_id = tenant_id.to_owned();
        let user_id = user_id.to_owned();
        let state_str = new_state.as_str().to_string();
        let clear_deletion = matches!(new_state, LifecycleState::Active);
        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    let _existing = Self::read_owned(tx, &tenant_id, &user_id, session_id).await?;
                    let now = OffsetDateTime::now_utc();
                    let scope = AccessScope::allow_all();
                    let mut update = SessionEntity::update_many()
                        .secure()
                        .scope_with(&scope)
                        .filter(Self::owned_filter(session_id, &tenant_id, &user_id))
                        .col_expr(
                            session_entity::Column::LifecycleState,
                            Expr::value(state_str.clone()),
                        )
                        .col_expr(session_entity::Column::UpdatedAt, Expr::value(now));
                    if clear_deletion {
                        // Restoring out of soft_deleted clears the deletion
                        // bookkeeping; archive transitions leave it intact (a
                        // soft-deleted session is the only state that uses
                        // those columns).
                        let null_dt: Option<OffsetDateTime> = None;
                        update = update
                            .col_expr(session_entity::Column::DeletedAt, Expr::value(null_dt))
                            .col_expr(
                                session_entity::Column::ScheduledHardDeleteAt,
                                Expr::value(null_dt),
                            );
                    }
                    update.exec(tx).await?;
                    Self::read_owned(tx, &tenant_id, &user_id, session_id).await
                })
            })
            .await
    }

    async fn soft_delete(
        &self,
        tenant_id: &str,
        user_id: &str,
        session_id: Uuid,
        retention_days: i64,
    ) -> Result<session_entity::Model, ChatEngineError> {
        let tenant_id = tenant_id.to_owned();
        let user_id = user_id.to_owned();
        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    let _existing = Self::read_owned(tx, &tenant_id, &user_id, session_id).await?;
                    let now = OffsetDateTime::now_utc();
                    let grace = retention_days.max(0);
                    let scheduled = now + Duration::from_secs((grace as u64) * 86_400);
                    let scope = AccessScope::allow_all();
                    SessionEntity::update_many()
                        .secure()
                        .scope_with(&scope)
                        .filter(Self::owned_filter(session_id, &tenant_id, &user_id))
                        .col_expr(
                            session_entity::Column::LifecycleState,
                            Expr::value(LifecycleState::SoftDeleted.as_str().to_string()),
                        )
                        .col_expr(
                            session_entity::Column::DeletedAt,
                            Expr::value(Some(now)),
                        )
                        .col_expr(
                            session_entity::Column::ScheduledHardDeleteAt,
                            Expr::value(Some(scheduled)),
                        )
                        .col_expr(session_entity::Column::UpdatedAt, Expr::value(now))
                        .exec(tx)
                        .await?;
                    Self::read_owned(tx, &tenant_id, &user_id, session_id).await
                })
            })
            .await
    }

    async fn hard_delete(
        &self,
        tenant_id: &str,
        user_id: &str,
        session_id: Uuid,
    ) -> Result<bool, ChatEngineError> {
        // SERIALIZABLE isolation: per ADR-0021 the cascade MUST observe a
        // consistent snapshot so concurrent message inserts cannot race the
        // deletion. Conflicts surface as serialization failures and bubble
        // out to the caller as 409 (Phase 14 will map them).
        let tenant_id = tenant_id.to_owned();
        let user_id = user_id.to_owned();
        self.db
            .transaction_with_config(TxConfig::serializable(), move |tx| {
                Box::pin(async move {
                    let scope = AccessScope::allow_all();
                    // Re-load inside the transaction to make sure the
                    // ownership/lifecycle check sees the same snapshot as
                    // the cascade deletes.
                    let owned = SessionEntity::find()
                        .secure()
                        .scope_with(&scope)
                        .filter(SeaSessionRepo::owned_filter(
                            session_id,
                            &tenant_id,
                            &user_id,
                        ))
                        .one(tx)
                        .await?;

                    let Some(_session) = owned else {
                        return Ok(false);
                    };

                    // Cascade reactions first (they FK onto messages).
                    let message_ids: Vec<Uuid> = MessageEntity::find()
                        .secure()
                        .scope_with(&scope)
                        .filter(
                            Condition::all().add(message_entity::Column::SessionId.eq(session_id)),
                        )
                        .all(tx)
                        .await?
                        .into_iter()
                        .map(|m| m.message_id)
                        .collect();
                    if !message_ids.is_empty() {
                        ReactionEntity::delete_many()
                            .secure()
                            .scope_with(&scope)
                            .filter(
                                Condition::all().add(
                                    reaction_entity::Column::MessageId.is_in(message_ids.clone()),
                                ),
                            )
                            .exec(tx)
                            .await?;
                    }

                    MessageEntity::delete_many()
                        .secure()
                        .scope_with(&scope)
                        .filter(
                            Condition::all().add(message_entity::Column::SessionId.eq(session_id)),
                        )
                        .exec(tx)
                        .await?;

                    SessionEntity::delete_many()
                        .secure()
                        .scope_with(&scope)
                        .filter(SeaSessionRepo::owned_filter(
                            session_id,
                            &tenant_id,
                            &user_id,
                        ))
                        .exec(tx)
                        .await?;

                    Ok(true)
                })
            })
            .await
    }

    async fn list_active_sessions_for_tenant(
        &self,
        tenant_id: &str,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<session_entity::Model>, ChatEngineError> {
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let mut filter = Condition::all()
            .add(session_entity::Column::TenantId.eq(tenant_id.to_owned()))
            .add(
                session_entity::Column::LifecycleState
                    .eq(LifecycleState::Active.as_str().to_string()),
            );
        if let Some(after) = after {
            // Keyset cursor on the `session_id` primary key — pages the
            // retention sweep across ticks without OFFSET scans.
            filter = filter.add(session_entity::Column::SessionId.gt(after));
        }
        // Push the per-tick cap into SQL so a large tenant returns only the
        // candidates this tick will process, not its entire active set.
        let rows = SessionEntity::find()
            .secure()
            .scope_with(&scope)
            .filter(filter)
            .order_by(session_entity::Column::SessionId, sea_orm::Order::Asc)
            .limit(u64::from(limit))
            .all(&conn)
            .await?;
        Ok(rows)
    }

    async fn list_tenants_with_active_sessions(
        &self,
    ) -> Result<Vec<String>, ChatEngineError> {
        // The secure layer doesn't yet expose `select_only() +
        // into_tuple()`, so the original `SELECT DISTINCT tenant_id`
        // projection has no `&impl DBRunner` execution path. Fall back to
        // selecting the active rows and deduplicating in-process — the
        // retention scheduler is the only caller, runs on an interval, and
        // is bounded by the active-session count which the cleanup task
        // already polls regularly. Once `SecureSelect::into_tuple` exists
        // this collapses back to a one-shot `DISTINCT` scan.
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let rows = SessionEntity::find()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all().add(
                    session_entity::Column::LifecycleState
                        .eq(LifecycleState::Active.as_str().to_string()),
                ),
            )
            .all(&conn)
            .await?;

        let mut tenants: Vec<String> = rows.into_iter().map(|m| m.tenant_id).collect();
        tenants.sort_unstable();
        tenants.dedup();
        Ok(tenants)
    }

    async fn find_by_session_id_unscoped(
        &self,
        session_id: Uuid,
    ) -> Result<Option<session_entity::Model>, ChatEngineError> {
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let row = SessionEntity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(session_entity::Column::SessionId.eq(session_id)))
            .one(&conn)
            .await?;
        Ok(row.filter(|m| m.lifecycle_state != LifecycleState::HardDeleted.as_str()))
    }

    async fn find_by_share_token(
        &self,
        share_token: &str,
    ) -> Result<Option<session_entity::Model>, ChatEngineError> {
        // UNIQUE index on `sessions.share_token` is exercised by this
        // single equality lookup — keep the query verbatim so EXPLAIN
        // selects the index per ADR-0016.
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let row = SessionEntity::find()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all().add(session_entity::Column::ShareToken.eq(share_token.to_owned())),
            )
            .one(&conn)
            .await?;
        Ok(row.filter(|m| m.lifecycle_state != LifecycleState::HardDeleted.as_str()))
    }

    async fn update_share_token(
        &self,
        tenant_id: &str,
        user_id: &str,
        session_id: Uuid,
        share_token: Option<String>,
        metadata: Option<JsonValue>,
    ) -> Result<session_entity::Model, ChatEngineError> {
        let tenant_id = tenant_id.to_owned();
        let user_id = user_id.to_owned();
        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    let _existing = Self::read_owned(tx, &tenant_id, &user_id, session_id).await?;
                    let now = OffsetDateTime::now_utc();
                    let scope = AccessScope::allow_all();
                    SessionEntity::update_many()
                        .secure()
                        .scope_with(&scope)
                        .filter(Self::owned_filter(session_id, &tenant_id, &user_id))
                        .col_expr(
                            session_entity::Column::ShareToken,
                            Expr::value(share_token.clone()),
                        )
                        .col_expr(
                            session_entity::Column::Metadata,
                            Expr::value(metadata.clone()),
                        )
                        .col_expr(session_entity::Column::UpdatedAt, Expr::value(now))
                        .exec(tx)
                        .await?;
                    Self::read_owned(tx, &tenant_id, &user_id, session_id).await
                })
            })
            .await
    }
}

/// Classify an `OData` pagination failure into a [`ChatEngineError`].
///
/// Filter / orderby / cursor problems are caller mistakes (HTTP 400); a
/// `Db` failure is an internal fault (HTTP 500).
fn map_odata_err(err: toolkit_odata::Error) -> ChatEngineError {
    match err {
        toolkit_odata::Error::Db(msg) => ChatEngineError::internal(msg),
        other => ChatEngineError::bad_request(other.to_string()),
    }
}
