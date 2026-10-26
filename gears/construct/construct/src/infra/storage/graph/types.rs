use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use construct_sdk::profile_types::graph_types;
use graph_storage_sdk::GraphStorageClientV1;
use graph_storage_sdk::models::TypeRegistration;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{DEFAULT_CALL_TIMEOUT, bounded, graph_error};
use crate::domain::error::DomainError;

/// Registers Construct's graph types in a tenant before the first write there.
/// Graph storage keeps types per tenant. Registering the same schemas again
/// converges, so a restart, or two instances registering at once, is safe; the
/// tenants already done are remembered for the life of the process only, and a
/// registration that failed is not remembered.
///
/// @cpt-dod:cpt-cf-construct-dod-profile-writer-registration:p1
pub struct GraphTypes {
    client: Arc<dyn GraphStorageClientV1>,
    timeout: Duration,
    registered: Mutex<HashSet<Uuid>>,
}

impl GraphTypes {
    #[must_use]
    pub fn new(client: Arc<dyn GraphStorageClientV1>) -> Self {
        Self {
            client,
            timeout: DEFAULT_CALL_TIMEOUT,
            registered: Mutex::default(),
        }
    }

    /// Replace [`DEFAULT_CALL_TIMEOUT`] for the registration call.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn done(&self, tenant_id: Uuid) -> bool {
        self.registered
            .lock()
            .is_ok_and(|registered| registered.contains(&tenant_id))
    }

    /// Make sure the types exist in the context's tenant.
    ///
    /// # Errors
    ///
    /// When graph storage refuses or cannot take the registration, or does not
    /// answer in time.
    pub async fn ensure(&self, ctx: &SecurityContext) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-register
        let tenant_id = ctx.subject_tenant_id();
        if self.done(tenant_id) {
            return Ok(());
        }
        let batch = graph_types()
            .into_iter()
            .map(|graph_type| {
                serde_json::from_str(graph_type.schema)
                    .map(|schema| TypeRegistration {
                        type_id: graph_type.type_id.to_owned(),
                        schema,
                    })
                    .map_err(|err| {
                        DomainError::internal(format!(
                            "graph type {} has no valid schema: {err}",
                            graph_type.type_id
                        ))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        bounded(
            self.timeout,
            "type registration",
            self.client.register_types(ctx, batch),
        )
        .await?
        .map_err(|err| graph_error("type registration", &err))?;
        if let Ok(mut registered) = self.registered.lock() {
            registered.insert(tenant_id);
        }
        Ok(())
        // @cpt-end:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-register
    }
}
