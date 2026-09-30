//! Bulk Quota CRUD (`features/bulk-quota-crud.md`): one tenant's creates,
//! updates, or deactivations, applied all or nothing under one envelope key.
//!
//! Every envelope runs in one order:
//!
//! 1. more than [`BULK_MAX_ITEMS_CEILING`] items is `BULK_TOO_LARGE`, before
//!    any PDP call, so the authorization work one envelope causes is bounded;
//! 2. the envelope itself: items present, an envelope key;
//! 3. the PDP, as the single-item operations ask it — one tenant-level check
//!    for a create, one resource-level check per item for an update or a
//!    deactivation;
//! 4. the replay: a stored outcome is returned only once every target is
//!    still visible under its item's current scope;
//! 5. the configured limit, after the replay, so a committed envelope stays
//!    replayable after an operator lowers it;
//! 6. the item checks, in submission order, stopping at the first failure:
//!    duplicates, the item keys, the envelope tenant, then the single-item
//!    validation chain;
//! 7. storage, one transaction for the envelope.
//!
//! The envelope's idempotency scope is `(tenant, no subjects, operation, key)`:
//! management envelopes name their targets explicitly, so there is no
//! resolved subject set to fingerprint, and a key is unique per tenant and
//! bulk operation. The payload digest covers the tenant and every item.

use std::collections::HashSet;

use quota_enforcement_sdk::{
    BulkCreateEntry, BulkCreateEnvelope, BulkCreated, BulkDeactivateEntry, BulkDeactivateEnvelope,
    BulkDeactivated, BulkRecord, BulkUpdateEntry, BulkUpdateEnvelope, BulkUpdated, EnforcementMode,
    IdempotencyScope, IdempotencySubjectKey, IdempotencyWrite, OperationType, PageRequest,
    PayloadHash, PeriodType, QuotaFilter, QuotaId, QuotaSource, QuotaType, SubjectRef, TenantId,
    ValidityWindow,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext};

use super::request::{CreateQuotaRequest, Presence, UpdateQuotaRequest};
use super::service::{PreparedCreate, PreparedUpdate, QuotaManagement};
use super::validation::{validate_create_shape, validate_update_shape};
use crate::domain::admission::AdmissionTarget;
use crate::domain::error::{DomainError, ResourceKind};
use crate::domain::pep::{actions, resources};
use crate::domain::ports::metrics::DenialReason;
use crate::domain::tokens;

/// The most items any bulk envelope may carry, whatever the operator
/// configures.
pub const BULK_MAX_ITEMS_CEILING: usize = 500;

/// A bulk create before validation.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkCreateRequest {
    /// The one tenant of the envelope.
    pub tenant_id: TenantId,
    /// The envelope idempotency key.
    pub idempotency_key: String,
    /// The drafts, in submission order.
    pub items: Vec<BulkCreateItem>,
}

/// One draft of a bulk create.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkCreateItem {
    /// Identifies the item in outcomes and errors.
    pub idempotency_key: Option<String>,
    /// The create request, or why the transport could not narrow the item
    /// to one. The error is the item's, raised in submission order among
    /// the item checks, never ahead of the envelope's own checks.
    pub request: Result<CreateQuotaRequest, DomainError>,
}

/// A bulk update before validation.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkUpdateRequest {
    /// The one tenant of the envelope.
    pub tenant_id: TenantId,
    /// The envelope idempotency key.
    pub idempotency_key: String,
    /// The patches, in submission order.
    pub items: Vec<BulkUpdateItem>,
}

/// One patch of a bulk update.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkUpdateItem {
    /// Identifies the item in outcomes and errors.
    pub idempotency_key: Option<String>,
    /// The Quota to patch.
    pub quota_id: QuotaId,
    /// The update request, or why the transport could not narrow the item
    /// to one; raised as the create item's is.
    pub request: Result<UpdateQuotaRequest, DomainError>,
}

/// A bulk deactivate before validation.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkDeactivateRequest {
    /// The one tenant of the envelope.
    pub tenant_id: TenantId,
    /// The envelope idempotency key.
    pub idempotency_key: String,
    /// The Quotas, in submission order.
    pub items: Vec<BulkDeactivateItem>,
}

/// One Quota of a bulk deactivate.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkDeactivateItem {
    /// Identifies the item in outcomes and errors.
    pub idempotency_key: Option<String>,
    /// The Quota to deactivate.
    pub quota_id: QuotaId,
}

// --- payload digests: everything but the envelope key ------------------------

#[derive(Serialize)]
#[serde(tag = "presence", content = "value", rename_all = "snake_case")]
enum PresenceDigest<'a, T> {
    Absent,
    Null,
    Value(&'a T),
}

impl<'a, T> From<&'a Presence<T>> for PresenceDigest<'a, T> {
    fn from(presence: &'a Presence<T>) -> Self {
        match presence {
            Presence::Absent => Self::Absent,
            Presence::Null => Self::Null,
            Presence::Value(value) => Self::Value(value),
        }
    }
}

#[derive(Serialize)]
struct CreateDigest<'a> {
    tenant_id: TenantId,
    subject: &'a SubjectRef,
    metric: &'a str,
    quota_type: QuotaType,
    period: PresenceDigest<'a, PeriodType>,
    enforcement_mode: EnforcementMode,
    cap: Option<i64>,
    notification_thresholds: &'a [u8],
    validity_window: Option<&'a ValidityWindow>,
    fail_open_hint: bool,
    metadata: Option<&'a Map<String, Value>>,
    source: QuotaSource,
}

impl<'a> From<&'a CreateQuotaRequest> for CreateDigest<'a> {
    fn from(request: &'a CreateQuotaRequest) -> Self {
        Self {
            tenant_id: request.tenant_id,
            subject: &request.subject,
            metric: &request.metric,
            quota_type: request.quota_type,
            period: (&request.period).into(),
            enforcement_mode: request.enforcement_mode,
            cap: request.cap,
            notification_thresholds: &request.notification_thresholds,
            validity_window: request.validity_window.as_ref(),
            fail_open_hint: request.fail_open_hint,
            metadata: request.metadata.as_ref(),
            source: request.source,
        }
    }
}

#[derive(Serialize)]
struct UpdateDigest<'a> {
    metric: PresenceDigest<'a, Value>,
    quota_type: PresenceDigest<'a, Value>,
    period: PresenceDigest<'a, Value>,
    subject: PresenceDigest<'a, Value>,
    cap: PresenceDigest<'a, i64>,
    notification_thresholds: Option<&'a [u8]>,
    validity_window: PresenceDigest<'a, ValidityWindow>,
    metadata: Option<&'a Map<String, Value>>,
    enforcement_mode: Option<EnforcementMode>,
    fail_open_hint: Option<bool>,
}

impl<'a> From<&'a UpdateQuotaRequest> for UpdateDigest<'a> {
    fn from(request: &'a UpdateQuotaRequest) -> Self {
        Self {
            metric: (&request.metric).into(),
            quota_type: (&request.quota_type).into(),
            period: (&request.period).into(),
            subject: (&request.subject).into(),
            cap: (&request.cap).into(),
            notification_thresholds: request.notification_thresholds.as_deref(),
            validity_window: (&request.validity_window).into(),
            metadata: request.metadata.as_ref(),
            enforcement_mode: request.enforcement_mode,
            fail_open_hint: request.fail_open_hint,
        }
    }
}

/// What an item's request digests to: the request, or the error its
/// conversion failed with. An envelope carrying an error never commits, so it
/// can never replay; the digest only has to tell it apart.
#[derive(Serialize)]
enum RequestDigest<T> {
    Request(T),
    Unconverted(String),
}

impl<'a, R: 'a, T: From<&'a R>> From<&'a Result<R, DomainError>> for RequestDigest<T> {
    fn from(request: &'a Result<R, DomainError>) -> Self {
        match request {
            Ok(request) => Self::Request(T::from(request)),
            Err(error) => Self::Unconverted(error.to_string()),
        }
    }
}

#[derive(Serialize)]
struct ItemDigest<'a, T> {
    idempotency_key: Option<&'a str>,
    quota_id: Option<QuotaId>,
    request: Option<RequestDigest<T>>,
}

#[derive(Serialize)]
struct EnvelopeDigest<'a, T> {
    tenant_id: TenantId,
    items: Vec<ItemDigest<'a, T>>,
}

fn digest<T: Serialize>(payload: &T) -> Result<PayloadHash, DomainError> {
    PayloadHash::of_canonical(payload).map_err(|error| DomainError::Internal(error.to_string()))
}

/// The envelope's idempotency write under the management-envelope scope.
fn envelope_write(
    tenant_id: TenantId,
    operation_type: OperationType,
    key: &str,
    payload_hash: PayloadHash,
) -> IdempotencyWrite {
    IdempotencyWrite {
        scope: IdempotencyScope {
            tenant_id,
            subject_key: IdempotencySubjectKey::of(&[]),
            operation_type,
            key: key.to_owned(),
        },
        payload_hash,
    }
}

fn not_found(quota_id: QuotaId) -> DomainError {
    DomainError::NotFound {
        kind: ResourceKind::Quota,
        id: quota_id.to_string(),
    }
}

/// Step 6's envelope-level checks of one item — its key and its target
/// against every earlier item — applied item by item in submission order, so
/// the first failing item is the one reported.
#[derive(Default)]
struct SeenItems {
    keys: HashSet<String>,
    targets: HashSet<QuotaId>,
}

impl SeenItems {
    /// A blank item key, an item key already used, or a Quota already named.
    fn check(&mut self, key: Option<&str>, target: Option<QuotaId>) -> Result<(), DomainError> {
        let refuse = |field: &'static str, reason: &'static str| {
            Err(DomainError::InvalidArgument { field, reason })
        };
        if let Some(key) = key {
            if key.trim().is_empty() {
                return refuse("idempotency_key", tokens::IDEMPOTENCY_KEY_REQUIRED);
            }
            if !self.keys.insert(key.to_owned()) {
                return refuse("idempotency_key", tokens::BATCH_ITEM_KEY_DUPLICATE);
            }
        }
        if let Some(target) = target
            && !self.targets.insert(target)
        {
            return refuse("quota_id", tokens::BULK_QUOTA_DUPLICATE);
        }
        Ok(())
    }
}

impl QuotaManagement<'_> {
    /// Step 6 for one item: its envelope-level checks, then the request its
    /// transport narrowed, or that narrowing's error.
    fn check_item<T>(
        &self,
        seen: &mut SeenItems,
        index: usize,
        key: Option<&str>,
        target: Option<QuotaId>,
        request: Result<T, DomainError>,
    ) -> Result<T, DomainError> {
        seen.check(key, target)
            .and(request)
            .map_err(|error| self.shape_rejected(error).at_item(index))
    }

    /// Steps 1 and 2: the ceiling, then the envelope itself.
    fn check_envelope(&self, items: usize, key: &str) -> Result<(), DomainError> {
        let refuse = |error: DomainError| {
            self.metrics.record_denial(DenialReason::InvalidArgument);
            error
        };
        if items > BULK_MAX_ITEMS_CEILING {
            return Err(refuse(DomainError::BulkTooLarge {
                items,
                max: BULK_MAX_ITEMS_CEILING,
            }));
        }
        if items == 0 {
            return Err(refuse(DomainError::InvalidArgument {
                field: "items",
                reason: tokens::BATCH_EMPTY,
            }));
        }
        if key.trim().is_empty() {
            return Err(refuse(DomainError::InvalidArgument {
                field: "idempotency_key",
                reason: tokens::IDEMPOTENCY_KEY_REQUIRED,
            }));
        }
        Ok(())
    }

    /// Step 5: the operator's limit.
    fn check_configured_limit(&self, items: usize) -> Result<(), DomainError> {
        if items > self.limits.bulk_max_items {
            self.metrics.record_denial(DenialReason::InvalidArgument);
            return Err(DomainError::BulkTooLarge {
                items,
                max: self.limits.bulk_max_items,
            });
        }
        Ok(())
    }

    /// The stored outcome under `write`, or the payload mismatch.
    async fn stored<T: DeserializeOwned>(
        &self,
        write: &IdempotencyWrite,
    ) -> Result<Option<T>, DomainError> {
        let Some(record) = self.storage.lookup_idempotency(&write.scope).await? else {
            return Ok(None);
        };
        if record.payload_hash != write.payload_hash {
            return Err(DomainError::IdempotencyPayloadMismatch);
        }
        serde_json::from_value::<BulkRecord<T>>(record.decision_blob)
            .map(|record| Some(record.outcome))
            .map_err(|error| DomainError::Internal(error.to_string()))
    }

    /// Step 4 for update and deactivate: every target still visible under its
    /// item's current scope, in the envelope tenant.
    async fn check_targets_visible(
        &self,
        ctx: &SecurityContext,
        tenant: TenantId,
        targets: &[(QuotaId, AccessScope)],
    ) -> Result<(), DomainError> {
        for (index, (quota_id, scope)) in targets.iter().enumerate() {
            let visible = self
                .read_one(ctx, scope, *quota_id)
                .await
                .map_err(|error| error.at_item(index))?
                .is_some_and(|quota| quota.tenant_id == tenant);
            if !visible {
                return Err(not_found(*quota_id).at_item(index));
            }
        }
        Ok(())
    }

    /// Admit every update or deactivation item as the single-item operation
    /// does, returning each item's scope.
    async fn admit_items(
        &self,
        ctx: &SecurityContext,
        action: &'static str,
        ids: &[QuotaId],
    ) -> Result<Vec<(QuotaId, AccessScope)>, DomainError> {
        let mut targets = Vec::with_capacity(ids.len());
        for (index, quota_id) in ids.iter().copied().enumerate() {
            let admitted = self
                .admit_resource(ctx, action, quota_id)
                .await
                .map_err(|error| error.at_item(index))?;
            targets.push((quota_id, admitted.access_scope));
        }
        Ok(targets)
    }

    /// Create every draft of one tenant, or none.
    ///
    /// # Errors
    ///
    /// `BULK_TOO_LARGE`, the envelope's own `InvalidArgument`, the PDP
    /// outcome, `IDEMPOTENCY_PAYLOAD_MISMATCH`, or the first failing item's
    /// error wrapped in [`DomainError::BulkItem`].
    // @cpt-flow:cpt-cf-quota-enforcement-flow-bulk-create:p2
    // @cpt-algo:cpt-cf-quota-enforcement-algo-bulk-envelope:p2
    pub async fn bulk_create(
        &self,
        ctx: &SecurityContext,
        request: BulkCreateRequest,
    ) -> Result<BulkCreated, DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-bulk-create:p2:inst-qbc-request
        let tenant = request.tenant_id;
        // @cpt-begin:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-ceiling-if
        // @cpt-begin:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-ceiling
        self.check_envelope(request.items.len(), &request.idempotency_key)?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-ceiling
        // @cpt-end:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-ceiling-if
        // @cpt-begin:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-authz
        let admitted = self
            .admission
            .admit(
                ctx,
                &resources::QUOTA,
                actions::CREATE,
                AdmissionTarget::tenant(tenant),
            )
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-authz
        // @cpt-end:cpt-cf-quota-enforcement-flow-bulk-create:p2:inst-qbc-request
        let payload = EnvelopeDigest {
            tenant_id: tenant,
            items: request
                .items
                .iter()
                .map(|item| ItemDigest {
                    idempotency_key: item.idempotency_key.as_deref(),
                    quota_id: None,
                    request: Some(RequestDigest::<CreateDigest<'_>>::from(&item.request)),
                })
                .collect(),
        };
        let write = envelope_write(
            tenant,
            OperationType::BulkCreateQuotas,
            &request.idempotency_key,
            digest(&payload)?,
        );
        // @cpt-begin:cpt-cf-quota-enforcement-flow-bulk-create:p2:inst-qbc-envelope
        // @cpt-begin:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-idem
        if let Some(stored) = self.stored::<BulkCreated>(&write).await? {
            self.check_created_visible(ctx, &admitted.access_scope, &stored)
                .await?;
            return Ok(stored);
        }
        // @cpt-end:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-idem
        // @cpt-end:cpt-cf-quota-enforcement-flow-bulk-create:p2:inst-qbc-envelope
        // @cpt-begin:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-size-if
        // @cpt-begin:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-size
        self.check_configured_limit(request.items.len())?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-size
        // @cpt-end:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-size-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-bulk-create:p2:inst-qbc-validate
        // @cpt-begin:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-validate
        // @cpt-begin:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-invalid-if
        let mut entries = Vec::with_capacity(request.items.len());
        let mut seen = SeenItems::default();
        for (index, item) in request.items.into_iter().enumerate() {
            // @cpt-begin:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-item-keys
            let quota = self.check_item(
                &mut seen,
                index,
                item.idempotency_key.as_deref(),
                None,
                item.request,
            )?;
            // @cpt-end:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-item-keys
            if quota.tenant_id != tenant {
                self.metrics.record_denial(DenialReason::InvalidArgument);
                // @cpt-begin:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-invalid
                return Err(DomainError::InvalidArgument {
                    field: "tenant_id",
                    reason: tokens::BATCH_TENANT_MIXED,
                }
                .at_item(index));
                // @cpt-end:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-invalid
            }
            let draft = validate_create_shape(quota)
                .map_err(|error| self.shape_rejected(error).at_item(index))?;
            let PreparedCreate { draft, events, .. } = self
                .prepare_create(draft)
                .await
                .map_err(|error| error.at_item(index))?;
            entries.push(BulkCreateEntry {
                idempotency_key: item.idempotency_key,
                scope: admitted.access_scope.clone(),
                draft,
                events,
            });
        }
        // @cpt-end:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-invalid-if
        // @cpt-end:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-validate
        // @cpt-end:cpt-cf-quota-enforcement-flow-bulk-create:p2:inst-qbc-validate
        // @cpt-begin:cpt-cf-quota-enforcement-flow-bulk-create:p2:inst-qbc-apply
        // @cpt-begin:cpt-cf-quota-enforcement-flow-bulk-create:p2:inst-qbc-return
        let outcome = self
            .storage
            .bulk_create_quotas(
                ctx,
                &BulkCreateEnvelope {
                    tenant_id: tenant,
                    idempotency: write,
                    items: entries,
                },
            )
            .await?;
        Ok(outcome.into_inner())
        // @cpt-end:cpt-cf-quota-enforcement-flow-bulk-create:p2:inst-qbc-return
        // @cpt-end:cpt-cf-quota-enforcement-flow-bulk-create:p2:inst-qbc-apply
    }

    /// Step 4 for a create: every Quota the stored outcome names is still
    /// visible under the current create scope. Those rows are the envelope
    /// tenant's, so their visibility is the tenant's membership in the scope.
    async fn check_created_visible(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        stored: &BulkCreated,
    ) -> Result<(), DomainError> {
        let ids: Vec<QuotaId> = stored.items.iter().map(|item| item.quota_id).collect();
        let page = self
            .storage
            .read_quotas(
                ctx,
                scope,
                QuotaFilter {
                    ids: ids.clone(),
                    ..QuotaFilter::default()
                },
                PageRequest::first(u32::try_from(ids.len()).unwrap_or(u32::MAX)),
            )
            .await?;
        let visible: HashSet<QuotaId> = page.items.iter().map(|quota| quota.id).collect();
        match stored
            .items
            .iter()
            .find(|item| !visible.contains(&item.quota_id))
        {
            Some(item) => Err(DomainError::PdpDenied {
                reason: Some(DomainError::SUBJECT_OUT_OF_SCOPE.to_owned()),
            }
            .at_item(item.index)),
            None => Ok(()),
        }
    }

    /// Apply every patch to one tenant's Quotas, or none.
    ///
    /// # Errors
    ///
    /// As [`QuotaManagement::bulk_create`], with the item errors of
    /// [`QuotaManagement::update`].
    // @cpt-flow:cpt-cf-quota-enforcement-flow-bulk-update:p2
    pub async fn bulk_update(
        &self,
        ctx: &SecurityContext,
        request: BulkUpdateRequest,
    ) -> Result<BulkUpdated, DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-bulk-update:p2:inst-qbu-request
        let tenant = request.tenant_id;
        self.check_envelope(request.items.len(), &request.idempotency_key)?;
        let ids: Vec<QuotaId> = request.items.iter().map(|item| item.quota_id).collect();
        let targets = self.admit_items(ctx, actions::UPDATE, &ids).await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-bulk-update:p2:inst-qbu-request
        let payload = EnvelopeDigest {
            tenant_id: tenant,
            items: request
                .items
                .iter()
                .map(|item| ItemDigest {
                    idempotency_key: item.idempotency_key.as_deref(),
                    quota_id: Some(item.quota_id),
                    request: Some(RequestDigest::<UpdateDigest<'_>>::from(&item.request)),
                })
                .collect(),
        };
        let write = envelope_write(
            tenant,
            OperationType::BulkUpdateQuotas,
            &request.idempotency_key,
            digest(&payload)?,
        );
        // @cpt-begin:cpt-cf-quota-enforcement-flow-bulk-update:p2:inst-qbu-envelope
        if let Some(stored) = self.stored::<BulkUpdated>(&write).await? {
            self.check_targets_visible(ctx, tenant, &targets).await?;
            return Ok(stored);
        }
        // @cpt-end:cpt-cf-quota-enforcement-flow-bulk-update:p2:inst-qbu-envelope
        self.check_configured_limit(request.items.len())?;
        // @cpt-begin:cpt-cf-quota-enforcement-flow-bulk-update:p2:inst-qbu-validate
        let mut entries = Vec::with_capacity(request.items.len());
        let mut seen = SeenItems::default();
        for (index, (item, (_, scope))) in request.items.into_iter().zip(targets).enumerate() {
            let update = self.check_item(
                &mut seen,
                index,
                item.idempotency_key.as_deref(),
                Some(item.quota_id),
                item.request,
            )?;
            let patch = validate_update_shape(update)
                .map_err(|error| self.shape_rejected(error).at_item(index))?;
            let PreparedUpdate {
                patch,
                events,
                tenant_id,
                ..
            } = self
                .prepare_update(ctx, &scope, item.quota_id, patch)
                .await
                .map_err(|error| error.at_item(index))?;
            if tenant_id != tenant {
                return Err(not_found(item.quota_id).at_item(index));
            }
            entries.push(BulkUpdateEntry {
                idempotency_key: item.idempotency_key,
                scope,
                quota_id: item.quota_id,
                patch,
                events,
            });
        }
        // @cpt-end:cpt-cf-quota-enforcement-flow-bulk-update:p2:inst-qbu-validate
        // @cpt-begin:cpt-cf-quota-enforcement-flow-bulk-update:p2:inst-qbu-apply
        // @cpt-begin:cpt-cf-quota-enforcement-flow-bulk-update:p2:inst-qbu-guard-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-bulk-update:p2:inst-qbu-guard
        // @cpt-begin:cpt-cf-quota-enforcement-flow-bulk-update:p2:inst-qbu-return
        let outcome = self
            .storage
            .bulk_update_quotas(
                ctx,
                &BulkUpdateEnvelope {
                    tenant_id: tenant,
                    idempotency: write,
                    items: entries,
                },
            )
            .await?;
        Ok(outcome.into_inner())
        // @cpt-end:cpt-cf-quota-enforcement-flow-bulk-update:p2:inst-qbu-return
        // @cpt-end:cpt-cf-quota-enforcement-flow-bulk-update:p2:inst-qbu-guard
        // @cpt-end:cpt-cf-quota-enforcement-flow-bulk-update:p2:inst-qbu-guard-if
        // @cpt-end:cpt-cf-quota-enforcement-flow-bulk-update:p2:inst-qbu-apply
    }

    /// Deactivate every Quota of one tenant, or none, resolving their active
    /// leases with it.
    ///
    /// # Errors
    ///
    /// As [`QuotaManagement::bulk_create`], with the item errors of
    /// [`QuotaManagement::deactivate`].
    // @cpt-flow:cpt-cf-quota-enforcement-flow-bulk-deactivate:p2
    pub async fn bulk_deactivate(
        &self,
        ctx: &SecurityContext,
        request: BulkDeactivateRequest,
    ) -> Result<BulkDeactivated, DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-bulk-deactivate:p2:inst-qbd-request
        let tenant = request.tenant_id;
        self.check_envelope(request.items.len(), &request.idempotency_key)?;
        let ids: Vec<QuotaId> = request.items.iter().map(|item| item.quota_id).collect();
        let targets = self.admit_items(ctx, actions::DEACTIVATE, &ids).await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-bulk-deactivate:p2:inst-qbd-request
        let payload: EnvelopeDigest<'_, ()> = EnvelopeDigest {
            tenant_id: tenant,
            items: request
                .items
                .iter()
                .map(|item| ItemDigest {
                    idempotency_key: item.idempotency_key.as_deref(),
                    quota_id: Some(item.quota_id),
                    request: None,
                })
                .collect(),
        };
        let write = envelope_write(
            tenant,
            OperationType::BulkDeactivateQuotas,
            &request.idempotency_key,
            digest(&payload)?,
        );
        // @cpt-begin:cpt-cf-quota-enforcement-flow-bulk-deactivate:p2:inst-qbd-envelope
        if let Some(stored) = self.stored::<BulkDeactivated>(&write).await? {
            self.check_targets_visible(ctx, tenant, &targets).await?;
            return Ok(stored);
        }
        // @cpt-end:cpt-cf-quota-enforcement-flow-bulk-deactivate:p2:inst-qbd-envelope
        self.check_configured_limit(request.items.len())?;
        let mut entries = Vec::with_capacity(request.items.len());
        let mut seen = SeenItems::default();
        for (index, (item, (_, scope))) in request.items.into_iter().zip(targets).enumerate() {
            self.check_item(
                &mut seen,
                index,
                item.idempotency_key.as_deref(),
                Some(item.quota_id),
                Ok(()),
            )?;
            let (tenant_id, events) = self
                .prepare_deactivate(ctx, &scope, item.quota_id)
                .await
                .map_err(|error| error.at_item(index))?;
            if tenant_id != tenant {
                return Err(not_found(item.quota_id).at_item(index));
            }
            entries.push(BulkDeactivateEntry {
                idempotency_key: item.idempotency_key,
                scope,
                quota_id: item.quota_id,
                events,
            });
        }
        // @cpt-begin:cpt-cf-quota-enforcement-flow-bulk-deactivate:p2:inst-qbd-cascade
        // @cpt-begin:cpt-cf-quota-enforcement-flow-bulk-deactivate:p2:inst-qbd-events
        // @cpt-begin:cpt-cf-quota-enforcement-flow-bulk-deactivate:p2:inst-qbd-atomic
        // @cpt-begin:cpt-cf-quota-enforcement-flow-bulk-deactivate:p2:inst-qbd-return
        let outcome = self
            .storage
            .bulk_deactivate_quotas(
                ctx,
                &BulkDeactivateEnvelope {
                    tenant_id: tenant,
                    idempotency: write,
                    items: entries,
                },
            )
            .await?;
        Ok(outcome.into_inner())
        // @cpt-end:cpt-cf-quota-enforcement-flow-bulk-deactivate:p2:inst-qbd-return
        // @cpt-end:cpt-cf-quota-enforcement-flow-bulk-deactivate:p2:inst-qbd-atomic
        // @cpt-end:cpt-cf-quota-enforcement-flow-bulk-deactivate:p2:inst-qbd-events
        // @cpt-end:cpt-cf-quota-enforcement-flow-bulk-deactivate:p2:inst-qbd-cascade
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "bulk_tests.rs"]
mod bulk_tests;
