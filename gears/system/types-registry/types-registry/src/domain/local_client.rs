//! In-process platform and tenant APIs over `RegistryService` (SPEC §10.1, D15, D17).
//! Shares the cursor encoding and errors with REST; validators are the domain tokens. Outbox mutations read back accepted
//! operations; failures return Aborted with `operation_id` for same-key replay (D19).
//! Contexts stay unvalidated with no principal (C2); publisher reaches the service at T45/Phase 9.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use gts::GtsId;
use serde_json::value::RawValue;
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use types_registry_sdk as sdk;
use types_registry_sdk::item_failure::AdmissionFailureReason;
use types_registry_sdk::{AdmissionFailure, PlatformTypesRegistryApi, TypesRegistryApi};
use uuid::Uuid;

use crate::domain::admission::{
    Candidate, DeleteRequest, DeleteTarget, StoredFailure, SubmitRequest, UnreadableFailure,
};
use crate::domain::cursor::{self, Binding};
use crate::domain::enums::{
    EntityKind, LifecycleFilter, LifecycleStatus, OperationItemStatus, OperationKind,
    OperationStatus,
};
use crate::domain::key::EntityKey;
use crate::domain::registry_service::{
    BatchGetItem, DiscoveryQuery, EntityLookup, EntityRecord, MAX_KEY_LEN, OperationItemRecord,
    OperationRecord, RegistryService, ServiceError,
};
use crate::domain::selection::{EntityField, FieldSelection};
use crate::domain::validator::{IfNoneMatch, Validator};

/// [`PlatformTypesRegistryApi`] and [`TypesRegistryApi`] served from this process's
/// [`RegistryService`]; the tenant reads are the platform reads.
#[domain_model]
pub struct LocalClient {
    service: Arc<RegistryService>,
}

impl LocalClient {
    #[must_use]
    pub fn new(service: Arc<RegistryService>) -> Self {
        Self { service }
    }

    /// The operation an accepted submit created, read back (D19).
    async fn read_back(&self, operation_id: Uuid) -> Result<OperationRecord, CanonicalError> {
        match self.service.operation(operation_id).await {
            Ok(Some(record)) => Ok(record),
            Ok(None) => Err(read_back_failed(operation_id, "it could not be found")),
            Err(e) => {
                tracing::warn!(
                    %operation_id,
                    cause = e.cause_kind(),
                    "types_registry could not read back an accepted operation"
                );
                Err(read_back_failed(operation_id, "the read failed"))
            }
        }
    }

    /// The batch read both contracts serve.
    async fn batch_get(
        &self,
        request: sdk::BatchGetEntitiesRequest,
    ) -> Result<sdk::BatchGetEntitiesResponse, CanonicalError> {
        let selection = selection(&request.projection);
        let mut asked: HashMap<EntityKey, sdk::EntityKey> =
            HashMap::with_capacity(request.items.len());
        let mut reads = Vec::with_capacity(request.items.len());
        for item in request.items {
            let key = domain_key(&item.key);
            let if_none_match = match &item.if_none_match {
                Some(validator) => condition(validator)?,
                None => None,
            };
            asked.entry(key.clone()).or_insert(item.key);
            reads.push(BatchGetItem { key, if_none_match });
        }

        let results = self
            .service
            .batch_get(&reads, selection)
            .await
            .map_err(CanonicalError::from)?;

        let mut lookups = HashMap::with_capacity(results.len());
        for (key, lookup) in results {
            let Some(asked_key) = asked.remove(&key) else {
                tracing::error!(unexpected_key = ?key, "types_registry batch read answered a key it was not asked");
                return Err(CanonicalError::internal(
                    "the registry could not match a batch read result",
                )
                .create());
            };
            lookups.insert(asked_key, lookup_from(lookup)?);
        }
        Ok(sdk::BatchGetEntitiesResponse(lookups))
    }

    /// The discovery page both contracts serve.
    async fn list(
        &self,
        query: sdk::ListEntitiesRequest,
    ) -> Result<sdk::ListEntitiesResponse, CanonicalError> {
        let mut discovery = DiscoveryQuery {
            pattern: query.filter.pattern.as_ref().map(ToString::to_string),
            after: None,
            limit: query.page.limit.map(u64::from),
            kind: query.filter.kind.map(domain_kind),
            lifecycle: domain_lifecycle(query.filter.lifecycle),
            max_chain_depth: query.filter.max_chain_depth,
            selection: selection(&query.projection),
        };
        if let Some(position) = &query.page.cursor {
            let token = cursor::read(position.as_str())?;
            discovery.after = Some(cursor::resume(&token, &Binding::from(&discovery))?);
        }

        let page = self
            .service
            .discover(&discovery)
            .await
            .map_err(CanonicalError::from)?;

        let next = page
            .next_after
            .as_deref()
            .map(|after| cursor::encode(after, &Binding::from(&discovery)))
            .transpose()?
            .map(sdk::Cursor::from_token);
        Ok(sdk::ListEntitiesResponse {
            items: page
                .items
                .into_iter()
                .map(snapshot_from)
                .collect::<Result<_, _>>()?,
            next,
        })
    }
}

#[async_trait]
impl PlatformTypesRegistryApi for LocalClient {
    async fn batch_get_entities(
        &self,
        _ctx: &PlatformSecurityContext,
        request: sdk::BatchGetEntitiesRequest,
    ) -> Result<sdk::BatchGetEntitiesResponse, CanonicalError> {
        self.batch_get(request).await
    }

    async fn list_entities(
        &self,
        _ctx: &PlatformSecurityContext,
        query: sdk::ListEntitiesRequest,
    ) -> Result<sdk::ListEntitiesResponse, CanonicalError> {
        self.list(query).await
    }

    async fn register_entities(
        &self,
        _ctx: &PlatformSecurityContext,
        key: sdk::IdempotencyKey,
        request: sdk::RegisterEntitiesRequest,
    ) -> Result<sdk::RegistrationOperation, CanonicalError> {
        let candidates = request
            .items
            .into_iter()
            .map(|item| {
                let gts_id = item.gts_id.to_string();
                let expected_resource_version = item
                    .expected_resource_version
                    .map(|v| stored_version(&gts_id, v))
                    .transpose()?;
                Ok(Candidate {
                    gts_id,
                    content: Some(item.content),
                    expected_resource_version,
                    force: item.force,
                })
            })
            .collect::<Result<Vec<_>, CanonicalError>>()?;
        let submit = SubmitRequest {
            idempotency_key: Some(key.as_str().to_owned()),
            dry_run: request.dry_run,
            candidates,
        };

        let accepted = self
            .service
            .submit(&submit, time::OffsetDateTime::now_utc())
            .await
            .map_err(CanonicalError::from)?;
        let operation_id = accepted.operation_id;
        match operation_from(self.read_back(operation_id).await?)
            .map_err(|e| unrepresentable(operation_id, &e))?
        {
            sdk::Operation::Registration(operation) => Ok(operation),
            sdk::Operation::Deletion(_) => Err(wrong_kind(operation_id)),
        }
    }

    async fn delete_entities(
        &self,
        _ctx: &PlatformSecurityContext,
        key: sdk::IdempotencyKey,
        request: sdk::DeleteEntitiesRequest,
    ) -> Result<sdk::DeletionOperation, CanonicalError> {
        let targets = request
            .items
            .into_iter()
            .map(|item| {
                let key = domain_key(&item.key);
                let expected_resource_version =
                    stored_version(&key.to_string(), item.expected_resource_version)?;
                Ok(DeleteTarget {
                    key,
                    expected_resource_version: Some(expected_resource_version),
                })
            })
            .collect::<Result<Vec<_>, CanonicalError>>()?;
        let delete = DeleteRequest {
            idempotency_key: Some(key.as_str().to_owned()),
            dry_run: request.dry_run,
            targets,
        };

        let accepted = self
            .service
            .delete(&delete, time::OffsetDateTime::now_utc())
            .await
            .map_err(CanonicalError::from)?;
        let operation_id = accepted.operation_id;
        match operation_from(self.read_back(operation_id).await?)
            .map_err(|e| unrepresentable(operation_id, &e))?
        {
            sdk::Operation::Deletion(operation) => Ok(operation),
            sdk::Operation::Registration(_) => Err(wrong_kind(operation_id)),
        }
    }

    async fn get_operation(
        &self,
        _ctx: &PlatformSecurityContext,
        operation_id: Uuid,
    ) -> Result<sdk::Operation, CanonicalError> {
        let record = self
            .service
            .operation(operation_id)
            .await
            .map_err(CanonicalError::from)?
            .ok_or(LocalClientError::OperationNotFound { operation_id })?;
        operation_from(record)
    }
}

/// The tenant reads are the platform's own (D17): the same lookups, encodings and errors.
///
/// C2/C6: P0 applies no tenant scope and records no principal from the `SecurityContext`;
/// the context is accepted, not validated, exactly as the platform context is.
#[async_trait]
impl TypesRegistryApi for LocalClient {
    async fn batch_get_entities(
        &self,
        _ctx: &SecurityContext,
        request: sdk::BatchGetEntitiesRequest,
    ) -> Result<sdk::BatchGetEntitiesResponse, CanonicalError> {
        self.batch_get(request).await
    }

    async fn list_entities(
        &self,
        _ctx: &SecurityContext,
        query: sdk::ListEntitiesRequest,
    ) -> Result<sdk::ListEntitiesResponse, CanonicalError> {
        self.list(query).await
    }
}

// ---- errors -----------------------------------------------------------------

/// What only the local client refuses: after an accepted submit (D19), and where an SDK
/// value cannot reach the service. The API ladder writes each one's wire form.
#[domain_model]
#[derive(Debug, thiserror::Error)]
pub enum LocalClientError {
    /// The submit was accepted, but its operation could not be read back.
    #[error("operation {operation_id} was accepted, but {why}")]
    ReadBackFailed {
        operation_id: Uuid,
        why: &'static str,
    },
    /// The read-back operation is of the other kind.
    #[error("operation {operation_id} was read back as an operation of the other kind")]
    WrongKind { operation_id: Uuid },
    /// The read-back operation has no SDK representation.
    #[error("operation {operation_id} could not be represented")]
    Unrepresentable { operation_id: Uuid },
    /// No operation has this id.
    #[error("no operation with id {operation_id}")]
    OperationNotFound { operation_id: Uuid },
    /// An `expected_resource_version` above any version the registry issues.
    #[error("{version} is not a resource version this registry issues")]
    VersionOutOfRange { key: String, version: u64 },
}

/// Aborted read-back after acceptance; names the operation for same-key replay.
fn read_back_failed(operation_id: Uuid, why: &'static str) -> CanonicalError {
    LocalClientError::ReadBackFailed { operation_id, why }.into()
}

/// Wrong-kind read-back names the accepted operation (D19).
fn wrong_kind(operation_id: Uuid) -> CanonicalError {
    tracing::error!(%operation_id, "types_registry read back an operation of the other kind");
    LocalClientError::WrongKind { operation_id }.into()
}

/// Conversion failure still names the accepted operation.
fn unrepresentable(operation_id: Uuid, error: &CanonicalError) -> CanonicalError {
    tracing::error!(%operation_id, %error, "types_registry cannot represent an accepted operation");
    LocalClientError::Unrepresentable { operation_id }.into()
}

/// Corrupt stored value, never caller input; log its row/value subject and failure cause.
fn corrupt(
    what: &str,
    subject: &dyn std::fmt::Display,
    cause: &dyn std::fmt::Display,
) -> CanonicalError {
    tracing::error!(
        what,
        %subject,
        %cause,
        "types_registry local client met an unrepresentable stored value"
    );
    CanonicalError::internal("the registry could not represent a stored value").create()
}

/// The condition an SDK validator states: the domain's token text, bounded like a key.
/// Bytes that are not UTF-8 cannot be a token this registry issued, so the condition is
/// unusable and the read proceeds unconditionally (DESIGN §3.3) — after the bound, so an
/// oversized value is refused whatever it holds.
fn condition(validator: &sdk::Validator) -> Result<Option<IfNoneMatch>, CanonicalError> {
    let bytes = validator.as_bytes();
    if bytes.len() > MAX_KEY_LEN {
        return Err(ServiceError::ValidatorTooLong { len: bytes.len() }.into());
    }
    Ok(std::str::from_utf8(bytes)
        .ok()
        .map(|token| IfNoneMatch::Validators(vec![token.to_owned()])))
}

/// `u64` precondition to the service's `i64`; above `i64::MAX` cannot exist. Refused in
/// the ladder's own shape for an unusable `expected_resource_version`.
fn stored_version(key: &str, version: u64) -> Result<i64, CanonicalError> {
    i64::try_from(version).map_err(|_| {
        LocalClientError::VersionOutOfRange {
            key: key.to_owned(),
            version,
        }
        .into()
    })
}

fn sdk_version(version: i64, subject: &dyn std::fmt::Display) -> Result<u64, CanonicalError> {
    u64::try_from(version).map_err(|e| corrupt("negative resource_version", subject, &e))
}

// ---- keys, selection, enums --------------------------------------------------

fn domain_key(key: &sdk::EntityKey) -> EntityKey {
    match key {
        sdk::EntityKey::GtsId(id) => EntityKey::GtsId(id.to_string()),
        sdk::EntityKey::GtsUuid(uuid) => EntityKey::Uuid(*uuid),
    }
}

fn sdk_key(key: EntityKey) -> Result<sdk::EntityKey, CanonicalError> {
    match key {
        EntityKey::GtsId(id) => Ok(sdk::EntityKey::GtsId(gts_id(&id)?)),
        EntityKey::Uuid(uuid) => Ok(sdk::EntityKey::GtsUuid(uuid)),
    }
}

fn gts_id(id: &str) -> Result<GtsId, CanonicalError> {
    GtsId::try_new(id).map_err(|e| corrupt("stored gts_id", &id.escape_debug(), &e))
}

/// The typed selection, field by field: no names are spelled and re-parsed.
fn selection(projection: &sdk::Projection) -> FieldSelection {
    FieldSelection::from_fields(projection.normalized().fields().map(domain_field))
}

const fn domain_field(field: sdk::EntityField) -> EntityField {
    match field {
        sdk::EntityField::GtsId => EntityField::GtsId,
        sdk::EntityField::GtsUuid => EntityField::GtsUuid,
        sdk::EntityField::Kind => EntityField::Kind,
        sdk::EntityField::LifecycleStatus => EntityField::LifecycleStatus,
        sdk::EntityField::Origin => EntityField::Origin,
        sdk::EntityField::Content => EntityField::Content,
        sdk::EntityField::ResolvedSchema => EntityField::ResolvedSchema,
        sdk::EntityField::EffectiveTraits => EntityField::EffectiveTraits,
        sdk::EntityField::EffectiveTraitsSchema => EntityField::EffectiveTraitsSchema,
        sdk::EntityField::Provenance => EntityField::Provenance,
    }
}

const fn domain_kind(kind: sdk::EntityKind) -> EntityKind {
    match kind {
        sdk::EntityKind::TypeSchema => EntityKind::TypeSchema,
        sdk::EntityKind::Instance => EntityKind::Instance,
    }
}

const fn sdk_kind(kind: EntityKind) -> sdk::EntityKind {
    match kind {
        EntityKind::TypeSchema => sdk::EntityKind::TypeSchema,
        EntityKind::Instance => sdk::EntityKind::Instance,
    }
}

const fn domain_lifecycle(filter: sdk::LifecycleFilter) -> LifecycleFilter {
    match filter {
        sdk::LifecycleFilter::Active => LifecycleFilter::Active,
        sdk::LifecycleFilter::Deleted => LifecycleFilter::Deleted,
        sdk::LifecycleFilter::All => LifecycleFilter::All,
    }
}

const fn sdk_lifecycle(status: LifecycleStatus) -> sdk::LifecycleStatus {
    match status {
        LifecycleStatus::Active => sdk::LifecycleStatus::Active,
        LifecycleStatus::Deleted => sdk::LifecycleStatus::Deleted,
    }
}

const fn sdk_operation_status(status: OperationStatus) -> sdk::OperationStatus {
    match status {
        OperationStatus::Pending => sdk::OperationStatus::Pending,
        OperationStatus::Running => sdk::OperationStatus::Running,
        OperationStatus::Completed => sdk::OperationStatus::Completed,
    }
}

const fn sdk_candidate_status(status: OperationItemStatus) -> sdk::CandidateStatus {
    match status {
        OperationItemStatus::Pending => sdk::CandidateStatus::Pending,
        OperationItemStatus::Running => sdk::CandidateStatus::Running,
        OperationItemStatus::Succeeded => sdk::CandidateStatus::Succeeded,
        OperationItemStatus::Unchanged => sdk::CandidateStatus::Unchanged,
        OperationItemStatus::Failed => sdk::CandidateStatus::Failed,
    }
}

// ---- reads ------------------------------------------------------------------

/// The domain's validator token (§8.5). RFC 9110 quoting is REST's representation only.
fn sdk_validator(validator: Validator) -> sdk::Validator {
    sdk::Validator::from_bytes(validator.encode().into_bytes())
}

fn lookup_from(lookup: EntityLookup) -> Result<sdk::EntityLookup, CanonicalError> {
    Ok(match lookup {
        EntityLookup::Found { record, etag } => sdk::EntityLookup::Found {
            entity: Box::new(snapshot_from(record)?),
            etag: sdk_validator(etag),
        },
        EntityLookup::Unchanged { etag } => sdk::EntityLookup::Unchanged {
            etag: sdk_validator(etag),
        },
        EntityLookup::NotFound => sdk::EntityLookup::NotFound,
    })
}

/// A selected JSON `null` stays `Some(Value::Null)`; `None` stays unselected.
fn document(
    raw: Option<Box<RawValue>>,
    gts_id: &str,
    field: EntityField,
) -> Result<Option<sdk::JsonDocument>, CanonicalError> {
    raw.map(|raw| {
        serde_json::from_str(raw.get()).map_err(|e| {
            corrupt(
                "stored document",
                &format_args!("{} {}", gts_id.escape_debug(), field.name()),
                &e,
            )
        })
    })
    .transpose()
}

fn snapshot_from(record: EntityRecord) -> Result<sdk::Entity, CanonicalError> {
    let id = record.gts_id.as_str();
    Ok(sdk::Entity {
        gts_id: gts_id(&record.gts_id)?,
        gts_uuid: record.gts_uuid,
        kind: sdk_kind(record.kind),
        lifecycle_status: sdk_lifecycle(record.lifecycle_status),
        origin: record
            .origin
            .map(|origin| {
                Ok::<_, CanonicalError>(sdk::Origin::Managed {
                    resource_version: sdk_version(origin.resource_version, &id.escape_debug())?,
                    created_at: origin.created_at,
                    updated_at: origin.updated_at,
                })
            })
            .transpose()?,
        content: document(record.content, id, EntityField::Content)?,
        resolved_schema: document(record.resolved_schema, id, EntityField::ResolvedSchema)?,
        effective_traits: document(record.effective_traits, id, EntityField::EffectiveTraits)?,
        effective_traits_schema: document(
            record.effective_traits_schema,
            id,
            EntityField::EffectiveTraitsSchema,
        )?,
        provenance: record.provenance.map(|p| sdk::Provenance {
            gts_spec_version: p.gts_spec_version,
            gts_impl_version: p.gts_impl_version,
            compat_forced: p.compat_forced,
        }),
    })
}

// ---- operations -------------------------------------------------------------

fn operation_from(record: OperationRecord) -> Result<sdk::Operation, CanonicalError> {
    let operation_id = record.operation_id;
    let status = sdk_operation_status(record.status);
    Ok(match record.kind {
        OperationKind::Registration => sdk::Operation::Registration(sdk::RegistrationOperation {
            operation_id,
            status,
            items: record
                .items
                .into_iter()
                .map(|item| {
                    let (key, status, resource_version, error) = item_parts(item, operation_id)?;
                    let sdk::EntityKey::GtsId(gts_id) = key else {
                        return Err(corrupt(
                            "registration item keyed by reference",
                            &operation_id,
                            &"a registration names its candidates by identifier",
                        ));
                    };
                    Ok(sdk::RegistrationItemResult {
                        gts_id,
                        status,
                        resource_version,
                        error,
                    })
                })
                .collect::<Result<_, CanonicalError>>()?,
        }),
        OperationKind::Deletion => sdk::Operation::Deletion(sdk::DeletionOperation {
            operation_id,
            status,
            items: record
                .items
                .into_iter()
                .map(|item| {
                    let (entity_key, status, resource_version, error) =
                        item_parts(item, operation_id)?;
                    Ok(sdk::DeletionItemResult {
                        entity_key,
                        status,
                        resource_version,
                        error,
                    })
                })
                .collect::<Result<_, CanonicalError>>()?,
        }),
    })
}

type ItemParts = (
    sdk::EntityKey,
    sdk::CandidateStatus,
    Option<u64>,
    Option<CanonicalError>,
);

fn item_parts(item: OperationItemRecord, operation_id: Uuid) -> Result<ItemParts, CanonicalError> {
    let canonical = item.key.to_string();
    let error = item
        .error
        .map(|stored| item_failure(&item.key, stored, operation_id).into_canonical(&canonical));
    Ok((
        sdk_key(item.key)?,
        sdk_candidate_status(item.status),
        item.resource_version
            .map(|v| sdk_version(v, &format_args!("{operation_id} {canonical}")))
            .transpose()?,
        error,
    ))
}

/// Reversible SDK item failure; expose only the reason of unreadable records, matching REST.
fn item_failure(
    key: &EntityKey,
    stored: Result<StoredFailure, UnreadableFailure>,
    operation_id: Uuid,
) -> AdmissionFailure {
    match stored {
        Ok(failure) => {
            let context = [
                (
                    sdk::item_failure::context::DEPENDENCY_ID,
                    failure.dependency_id,
                ),
                (
                    sdk::item_failure::context::DEPENDENCY_KIND,
                    failure.dependency_kind,
                ),
                (
                    sdk::item_failure::context::DIAGNOSTIC_CODE,
                    failure.error_code,
                ),
            ];
            context
                .into_iter()
                .filter_map(|(name, value)| Some((name, value?)))
                .fold(
                    AdmissionFailure::new(
                        AdmissionFailureReason::from_wire(&failure.reason),
                        failure.message,
                    ),
                    |acc, (name, value)| acc.with_context(name, value),
                )
        }
        Err(unreadable) => {
            tracing::error!(
                %operation_id,
                entity_key = %key,
                reason = unreadable.reason.as_wire(),
                cause = %unreadable.cause,
                "types_registry cannot read a stored item failure"
            );
            AdmissionFailure::new(unreadable.reason, "the recorded failure could not be read")
        }
    }
}

#[cfg(test)]
#[path = "local_client_tests.rs"]
mod local_client_tests;
