//! Authorization resources for publishing, subscriptions, and consumer groups.
//!
//! Concrete resources use their own GTS id as the PEP resource type
//! (`docs/DESIGN.md` §3.3). The id is also sent as a resource property.
//! Event and topic decisions carry the target tenant as `owner_tenant_id`;
//! publish decides tenant reach and barrier crossing in its own requests
//! (`IngestServiceImpl::authorize_publish`), and so does JOIN for what it
//! consumes (`domain::join_scope`). JOIN, stream open and seek, and the point
//! and list operations of named groups, ask on the consumer group itself (the
//! group check of `domain::delivery`); the other consume and management paths
//! check tenant authority through `tenant_authorized`, and so do producer
//! registration, cursors and reset. Topic and event-type metadata reads ask
//! [`MetadataAccess`].

use std::future::Future;
use std::sync::{Arc, LazyLock};

use authz_resolver_sdk::{
    AccessRequest, BarrierMode, EnforcerError, PolicyEnforcer, ResourceType, TenantMode,
};
use event_broker_sdk::gts::{CONSUMER_GROUP_RESOURCE_TYPE, EventV1, REQUEST_RESOURCE_TYPE};
use gts::{GtsId, GtsIdPattern, GtsTypeId};
use toolkit_gts::{GtsInstanceId, GtsSchema};
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use tracing::Instrument;
use uuid::Uuid;

use crate::domain::error::{DomainError, ErrorCode};
use crate::domain::join_scope::TOPIC_ID;
use crate::domain::specification::{EventTypeCandidates, SpecificationManager};
use crate::domain::streaming::scope::CONSUME_PROPS;

/// Resource for anonymous consumer groups: `define` to create one, and the
/// group check (`consume`) of JOIN, stream open and seek, which names the group
/// and its tenant. It declares `owner_tenant_id`, the property a tenant-aware
/// PDP names in its allow. Named groups use [`consumer_group_resource`] instead
/// (`docs/DESIGN.md`, Consumer Group Lifecycle).
pub const CONSUMER_GROUP_FAMILY_RESOURCE: ResourceType = ResourceType::from_static(
    CONSUMER_GROUP_RESOURCE_TYPE,
    &[pep_properties::OWNER_TENANT_ID],
);

/// Matches the event base type and its descendants. Invalid SDK metadata is
/// handled by [`event_family`] without panicking.
static EVENT_FAMILY: LazyLock<Option<GtsIdPattern>> =
    LazyLock::new(|| GtsIdPattern::try_new(EventV1::<()>::TYPE_ID).ok());

/// The event family pattern; fails closed when it cannot be built.
fn event_family() -> Result<&'static GtsIdPattern, DomainError> {
    EVENT_FAMILY.as_ref().ok_or_else(|| {
        DomainError::Internal(format!(
            "event base type '{}' is not a valid GTS pattern",
            EventV1::<()>::TYPE_ID
        ))
    })
}

/// PEP resource for publishing or consuming a concrete event type.
/// Rejects types outside the event family before authorization.
///
/// # Errors
/// `DomainError::NotFound` with `EventTypeNotFound` when `event_type` does not
/// derive from `gts.cf.core.events.event.v1~`; `DomainError::Internal` if the
/// event family pattern cannot be built.
pub fn event_type_resource(event_type: &GtsTypeId) -> Result<ResourceType, DomainError> {
    let family = event_family()?;
    let is_event = GtsId::try_new(event_type.as_ref()).is_ok_and(|id| id.matches_pattern(family));
    if !is_event {
        return Err(DomainError::NotFound {
            code: ErrorCode::EventTypeNotFound,
            message: format!("event type '{event_type}' is not registered"),
            resource: event_type.to_string(),
        });
    }
    Ok(ResourceType::new(
        event_type.as_ref().to_owned(),
        &[pep_properties::OWNER_TENANT_ID],
    ))
}

/// Refuses an event-type selector that neither covers nor is covered by the
/// event family, so it cannot match any event type.
///
/// # Errors
///
/// `DomainError::Validation` with `BadTypePattern` when `pattern` cannot match
/// any event type; `DomainError::Internal` if the event family pattern cannot
/// be built.
pub fn check_event_type_selector(pattern: &GtsIdPattern) -> Result<(), DomainError> {
    let family = event_family()?;
    if !(family.covers(pattern) || pattern.covers(family)) {
        return Err(DomainError::Validation {
            code: ErrorCode::BadTypePattern,
            message: format!(
                "event type selector '{}' cannot match any event type ({}…)",
                pattern.pattern(),
                EventV1::<()>::TYPE_ID
            ),
        });
    }
    Ok(())
}

/// PEP resource for one concrete topic with only the tenant property: the
/// barrier right JOIN, stream open and a stream's scope refresh ask for
/// (`consume_ignore_barrier`).
#[must_use]
pub fn topic_resource(topic: &GtsInstanceId) -> ResourceType {
    ResourceType::new(
        topic.as_ref().to_owned(),
        &[pep_properties::OWNER_TENANT_ID],
    )
}

/// PEP resource for the consume decision on one topic, which JOIN, stream
/// open, a stream's scope refresh and delivery's single-type evaluation ask.
/// It declares every property a consume scope may filter on
/// ([`CONSUME_PROPS`]), so the PDP can answer with type and subject-type rules
/// as well as the tenant.
#[must_use]
pub fn topic_consume_resource(topic: &GtsInstanceId) -> ResourceType {
    ResourceType::new(topic.as_ref().to_owned(), &CONSUME_PROPS)
}

/// PEP resource for an existing named consumer group, its own id: the group
/// check of JOIN, stream open and seek, and get, delete and list. It declares
/// `owner_tenant_id`, the property a tenant-aware PDP names in its allow.
/// Anonymous groups use [`CONSUMER_GROUP_FAMILY_RESOURCE`] instead.
#[must_use]
pub fn consumer_group_resource(group: &GtsInstanceId) -> ResourceType {
    ResourceType::new(
        group.as_ref().to_owned(),
        &[pep_properties::OWNER_TENANT_ID],
    )
}
/// Generic resource for the platform-tenant-scope check
/// (`docs/openapi.yaml`'s `TenantIdNotAuthorized`) - deliberately distinct
/// from the topic/event-type resources: this call is about the
/// calling principal's authority over the *target tenant*, not the
/// topic/event type being acted on, so it stays its own `PolicyEnforcer`
/// call (and therefore its own denial code). On publish its denial is not
/// final: the barrier check (`produce_ignore_barrier` on the event type) is
/// asked next, and only both denials answer `TenantIdNotAuthorized`.
pub const TENANT_SCOPE_RESOURCE: ResourceType =
    ResourceType::from_static(REQUEST_RESOURCE_TYPE, &[pep_properties::OWNER_TENANT_ID]);

/// Overwrites a `DomainError::Forbidden`'s `code` with a specific
/// `docs/openapi.yaml` code (`TopicNotAuthorized`/`EventTypeNotAuthorized`/
/// `NotAuthorizedToProduce`/`TenantIdNotAuthorized`) - `From<EnforcerError>`
/// only knows a generic `"AuthzDenied"`, since which specific check failed
/// is known only at the call site (design.md "code-per-call-site"). A no-op
/// for every other `DomainError` variant (a PEP failure only ever produces
/// `Forbidden` or `Internal` per that `From` impl).
#[must_use]
pub fn with_forbidden_code(mut err: DomainError, code: ErrorCode) -> DomainError {
    if let DomainError::Forbidden { code: c, .. } = &mut err {
        *c = code;
    }
    err
}

/// Confirms the calling principal is authorized to act as `target` tenant -
/// a dedicated `PolicyEnforcer` check against `TENANT_SCOPE_RESOURCE`, not a
/// `tenant-resolver-sdk` call. The request names `target` as its tenant
/// context (the root, `root_only` mode, barriers respected) and as
/// `pep_properties::OWNER_TENANT_ID`, the shape publish sends for its event
/// tenant. It asks for a decision only (no constraints). Whether this
/// enforces the tenant hierarchy (a tree-aware PDP allows a caller whose
/// tenant is `target` or one of its ancestors) or always allows is the
/// deployed authz-resolver plugin's concern.
///
/// # Errors
/// Returns `DomainError::Forbidden { code: "TenantIdNotAuthorized", .. }` if
/// the PEP denies, or the mapped `DomainError` for any other `EnforcerError`.
pub async fn tenant_authorized(
    ctx: &SecurityContext,
    policy_enforcer: &PolicyEnforcer,
    action: &'static str,
    target: Uuid,
) -> Result<(), DomainError> {
    policy_enforcer
        .access_scope_with(
            ctx,
            &TENANT_SCOPE_RESOURCE,
            action,
            None,
            &AccessRequest::new()
                .context_tenant_id(target)
                .tenant_mode(TenantMode::RootOnly)
                .barrier_mode(BarrierMode::Respect)
                .resource_property(pep_properties::OWNER_TENANT_ID, target)
                .require_constraints(false),
        )
        .await
        .map_err(|e| with_forbidden_code(e.into(), ErrorCode::TenantIdNotAuthorized))?;
    Ok(())
}

/// The most PDP calls one metadata read has in flight at once: a list
/// authorizes its topics or event types concurrently, each one asking one call
/// at a time. The bound is per request. A topic holds a gear database
/// connection only for its one indexed candidates read, never across a PDP
/// call, so concurrent lists queue on the pool as publish's own event-type
/// reads do.
pub const METADATA_CHECK_CONCURRENCY: usize = 5;

/// Who may read a topic's or an event type's metadata: a caller that may
/// consume the topic, or else one that may publish one of its event types.
/// Producers read specs to validate what they publish; consumers may read
/// them too, for example for code generation. Both questions are asked in the
/// caller's own tenant, since metadata belongs to no tenant.
///
/// A denied answer, or one that does not compile, refuses the item, as JOIN's
/// topic checks refuse it; an answer that fails to evaluate is an outage and
/// fails the read. The tenant-gated lists answer `500` for an answer that does
/// not compile instead: they ask about the request resource, not a topic.
///
/// No nil-identity guard runs here: the nil-tenant producers of bss-pricing
/// and bss-products read metadata at start (#5955, #5956), and a guard would
/// refuse them even under a PDP that allows them. The shipped PDPs refuse a
/// nil tenant on their own.
#[toolkit::domain_model]
#[derive(Clone)]
pub struct MetadataAccess {
    ctx: SecurityContext,
    policy_enforcer: PolicyEnforcer,
    specs: Arc<dyn SpecificationManager>,
}

impl MetadataAccess {
    #[must_use]
    pub fn new(
        ctx: &SecurityContext,
        policy_enforcer: &PolicyEnforcer,
        specs: &Arc<dyn SpecificationManager>,
    ) -> Self {
        Self {
            ctx: ctx.clone(),
            policy_enforcer: policy_enforcer.clone(),
            specs: Arc::clone(specs),
        }
    }

    /// Whether the caller may read `topic`: it may consume it, or else
    /// publish one of its registered event types, asked in id order until one
    /// is allowed. A topic with more event types than
    /// [`EventTypeCandidates::CAP`] is refused without asking about its types.
    ///
    /// # Errors
    ///
    /// `DomainError::Internal` when an evaluation fails or the topic's event
    /// types do not parse; `DomainError::StorageUnavailable` when they cannot
    /// be read.
    pub async fn may_read_topic(&self, topic: &GtsInstanceId) -> Result<bool, DomainError> {
        if self.may_consume(topic).await? {
            return Ok(true);
        }
        let EventTypeCandidates::Listed(mut candidates) =
            self.specs.get_event_type_candidates(topic).await?
        else {
            return Ok(false);
        };
        candidates.sort_by(|a, b| a.id.as_ref().cmp(b.id.as_ref()));
        for candidate in candidates {
            if self.may_produce(&candidate.id).await? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether the caller may consume `topic` in its own tenant.
    ///
    /// # Errors
    ///
    /// `DomainError::Internal` when the evaluation fails.
    pub async fn may_consume(&self, topic: &GtsInstanceId) -> Result<bool, DomainError> {
        let answer = self
            .policy_enforcer
            .access_scope_with(
                &self.ctx,
                &topic_consume_resource(topic),
                "consume",
                None,
                &Self::view_request(self.ctx.subject_tenant_id(), topic),
            )
            .await;
        Self::decided(answer)
    }

    /// Whether the caller may publish `event_type` for its own tenant.
    ///
    /// # Errors
    ///
    /// `DomainError::Internal` when the evaluation fails.
    pub async fn may_produce(&self, event_type: &GtsTypeId) -> Result<bool, DomainError> {
        // A type outside the event family cannot be published: a refusal, not
        // an error.
        let resource = match event_type_resource(event_type) {
            Ok(resource) => resource,
            Err(DomainError::NotFound { .. }) => return Ok(false),
            Err(other) => return Err(other),
        };
        let answer = self
            .policy_enforcer
            .access_scope_with(
                &self.ctx,
                &resource,
                "produce",
                None,
                &Self::produce_request(self.ctx.subject_tenant_id(), event_type),
            )
            .await;
        Self::decided(answer)
    }

    /// Decides each of `items` with `decide`, at most
    /// [`METADATA_CHECK_CONCURRENCY`] at once, and returns the decisions in
    /// the order of `items`. Each decision runs in the caller's span and
    /// under its `x-request-id`, so a remote policy call stays correlated
    /// with the request.
    ///
    /// # Errors
    ///
    /// The first error a decision returns; the decisions still running are
    /// dropped with it. `DomainError::Internal` when a decision panics.
    pub async fn decide_each<T, F, D>(items: &[T], decide: D) -> Result<Vec<bool>, DomainError>
    where
        D: Fn(&T) -> F,
        F: Future<Output = Result<bool, DomainError>> + Send + 'static,
    {
        let mut decisions = vec![false; items.len()];
        let mut running = tokio::task::JoinSet::new();
        // A task-local: a spawned task does not inherit it, so it is read here.
        let request_id = toolkit::api::current_request_id();
        for (index, item) in items.iter().enumerate() {
            if running.len() == METADATA_CHECK_CONCURRENCY {
                Self::record_next(&mut running, &mut decisions).await?;
            }
            let decision = decide(item);
            running.spawn(
                toolkit::api::with_request_id(request_id.clone(), async move {
                    (index, decision.await)
                })
                .in_current_span(),
            );
        }
        while !running.is_empty() {
            Self::record_next(&mut running, &mut decisions).await?;
        }
        Ok(decisions)
    }

    /// Waits for one running decision and writes it at its index.
    async fn record_next(
        running: &mut tokio::task::JoinSet<(usize, Result<bool, DomainError>)>,
        decisions: &mut [bool],
    ) -> Result<(), DomainError> {
        let Some(joined) = running.join_next().await else {
            return Ok(());
        };
        let (index, decision) = joined.map_err(|_| {
            DomainError::Internal("a metadata authorization did not finish".to_owned())
        })?;
        decisions[index] = decision?;
        Ok(())
    }

    /// V: consume on the topic, rooted at the caller's tenant with barriers
    /// respected, naming the topic and the caller's tenant as its owner.
    fn view_request(caller_tenant: Uuid, topic: &GtsInstanceId) -> AccessRequest {
        AccessRequest::new()
            .context_tenant_id(caller_tenant)
            .tenant_mode(TenantMode::RootOnly)
            .barrier_mode(BarrierMode::Respect)
            .resource_property(TOPIC_ID, topic.as_ref())
            .resource_property(pep_properties::OWNER_TENANT_ID, caller_tenant)
            .require_constraints(false)
    }

    /// P: produce on the event type for an event of the caller's own tenant,
    /// the shape of publish's type check.
    fn produce_request(caller_tenant: Uuid, event_type: &GtsTypeId) -> AccessRequest {
        AccessRequest::new()
            .context_tenant_id(caller_tenant)
            .tenant_mode(TenantMode::RootOnly)
            .resource_property("event_type_id", event_type.as_ref())
            .resource_property(pep_properties::OWNER_TENANT_ID, caller_tenant)
            .require_constraints(false)
    }

    /// An allow is `true`; a refusal or an answer that does not compile is
    /// `false`; a failed evaluation is an error.
    fn decided(answer: Result<AccessScope, EnforcerError>) -> Result<bool, DomainError> {
        match answer {
            Ok(_) => Ok(true),
            Err(EnforcerError::AccessDenied(_) | EnforcerError::CompileFailed(_)) => Ok(false),
            // A failed evaluation, or an outcome class this build does not
            // know, which the conversion makes an internal failure.
            Err(error) => Err(error.into()),
        }
    }
}

impl std::fmt::Debug for MetadataAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetadataAccess")
            .field("subject_tenant_id", &self.ctx.subject_tenant_id())
            .finish_non_exhaustive()
    }
}

/// Refuses a caller whose subject or tenant is the nil UUID. An entry point
/// that runs it does so before any read or PDP call. The broker acts on these
/// ids, so a nil one names no principal; a bearer token does not exempt the
/// caller.
///
/// # Errors
///
/// [`DomainError::Forbidden`] with [`ErrorCode::TenantIdNotAuthorized`] and an
/// empty `resource`, as a PDP tenant denial carries, when either id is nil.
pub fn require_caller_identity(ctx: &SecurityContext) -> Result<(), DomainError> {
    if ctx.subject_id().is_nil() || ctx.subject_tenant_id().is_nil() {
        return Err(DomainError::Forbidden {
            code: ErrorCode::TenantIdNotAuthorized,
            message: "the caller has no identity: its subject or tenant is the nil UUID".to_owned(),
            resource: String::new(),
        });
    }
    Ok(())
}

#[cfg(test)]
#[path = "authz_tests.rs"]
mod authz_tests;

#[cfg(test)]
#[path = "authz_pdp_tests.rs"]
mod authz_pdp_tests;

#[cfg(test)]
#[path = "pdp_fixtures.rs"]
mod pdp_fixtures;

#[cfg(test)]
#[path = "consume_pdp_tests.rs"]
mod consume_pdp_tests;
