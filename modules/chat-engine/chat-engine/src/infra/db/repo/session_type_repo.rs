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

use async_trait::async_trait;
use sea_orm::{ActiveModelTrait, DatabaseConnection, EntityTrait, QueryOrder};
use uuid::Uuid;

use crate::domain::error::ChatEngineError;
use crate::infra::db::entity::session_type::{
    self as session_type_entity, Entity as SessionTypeEntity,
};

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
pub struct SeaSessionTypeRepo {
    db: DatabaseConnection,
}

impl SeaSessionTypeRepo {
    #[must_use]
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait]
impl SessionTypeRepo for SeaSessionTypeRepo {
    async fn insert(
        &self,
        model: session_type_entity::ActiveModel,
    ) -> Result<session_type_entity::Model, ChatEngineError> {
        let inserted = model.insert(&self.db).await?;
        Ok(inserted)
    }

    async fn find_by_id(
        &self,
        session_type_id: Uuid,
    ) -> Result<Option<session_type_entity::Model>, ChatEngineError> {
        let row = SessionTypeEntity::find_by_id(session_type_id)
            .one(&self.db)
            .await?;
        Ok(row)
    }

    async fn list(&self) -> Result<Vec<session_type_entity::Model>, ChatEngineError> {
        let rows = SessionTypeEntity::find()
            .order_by_desc(session_type_entity::Column::CreatedAt)
            .all(&self.db)
            .await?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

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
