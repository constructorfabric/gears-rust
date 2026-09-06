// Created: 2026-09-06 by Constructor Tech
//! Fakes and fixtures shared by this crate's unit tests.
//!
//! Every fake here stands in for a port the gear resolves from the platform at
//! init — the types registry, the audit sink, the tenant resolver — so a test
//! can run the real domain code and the real repositories over an in-memory
//! database without a platform around it.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use sea_orm_migration::MigratorTrait;
use serde_json::Value;
use settings_service_sdk::SettingKey;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use types_registry_sdk::{GtsTypeId, GtsTypeSchema};
use uuid::Uuid;

use crate::audit::{AuditEmitter, AuditRecord};
use crate::domain::contribution::SettingTypeRegistrar;
use crate::domain::error::DomainError;
use crate::domain::platform_scope::PlatformScope;
use crate::infra::type_validator::SchemaSource;

/// A registry standing in for the value-type catalogue.
#[derive(Default)]
pub struct FakeSource {
    pub schemas: HashMap<String, GtsTypeSchema>,
    pub instances: HashSet<String>,
    pub unavailable: bool,
}

impl FakeSource {
    pub fn with_type(mut self, id: &str, schema: Value) -> Self {
        let schema = GtsTypeSchema::try_new(GtsTypeId::new(id), schema, None, None)
            .expect("fixture schema is a valid root type");
        self.schemas.insert(id.to_owned(), schema);
        self
    }

    pub fn with_instance(mut self, id: &str) -> Self {
        self.instances.insert(id.to_owned());
        self
    }
}

#[async_trait]
impl SchemaSource for FakeSource {
    async fn type_schema(&self, type_id: &str) -> Result<Option<GtsTypeSchema>, DomainError> {
        if self.unavailable {
            return Err(DomainError::Unavailable {
                detail: "registry down".to_owned(),
            });
        }
        Ok(self.schemas.get(type_id).cloned())
    }

    async fn instance_exists(&self, instance_id: &str) -> Result<bool, DomainError> {
        Ok(self.instances.contains(instance_id))
    }
}

/// An audit sink that keeps what it was given.
#[derive(Default)]
pub struct RecordingAudit {
    pub records: Mutex<Vec<AuditRecord>>,
}

impl RecordingAudit {
    pub fn actions(&self) -> Vec<String> {
        self.records
            .lock()
            .expect("audit lock")
            .iter()
            .map(|r| r.action.clone())
            .collect()
    }
}

#[async_trait]
impl AuditEmitter for RecordingAudit {
    async fn audit(&self, record: AuditRecord) -> Result<(), DomainError> {
        self.records.lock().expect("audit lock").push(record);
        Ok(())
    }
}

/// A platform scope with a fixed root tenant.
pub struct FixedScope(pub Uuid);

#[async_trait]
impl PlatformScope for FixedScope {
    async fn root_tenant(&self) -> Result<Uuid, DomainError> {
        Ok(self.0)
    }
}

/// A setting-type registrar that records the keys it was asked to register,
/// and can be told to fail.
#[derive(Default)]
pub struct RecordingRegistrar {
    pub registered: Mutex<Vec<(String, String)>>,
    pub fail: bool,
}

#[async_trait]
impl SettingTypeRegistrar for RecordingRegistrar {
    async fn register_setting_type(
        &self,
        key: &SettingKey,
        value_type_id: &str,
    ) -> Result<(), DomainError> {
        if self.fail {
            return Err(DomainError::Unavailable {
                detail: "types registry down".to_owned(),
            });
        }
        self.registered
            .lock()
            .expect("registrar lock")
            .push((key.to_string(), value_type_id.to_owned()));
        Ok(())
    }
}

/// A fresh in-memory `SQLite` database with this gear's migrations applied.
///
/// One connection, because `SQLite` `:memory:` is per-connection: every
/// repository and every transaction must see the same schema and data.
pub async fn sqlite_provider() -> Arc<DBProvider<DbError>> {
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db("sqlite::memory:", opts)
        .await
        .expect("in-memory sqlite connects");
    run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .map_err(|e| e.to_string())
    .expect("migrations apply on sqlite");
    Arc::new(DBProvider::new(db))
}
