use std::sync::Arc;

use async_trait::async_trait;
use construct_sdk::linked_schema::Schema;

use crate::domain::error::DomainError;
use crate::domain::profile_schema::SchemaEffects;
use crate::domain::profile_writes::{ProfileWrites, WriteError};
use crate::domain::subject_settings::SubjectKey;
use crate::infra::storage::graph::types::GraphTypes;

/// [`SchemaEffects`] in graph storage: the types are registered as Construct, and the nodes of a removed kind are
/// removed the way the person's own forget removes them.
pub struct GraphSchemaEffects {
    types: Arc<GraphTypes>,
    writes: Arc<ProfileWrites>,
}

impl GraphSchemaEffects {
    #[must_use]
    pub fn new(types: Arc<GraphTypes>, writes: Arc<ProfileWrites>) -> Self {
        Self { types, writes }
    }
}

#[async_trait]
impl SchemaEffects for GraphSchemaEffects {
    async fn register(&self, key: SubjectKey, schema: &Schema) -> Result<(), DomainError> {
        let (construct, _) = self.writes.construct(key.tenant_id).await?;
        self.types
            .ensure(&construct, key.subject_id, schema)
            .await
            .map_err(|error| match error {
                // A refusal is the answer to the host; the reason is graph storage's.
                DomainError::Internal(reason) => DomainError::InvalidValue {
                    field: "schema",
                    rule: reason,
                },
                other => other,
            })
    }

    async fn forget(&self, key: SubjectKey) -> Result<usize, DomainError> {
        let (construct, _) = self.writes.construct(key.tenant_id).await?;
        self.writes
            .forget_orphans(&construct, key, self.writes.deadline())
            .await
            .map_err(|error| match error {
                WriteError::NotWritten(error) | WriteError::MaybeWritten(error) => error,
            })
    }
}
