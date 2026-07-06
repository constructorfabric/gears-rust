//! `session_types` repository.
//!
//! Session types live at the developer scope, not the per-tenant scope. The
//! current Phase 1 schema reflects this (no `tenant_id` column on
//! `session_types`). When per-tenant session-type registration is added we
//! will extend the entity and these methods together.
//!
//! Phase 4 owns only `insert`, `find_by_id`, and `list`. Update / delete
//! flows belong to the developer-admin surface that Phase 14 will assemble.
//!
//! The [`SessionTypeRepo`] port is defined in `domain::ports` and returns the
//! domain [`SessionType`] — the entity → domain conversion lives here in the
//! adapter (DE0301).
//
// @cpt-cf-chat-engine-session-type-repo:p4

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{ActiveValue::Set, EntityTrait, QueryOrder};
use toolkit_db::secure::{AccessScope, SecureEntityExt, SecureInsertExt};
use uuid::Uuid;

use chat_engine_sdk::models::SessionType;

use crate::domain::error::ChatEngineError;
use crate::domain::ports::{NewSessionType, SessionTypeRepo};
use crate::infra::db::entity::session_type::{
    self as session_type_entity, Entity as SessionTypeEntity,
};
use crate::infra::db::repo::ChatEngineDb;

/// Project a persisted `session_types` row into the domain [`SessionType`].
fn to_domain(model: session_type_entity::Model) -> SessionType {
    SessionType {
        session_type_id: model.session_type_id,
        name: model.name,
        plugin_instance_id: model.plugin_instance_id,
        created_at: model.created_at,
        updated_at: model.updated_at,
    }
}

/// Sea-ORM-backed implementation of [`SessionTypeRepo`].
///
/// Holds the toolkit-db `DBProvider` so every method runs against the same
/// connection the migration runner used. `session_types` has no tenant
/// column (entity is marked `#[secure(unrestricted)]`), so the secure
/// wrappers run with `AccessScope::allow_all()` — they exist here purely to
/// give us a `&impl DBRunner` execution path; a follow-up that introduces
/// per-tenant session types will replace the noop scope with the real one.
pub struct SeaSessionTypeRepo {
    db: Arc<ChatEngineDb>,
}

impl SeaSessionTypeRepo {
    #[must_use]
    pub fn new(db: Arc<ChatEngineDb>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl SessionTypeRepo for SeaSessionTypeRepo {
    async fn insert(&self, new: NewSessionType) -> Result<SessionType, ChatEngineError> {
        let model = session_type_entity::ActiveModel {
            session_type_id: Set(new.session_type_id),
            name: Set(new.name),
            plugin_instance_id: Set(new.plugin_instance_id),
            created_at: Set(new.created_at),
            updated_at: Set(new.updated_at),
        };
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let inserted = SessionTypeEntity::insert(model)
            .secure()
            .scope_unchecked(&scope)?
            .exec_with_returning(&conn)
            .await?;
        Ok(to_domain(inserted))
    }

    async fn find_by_id(
        &self,
        session_type_id: Uuid,
    ) -> Result<Option<SessionType>, ChatEngineError> {
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let row = SessionTypeEntity::find_by_id(session_type_id)
            .secure()
            .scope_with(&scope)
            .one(&conn)
            .await?;
        Ok(row.map(to_domain))
    }

    async fn list(&self) -> Result<Vec<SessionType>, ChatEngineError> {
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let rows = SessionTypeEntity::find()
            .order_by_desc(session_type_entity::Column::CreatedAt)
            .secure()
            .scope_with(&scope)
            .all(&conn)
            .await?;
        Ok(rows.into_iter().map(to_domain).collect())
    }
}

#[cfg(test)]
#[path = "session_type_repo_tests.rs"]
mod session_type_repo_tests;
