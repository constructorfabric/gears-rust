//! `session_types` repository.
//!
//! Session types live at the developer scope, not the per-tenant scope. The
//! current Phase 1 schema reflects this (no `tenant_id` column on
//! `session_types`). When per-tenant session-type registration is added we
//! will extend the entity and these methods together.
//!
//! Phase 4 owns only `insert`, `find_by_id`, and `list`. Update / delete
//! flows belong to the developer-admin surface that Phase 14 will assemble.
//
// @cpt-cf-chat-engine-session-type-repo:p4

use std::sync::Arc;

use async_trait::async_trait;
use modkit_db::secure::{AccessScope, SecureEntityExt, SecureInsertExt};
use sea_orm::{EntityTrait, QueryOrder};
use uuid::Uuid;

use crate::domain::error::ChatEngineError;
use crate::infra::db::entity::session_type::{
    self as session_type_entity, Entity as SessionTypeEntity,
};
use crate::infra::db::repo::ChatEngineDb;

/// Repository surface for the `session_types` table.
#[async_trait]
pub trait SessionTypeRepo: Send + Sync {
    /// Persist a new session type.
    async fn insert(
        &self,
        model: session_type_entity::ActiveModel,
    ) -> Result<session_type_entity::Model, ChatEngineError>;

    /// Lookup by surrogate primary key.
    async fn find_by_id(
        &self,
        session_type_id: Uuid,
    ) -> Result<Option<session_type_entity::Model>, ChatEngineError>;

    /// List all session types ordered by `created_at DESC`. Phase 4 does not
    /// paginate this surface — session types are operator-managed and small.
    async fn list(&self) -> Result<Vec<session_type_entity::Model>, ChatEngineError>;
}

/// Sea-ORM-backed implementation of [`SessionTypeRepo`].
///
/// Holds the modkit-db `DBProvider` so every method runs against the same
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
    async fn insert(
        &self,
        model: session_type_entity::ActiveModel,
    ) -> Result<session_type_entity::Model, ChatEngineError> {
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let inserted = SessionTypeEntity::insert(model)
            .secure()
            .scope_unchecked(&scope)?
            .exec_with_returning(&conn)
            .await?;
        Ok(inserted)
    }

    async fn find_by_id(
        &self,
        session_type_id: Uuid,
    ) -> Result<Option<session_type_entity::Model>, ChatEngineError> {
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let row = SessionTypeEntity::find_by_id(session_type_id)
            .secure()
            .scope_with(&scope)
            .one(&conn)
            .await?;
        Ok(row)
    }

    async fn list(&self) -> Result<Vec<session_type_entity::Model>, ChatEngineError> {
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let rows = SessionTypeEntity::find()
            .order_by_desc(session_type_entity::Column::CreatedAt)
            .secure()
            .scope_with(&scope)
            .all(&conn)
            .await?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockRepo;

    #[async_trait]
    impl SessionTypeRepo for MockRepo {
        async fn insert(
            &self,
            _model: session_type_entity::ActiveModel,
        ) -> Result<session_type_entity::Model, ChatEngineError> {
            Err(ChatEngineError::internal("mock: insert not implemented"))
        }

        async fn find_by_id(
            &self,
            _session_type_id: Uuid,
        ) -> Result<Option<session_type_entity::Model>, ChatEngineError> {
            Ok(None)
        }

        async fn list(&self) -> Result<Vec<session_type_entity::Model>, ChatEngineError> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn trait_is_object_safe() {
        let _erased: Arc<dyn SessionTypeRepo> = Arc::new(MockRepo);
    }
}
