//! [`BindingSource`] over the gear's database: one repository read per
//! request on a fresh connection.

use async_trait::async_trait;
use toolkit_db::Db;
use uuid::Uuid;

use crate::domain::ports::{ActiveBinding, BindingSource, PortError};
use crate::domain::repos::ActiveContentLoader;
use crate::infra::storage::content_repo::OrmActiveContentLoader;

/// The database-backed [`BindingSource`].
#[derive(Clone)]
pub struct DbBindingSource {
    db: Db,
    loader: OrmActiveContentLoader,
}

impl std::fmt::Debug for DbBindingSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbBindingSource").finish_non_exhaustive()
    }
}

impl DbBindingSource {
    /// Reads through `db`.
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self {
            db,
            loader: OrmActiveContentLoader,
        }
    }
}

#[async_trait]
impl BindingSource for DbBindingSource {
    async fn active_bindings(&self, tenants: &[Uuid]) -> Result<Vec<ActiveBinding>, PortError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| PortError::Unavailable(e.to_string()))?;
        self.loader
            .load_for_tenants(&conn, tenants)
            .await
            .map_err(|e| PortError::Unavailable(e.to_string()))
    }
}
