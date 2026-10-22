//! [`BindingSource`] over the gear's database: each read on a fresh
//! connection, bounded by `store_timeout`.

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use async_trait::async_trait;
use toolkit_db::Db;
use toolkit_db::secure::DbConn;
use uuid::Uuid;

use crate::domain::model::{Document, VersionId};
use crate::domain::ports::{ActiveBinding, BindingSource, PortError};
use crate::domain::repos::ActiveContentLoader;
use crate::domain::repos::RepoError;
use crate::infra::storage::content_repo::OrmActiveContentLoader;

/// The database-backed [`BindingSource`].
#[derive(Clone)]
pub struct DbBindingSource {
    db: Db,
    loader: OrmActiveContentLoader,
    timeout: Duration,
}

impl std::fmt::Debug for DbBindingSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbBindingSource")
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl DbBindingSource {
    /// Reads through `db`, giving up after `timeout`.
    #[must_use]
    pub fn new(db: Db, timeout: Duration) -> Self {
        Self {
            db,
            loader: OrmActiveContentLoader,
            timeout,
        }
    }
}

impl DbBindingSource {
    /// Runs `read` on a fresh connection under the store timeout.
    async fn read<'s, T, F, Fut>(&'s self, read: F) -> Result<T, PortError>
    where
        F: FnOnce(DbConn<'s>) -> Fut,
        Fut: Future<Output = Result<T, RepoError>>,
    {
        let attempt = async {
            let conn = self
                .db
                .conn()
                .map_err(|e| PortError::Unavailable(format!("policy store connection: {e}")))?;
            read(conn)
                .await
                .map_err(|e| PortError::Unavailable(format!("policy store read: {e}")))
        };
        tokio::time::timeout(self.timeout, attempt)
            .await
            .map_err(|_| PortError::Timeout)?
    }
}

#[async_trait]
impl BindingSource for DbBindingSource {
    async fn active_bindings(&self, tenants: &[Uuid]) -> Result<Vec<ActiveBinding>, PortError> {
        self.read(|conn| async move { self.loader.load_for_tenants(&conn, tenants).await })
            .await
    }

    async fn version_documents(
        &self,
        versions: &[VersionId],
    ) -> Result<HashMap<VersionId, Vec<Document>>, PortError> {
        self.read(|conn| async move { self.loader.load_documents(&conn, versions).await })
            .await
    }
}
