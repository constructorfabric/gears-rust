//! `plugin_configs` repository.
//!
//! Owns CRUD over the `(plugin_instance_id, session_type_id) -> JSONB` mapping
//! that backs `PluginCallContext.plugin_config`. The contents of `config` are
//! opaque to Chat Engine — this repo forwards bytes verbatim. Upsert refreshes
//! `updated_at`; `find` returns `None` when no row exists.
//!
//! Per Phase 3 contract:
//! - Trait is object-safe (`Arc<dyn PluginConfigRepo>` is wired into
//!   `PluginService`).
//! - Composite PK `(plugin_instance_id, session_type_id)` is the only key
//!   surface; `find`, `upsert`, `delete` all key on the pair.
//! - The Sea-ORM impl uses `ON CONFLICT (plugin_instance_id, session_type_id)
//!   DO UPDATE SET config = EXCLUDED.config, updated_at = EXCLUDED.updated_at`.
//
// @cpt-cf-chat-engine-plugin-config-repo:p3

use async_trait::async_trait;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set,
};
use serde_json::Value as JsonValue;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::ChatEngineError;
use crate::infra::db::entity::plugin_config::{
    ActiveModel, Column, Entity as PluginConfigEntity,
};

/// Repository surface for `plugin_configs`.
///
/// The trait is object-safe — services hold `Arc<dyn PluginConfigRepo>` so the
/// concrete backend (Sea-ORM today, an in-memory mock in tests) is swappable.
#[async_trait]
pub trait PluginConfigRepo: Send + Sync {
    /// Look up the JSONB config for a `(plugin_instance_id, session_type_id)`
    /// pair. Returns `Ok(None)` when the row is absent.
    async fn find(
        &self,
        plugin_instance_id: &str,
        session_type_id: Uuid,
    ) -> Result<Option<JsonValue>, ChatEngineError>;

    /// Insert or update the JSONB config for a pair. The `updated_at` column
    /// MUST be refreshed on both insert and update.
    async fn upsert(
        &self,
        plugin_instance_id: &str,
        session_type_id: Uuid,
        config: JsonValue,
    ) -> Result<(), ChatEngineError>;

    /// Remove the row keyed by `(plugin_instance_id, session_type_id)`. A
    /// missing row is NOT an error — this method is idempotent.
    async fn delete(
        &self,
        plugin_instance_id: &str,
        session_type_id: Uuid,
    ) -> Result<(), ChatEngineError>;
}

/// Sea-ORM-backed implementation of [`PluginConfigRepo`].
///
/// Holds an owned `DatabaseConnection` — clone freely; under the hood SeaORM's
/// connection is reference-counted. Repositories in other phases use the
/// `DBRunner` abstraction for transactional scope; the plugin-config repo is
/// intentionally connection-scoped because its writes are independent of any
/// session/message transaction.
pub struct SeaPluginConfigRepo {
    db: DatabaseConnection,
}

impl SeaPluginConfigRepo {
    #[must_use]
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait]
impl PluginConfigRepo for SeaPluginConfigRepo {
    async fn find(
        &self,
        plugin_instance_id: &str,
        session_type_id: Uuid,
    ) -> Result<Option<JsonValue>, ChatEngineError> {
        let row = PluginConfigEntity::find_by_id((
            plugin_instance_id.to_owned(),
            session_type_id,
        ))
        .one(&self.db)
        .await?;
        Ok(row.and_then(|m| m.config))
    }

    async fn upsert(
        &self,
        plugin_instance_id: &str,
        session_type_id: Uuid,
        config: JsonValue,
    ) -> Result<(), ChatEngineError> {
        let now = OffsetDateTime::now_utc();
        let am = ActiveModel {
            plugin_instance_id: Set(plugin_instance_id.to_owned()),
            session_type_id: Set(session_type_id),
            config: Set(Some(config)),
            created_at: Set(now),
            updated_at: Set(now),
        };

        // Composite-PK upsert: refresh `config` and `updated_at`; leave
        // `created_at` untouched on update.
        let on_conflict = OnConflict::columns([
            Column::PluginInstanceId,
            Column::SessionTypeId,
        ])
        .update_columns([Column::Config, Column::UpdatedAt])
        .to_owned();

        PluginConfigEntity::insert(am)
            .on_conflict(on_conflict)
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn delete(
        &self,
        plugin_instance_id: &str,
        session_type_id: Uuid,
    ) -> Result<(), ChatEngineError> {
        PluginConfigEntity::delete_many()
            .filter(Column::PluginInstanceId.eq(plugin_instance_id.to_owned()))
            .filter(Column::SessionTypeId.eq(session_type_id))
            .exec(&self.db)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// In-memory mock used to confirm the trait is object-safe and the service
    /// layer's `Arc<dyn PluginConfigRepo>` parameter compiles.
    struct MockRepo;

    #[async_trait]
    impl PluginConfigRepo for MockRepo {
        async fn find(
            &self,
            _plugin_instance_id: &str,
            _session_type_id: Uuid,
        ) -> Result<Option<JsonValue>, ChatEngineError> {
            Ok(None)
        }

        async fn upsert(
            &self,
            _plugin_instance_id: &str,
            _session_type_id: Uuid,
            _config: JsonValue,
        ) -> Result<(), ChatEngineError> {
            Ok(())
        }

        async fn delete(
            &self,
            _plugin_instance_id: &str,
            _session_type_id: Uuid,
        ) -> Result<(), ChatEngineError> {
            Ok(())
        }
    }

    #[test]
    fn trait_is_object_safe() {
        let _erased: Arc<dyn PluginConfigRepo> = Arc::new(MockRepo);
    }

    #[tokio::test]
    async fn mock_find_returns_none() {
        let repo: Arc<dyn PluginConfigRepo> = Arc::new(MockRepo);
        let got = repo.find("p1", Uuid::nil()).await.expect("ok");
        assert!(got.is_none());
    }
}
