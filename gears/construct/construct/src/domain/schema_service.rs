//! Reading and changing a person's profile schema: owner only, applied from the next message, kept across restarts.

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use construct_sdk::linked_schema::{Schema, default_schema};
use construct_sdk::schema_doc::SchemaDoc;
use toolkit_gts::gts_id;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use super::DbProvider;
use super::error::DomainError;
use super::profile_schema::{
    MAX_NAMES_CHARGED, ProfileSchemas, SchemaChange, SchemaEffects, SchemaLookup, SchemaStore,
    SubjectSchemaRepository, apply, names_brought_in, removed_node_kinds,
};
use super::subject_settings::{SubjectKey, ensure_owner, out_of_scope};

/// GTS type id of a person's profile schema: the authorization resource type of the schema routes.
pub(crate) const SUBJECT_SCHEMA_TYPE: &str = gts_id!("cf.construct.subject.schema.v1~");

/// Authorization resource type for a person's schema, scoped like their settings: by tenant, subject id and owner,
/// the person being the owner, so a policy can say "owner only" itself.
pub(crate) const SUBJECT_SCHEMA_RESOURCE: ResourceType = ResourceType::from_static(
    SUBJECT_SCHEMA_TYPE,
    &[
        pep_properties::OWNER_TENANT_ID,
        pep_properties::RESOURCE_ID,
        pep_properties::OWNER_ID,
    ],
);

pub(crate) mod actions {
    pub const GET: &str = "get";
    pub const UPDATE: &str = "update";
}

fn schema_request(key: SubjectKey) -> AccessRequest {
    AccessRequest::new()
        .resource_property(pep_properties::OWNER_TENANT_ID, key.tenant_id)
        .resource_property(pep_properties::OWNER_ID, key.subject_id)
}

/// What the owner is told when the schema is stored but the nodes of its removed kinds are not yet forgotten. The
/// cause is logged, not given: it can name what graph storage said.
pub(crate) const SWEEP_PENDING: &str = "the schema was changed, but the nodes of its removed kinds could not be removed yet; send an empty change (`{\"changes\": []}`) or any other change that does not add the kind back to finish; sending the same removal again is refused, because the kind is gone";

/// What the owner is told when another change was stored first.
pub(crate) const CONFLICT: &str =
    "the schema was changed by another call; read it again and repeat the change";

fn conflict() -> DomainError {
    DomainError::Unavailable(CONFLICT.to_owned())
}

/// What an audit trail records of one change of a person's schema.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaChanged {
    pub tenant_id: Uuid,
    pub subject_id: Uuid,
    pub caller: Uuid,
    /// How many changes the request held.
    pub changes: usize,
    /// The node kinds the change removed.
    pub removed_kinds: Vec<String>,
    /// The version the schema has now.
    pub version: i64,
}

/// What an audit trail records of nodes forgotten because the schema no longer has their kind.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodesForgotten {
    pub tenant_id: Uuid,
    pub subject_id: Uuid,
    pub nodes: usize,
}

/// Where the changes of schemas, and the nodes they leave to forget, are recorded, like the other changes of a
/// profile.
pub trait SchemaEvents: Send + Sync {
    fn changed(&self, event: SchemaChanged);

    fn nodes_forgotten(&self, event: NodesForgotten);
}

/// A person's schema and its version: how many times it was changed, an erasure counting as one. Version 0 is the
/// default schema of a person with no row.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct SchemaView {
    pub schema: SchemaDoc,
    pub version: i64,
}

/// [`ProfileSchemas`] over the stored schemas: a person whose host set none has the default schema. Read as the
/// gear: the callers have been authorized on their own.
#[domain_model]
pub struct StoredSchemas<R: SubjectSchemaRepository> {
    db: Arc<DbProvider>,
    repo: Arc<R>,
    default: Arc<Schema>,
}

impl<R: SubjectSchemaRepository> StoredSchemas<R> {
    #[must_use]
    pub fn new(db: Arc<DbProvider>, repo: Arc<R>) -> Self {
        Self {
            db,
            repo,
            default: Arc::new(default_schema()),
        }
    }
}

#[async_trait]
impl<R: SubjectSchemaRepository + 'static> ProfileSchemas for StoredSchemas<R> {
    async fn schema(&self, key: SubjectKey) -> Result<Arc<Schema>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        match self
            .repo
            .find(&conn, &AccessScope::allow_all(), key)
            .await?
        {
            SchemaLookup::Stored(stored) => Ok(Arc::new(stored.doc.build())),
            SchemaLookup::Absent => Ok(Arc::clone(&self.default)),
            SchemaLookup::OutOfScope(refusal) => Err(DomainError::internal(format!(
                "an unrestricted read of a schema was refused: {refusal:?}"
            ))),
        }
    }
}

#[domain_model]
pub struct SchemaService<R: SubjectSchemaRepository> {
    db: Arc<DbProvider>,
    repo: Arc<R>,
    effects: Arc<dyn SchemaEffects>,
    events: Arc<dyn SchemaEvents>,
    policy_enforcer: PolicyEnforcer,
    default: SchemaDoc,
}

impl<R: SubjectSchemaRepository + 'static> SchemaService<R> {
    pub fn new(
        db: Arc<DbProvider>,
        repo: Arc<R>,
        effects: Arc<dyn SchemaEffects>,
        events: Arc<dyn SchemaEvents>,
        policy_enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            db,
            repo,
            effects,
            events,
            policy_enforcer,
            default: SchemaDoc::of(&default_schema()),
        }
    }

    async fn scope(
        &self,
        ctx: &SecurityContext,
        key: SubjectKey,
        action: &str,
    ) -> Result<AccessScope, DomainError> {
        Ok(self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &SUBJECT_SCHEMA_RESOURCE,
                action,
                Some(key.subject_id),
                &schema_request(key),
            )
            .await?)
    }

    /// Forgets the nodes the schema has no kind for; `removed` names the kinds just removed, for the log.
    async fn forget_or_say(&self, key: SubjectKey, removed: &[String]) -> Result<(), DomainError> {
        let forgotten = self.effects.forget(key).await.map_err(|error| {
            tracing::error!(
                %error,
                ?removed,
                "construct: the nodes of the removed kinds were not forgotten"
            );
            DomainError::Unavailable(SWEEP_PENDING.to_owned())
        })?;
        if forgotten > 0 {
            self.events.nodes_forgotten(NodesForgotten {
                tenant_id: key.tenant_id,
                subject_id: key.subject_id,
                nodes: forgotten,
            });
        }
        Ok(())
    }

    async fn current(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        key: SubjectKey,
    ) -> Result<(SchemaView, Option<i64>, i64), DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        match self.repo.find(&conn, scope, key).await? {
            SchemaLookup::Stored(stored) => {
                let version = stored.version;
                Ok((
                    SchemaView {
                        schema: stored.doc,
                        version,
                    },
                    Some(version),
                    stored.names_charged,
                ))
            }
            SchemaLookup::Absent => Ok((
                SchemaView {
                    schema: self.default.clone(),
                    version: 0,
                },
                None,
                0,
            )),
            SchemaLookup::OutOfScope(refusal) => Err(out_of_scope(ctx, key, refusal)),
        }
    }

    /// The schema of the person `subject_id` in the caller's tenant. Owner only.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] for another person's schema, or a refusal of the policy.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), subject_id = %subject_id))]
    pub async fn schema(
        &self,
        ctx: &SecurityContext,
        subject_id: Uuid,
    ) -> Result<SchemaView, DomainError> {
        ensure_owner(ctx, subject_id)?;
        let key = SubjectKey {
            tenant_id: ctx.subject_tenant_id(),
            subject_id,
        };
        let scope = self.scope(ctx, key, actions::GET).await?;
        Ok(self.current(ctx, &scope, key).await?.0)
    }

    /// Makes `changes` to the schema of the person `subject_id`, all or none. Owner only. The types of a kind added
    /// are registered in graph storage before the new schema is stored, and the nodes of a kind removed are forgotten
    /// after it; the next message is planned and written over it.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidValue`] when a change names what does not exist or the result breaks a rule, or graph
    /// storage refuses the types: the schema is not changed (a refusal by a rule charges nothing; any failure after the charge, such as a refusal by graph storage, leaves
    /// the names the change would have made the process keep charged). [`DomainError::Unavailable`] when the schema was changed by another call meanwhile: nothing
    /// is stored, and the call may be repeated. [`DomainError::Unavailable`] also when
    /// the schema is stored but the removal of the nodes of a removed kind failed: the nodes stay in graph storage,
    /// hidden from every read.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), subject_id = %subject_id))]
    pub async fn change(
        &self,
        ctx: &SecurityContext,
        subject_id: Uuid,
        changes: &[SchemaChange],
    ) -> Result<SchemaView, DomainError> {
        ensure_owner(ctx, subject_id)?;
        let key = SubjectKey {
            tenant_id: ctx.subject_tenant_id(),
            subject_id,
        };
        let scope = self.scope(ctx, key, actions::UPDATE).await?;
        let (before, expected, introduced) = self.current(ctx, &scope, key).await?;
        let next = apply(&before.schema, changes)?;
        if next == before.schema {
            // Nothing to change: a change that does nothing finishes a removal an earlier one could not, so the
            // owner finishes a removal by sending an empty change.
            self.forget_or_say(key, &[]).await?;
            return Ok(before);
        }
        let conn = self.db.conn().map_err(DomainError::from)?;
        // The names the change brings in are charged to the person before the process keeps any, so a change that
        // is then refused, or loses the compare-and-move, has paid for what it leaked; and of two changes on one
        // version only one is charged and goes on.
        let fresh = i64::try_from(names_brought_in(&before.schema, &next)).unwrap_or(i64::MAX);
        let charged = introduced.saturating_add(fresh);
        if charged > MAX_NAMES_CHARGED {
            return Err(DomainError::InvalidValue {
                field: "schema",
                rule: format!(
                    "a person may make the process keep at most {MAX_NAMES_CHARGED} kind, property and field names over the life of their schema, removals not refunded"
                ),
            });
        }
        let expected = if fresh > 0 {
            match self
                .repo
                .reserve_names(
                    &conn,
                    &scope,
                    key,
                    expected,
                    &before.schema,
                    introduced,
                    charged,
                )
                .await?
            {
                SchemaStore::Stored(version) => Some(version),
                SchemaStore::Conflict => return Err(conflict()),
                SchemaStore::OutOfScope(refusal) => return Err(out_of_scope(ctx, key, refusal)),
            }
        } else {
            expected
        };
        let removed = removed_node_kinds(&before.schema, &next);
        self.effects.register(key, &next.build()).await?;
        let stored = self
            .repo
            .store(&conn, &scope, key, expected, &next, charged)
            .await?;
        match stored {
            SchemaStore::Stored(version) => {
                // The schema is stored first, so a conflict or a refusal never costs the person their nodes. The
                // nodes of a removed kind are hidden from every read from here on, whether or not the removal
                // below succeeds.
                let forgotten = self.forget_or_say(key, &removed).await;
                self.events.changed(SchemaChanged {
                    tenant_id: key.tenant_id,
                    subject_id: key.subject_id,
                    caller: ctx.subject_id(),
                    changes: changes.len(),
                    removed_kinds: removed,
                    version,
                });
                forgotten?;
                Ok(SchemaView {
                    schema: next,
                    version,
                })
            }
            SchemaStore::Conflict => Err(conflict()),
            SchemaStore::OutOfScope(refusal) => Err(out_of_scope(ctx, key, refusal)),
        }
    }
}
