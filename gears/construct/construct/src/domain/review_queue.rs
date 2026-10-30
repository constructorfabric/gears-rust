//! The Review Queue: the subject flags one of its facts as incorrect, which
//! opens a review request, and a reviewer of the tenant resolves it as
//! corrected, deleted or rejected. While a request is open its fact is not
//! served to applications and agents, and no record or agent changes it.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use serde_json::Value;
use time::OffsetDateTime;
use tokio::time::Instant;
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::{AccessScope, ScopeFilter, ScopeValue, SecurityContext, pep_properties};
use uuid::Uuid;

use super::admission::{Admission, AdmissionOutcome};
use super::error::DomainError;
use super::plan::{Change, NodeKey, Origin, Plan, Property, Step};
use super::planner::catalog::PropertyCatalog;
use super::planner::processing::Written;
use super::profile::Category;
use super::record_intake::DropReason;
use super::sensitive_data::{Checked, SensitiveDataChecks};
use super::subject_control::{Attempt, Removed, SubjectControlService, actions as fact_actions};
use super::subject_settings::SubjectKey;

/// GTS type id of a review request: the authorization resource type of the
/// reviewers' routes, and the resource type their errors name.
pub(crate) const REVIEW_REQUEST_TYPE: &str =
    toolkit_gts::gts_id!("cf.construct.review.request.v1~");

/// Authorization resource type for review requests, scoped by tenant: a
/// reviewer resolves the requests of a whole tenant.
pub(crate) const REVIEW_REQUEST_RESOURCE: ResourceType =
    ResourceType::from_static(REVIEW_REQUEST_TYPE, &[pep_properties::OWNER_TENANT_ID]);

pub(crate) mod actions {
    /// The subject flags one of its own facts; on the subject facts resource.
    pub const FLAG: &str = "flag";
    /// A reviewer lists and resolves the tenant's review requests.
    pub const RESOLVE: &str = "resolve";
}

/// The longest comment a subject gives with a flag, and the longest reason a
/// reviewer gives with a resolution, in characters.
pub const MAX_TEXT_CHARS: usize = 1000;

/// Whether a request still waits for a reviewer.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewState {
    Open,
    Closed,
}

impl ReviewState {
    /// The state's name on the wire and in the table.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Open, Self::Closed]
            .into_iter()
            .find(|state| state.name() == name)
    }
}

/// How a request was closed.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewOutcome {
    /// A reviewer stored a new value in place of the fact.
    Corrected,
    /// The fact was deleted: by a reviewer, by the subject, or before a
    /// correction could be stored.
    Deleted,
    /// A reviewer kept the fact as it is.
    Rejected,
}

impl ReviewOutcome {
    /// The outcome's name on the wire, in the table and in events.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Corrected => "corrected",
            Self::Deleted => "deleted",
            Self::Rejected => "rejected",
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Corrected, Self::Deleted, Self::Rejected]
            .into_iter()
            .find(|outcome| outcome.name() == name)
    }
}

/// One review request, open or closed. `value` is the flagged value, kept
/// only while the request is open.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewRequest {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub subject_id: Uuid,
    pub fact_id: NodeKey,
    pub category: Category,
    pub property: String,
    pub value: Option<Value>,
    pub comment: Option<String>,
    pub state: ReviewState,
    pub outcome: Option<ReviewOutcome>,
    pub reason: Option<String>,
    /// Who closed it: the reviewer, or the subject that deleted the fact.
    pub resolved_by: Option<Uuid>,
    /// The id of the fact a correction stored.
    pub corrected_fact_id: Option<NodeKey>,
    pub created_at: OffsetDateTime,
    pub resolved_at: Option<OffsetDateTime>,
}

/// A request to open: the fact as the subject flagged it.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct NewReviewRequest {
    pub id: Uuid,
    pub key: SubjectKey,
    pub fact_id: NodeKey,
    pub category: Category,
    pub property: String,
    pub value: Value,
    pub comment: Option<String>,
    pub created_at: OffsetDateTime,
}

/// What a close sets on an open request.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Closing {
    pub outcome: ReviewOutcome,
    pub by: Uuid,
    pub reason: Option<String>,
    pub corrected_fact_id: Option<NodeKey>,
    pub at: OffsetDateTime,
}

/// What opening a request did.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum Opened {
    Opened(Box<ReviewRequest>),
    /// The fact already has an open request; nothing was stored.
    AlreadyOpen,
}

/// The fields the subject's list of its requests filters and orders by.
/// Exists only to derive [`SubjectReviewField`].
#[domain_model]
#[derive(toolkit_odata_macros::ODataFilterable)]
pub struct SubjectReviewQuery {
    #[odata(filter(kind = "Uuid"))]
    pub id: Uuid,
    #[odata(filter(kind = "String"))]
    pub fact_id: String,
    /// `open` or `closed`.
    #[odata(filter(kind = "String"))]
    pub state: String,
    /// `corrected`, `deleted` or `rejected`.
    #[odata(filter(kind = "String"))]
    pub outcome: String,
    #[odata(filter(kind = "DateTimeUtc"))]
    pub created_at: OffsetDateTime,
}

pub use SubjectReviewQueryFilterField as SubjectReviewField;

/// The fields the reviewers' list of open requests filters and orders by.
/// Exists only to derive [`OpenReviewField`].
#[domain_model]
#[derive(toolkit_odata_macros::ODataFilterable)]
pub struct OpenReviewQuery {
    #[odata(filter(kind = "Uuid"))]
    pub id: Uuid,
    #[odata(filter(kind = "Uuid"))]
    pub subject_id: Uuid,
    #[odata(filter(kind = "String"))]
    pub fact_id: String,
    /// `identity`, `role`, `skill` or `preference`.
    #[odata(filter(kind = "String"))]
    pub entity_kind: String,
    #[odata(filter(kind = "DateTimeUtc"))]
    pub created_at: OffsetDateTime,
}

pub use OpenReviewQueryFilterField as OpenReviewField;

/// Construct's review requests. The calls about one subject run as Construct
/// in the subject's tenant, after the caller was authorized; the reviewers'
/// calls run under the reviewer's scope.
#[async_trait]
pub trait ReviewRequests: Send + Sync {
    /// Store a new open request, unless the fact has one.
    async fn open(&self, request: NewReviewRequest) -> Result<Opened, DomainError>;

    /// The request `id` of `tenant_id`, under `scope`.
    async fn find(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        id: Uuid,
    ) -> Result<Option<ReviewRequest>, DomainError>;

    /// Close the request `id` while it is open; none when it is not.
    async fn close(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        id: Uuid,
        closing: &Closing,
    ) -> Result<Option<ReviewRequest>, DomainError>;

    /// Close the open request on the subject's fact, if there is one.
    async fn close_open_on(
        &self,
        key: SubjectKey,
        fact_id: &NodeKey,
        closing: &Closing,
    ) -> Result<Option<ReviewRequest>, DomainError>;

    /// The subject's facts that have an open request: the facts under
    /// review, which the profile read hides and processing leaves alone.
    async fn under_review(&self, key: SubjectKey) -> Result<BTreeSet<NodeKey>, DomainError>;

    /// All the subject's requests, newest first.
    async fn of_subject(&self, key: SubjectKey) -> Result<Vec<ReviewRequest>, DomainError>;

    /// The page of the subject's requests that `query` asks for.
    async fn subject_page(
        &self,
        key: SubjectKey,
        query: &ODataQuery,
    ) -> Result<Page<ReviewRequest>, DomainError>;

    /// The page of the tenant's open requests that `query` asks for, under
    /// `scope`.
    async fn open_page(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ReviewRequest>, DomainError>;
}

/// What the Review Queue did, for the audit log. No event holds a value, a
/// comment or a reason.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewQueueEvent {
    /// The subject flagged a fact.
    Requested {
        tenant_id: Uuid,
        subject_id: Uuid,
        request_id: Uuid,
        fact_id: NodeKey,
        category: Category,
    },
    /// A request was closed, by a reviewer or by the subject's delete.
    Resolved {
        tenant_id: Uuid,
        subject_id: Uuid,
        request_id: Uuid,
        fact_id: NodeKey,
        outcome: ReviewOutcome,
        by: Uuid,
        corrected_fact_id: Option<NodeKey>,
    },
}

impl ReviewQueueEvent {
    /// The resolved event of a request just closed.
    #[must_use]
    pub fn resolved(request: &ReviewRequest) -> Option<Self> {
        Some(Self::Resolved {
            tenant_id: request.tenant_id,
            subject_id: request.subject_id,
            request_id: request.id,
            fact_id: request.fact_id.clone(),
            outcome: request.outcome?,
            by: request.resolved_by?,
            corrected_fact_id: request.corrected_fact_id.clone(),
        })
    }
}

/// Where the Review Queue's events go: the structured log until the Audit
/// gear ships.
pub trait ReviewQueueEvents: Send + Sync {
    fn record(&self, event: ReviewQueueEvent);
}

/// A reviewer's decision on an open request.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    /// Keep the fact as it is.
    Rejected { reason: String },
    /// Delete the fact.
    Deleted { reason: Option<String> },
    /// Store `value` for `property` in place of the fact.
    Corrected {
        property: String,
        value: Value,
        reason: Option<String>,
    },
}

/// Why a correction stored nothing. The request stays open.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotStored {
    /// The checks failed, or Admission dropped the plan.
    Dropped(DropReason),
    /// The checks blocked the value.
    Blocked,
}

impl NotStored {
    /// The reason's name in the answer.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Dropped(reason) => reason.name(),
            Self::Blocked => "value_blocked",
        }
    }
}

/// What a correction did.
#[domain_model]
enum Corrected {
    Stored(NodeKey),
    /// The fact is gone; nothing was stored.
    Gone,
}

/// The stages a corrected value passes before it is written: the same checks
/// and Admission as any plan, and the person types' schemas. Built only where
/// the deployment opted in to storing values without sensitive-data checks,
/// as record processing is.
#[domain_model]
pub struct Corrections {
    pub checks: SensitiveDataChecks,
    pub admission: Admission,
    pub catalog: Arc<PropertyCatalog>,
}

/// @cpt-dod:cpt-cf-construct-dod-review-queue-subject:p1
#[domain_model]
pub struct ReviewQueueService {
    control: Arc<SubjectControlService>,
    requests: Arc<dyn ReviewRequests>,
    events: Arc<dyn ReviewQueueEvents>,
    corrections: Option<Corrections>,
    policy_enforcer: PolicyEnforcer,
}

impl ReviewQueueService {
    #[must_use]
    pub fn new(
        control: Arc<SubjectControlService>,
        requests: Arc<dyn ReviewRequests>,
        events: Arc<dyn ReviewQueueEvents>,
        corrections: Option<Corrections>,
        policy_enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            control,
            requests,
            events,
            corrections,
            policy_enforcer,
        }
    }

    /// Open a review request on one of the subject's live facts.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when the caller is not the subject or the
    /// platform refuses; [`DomainError::NoSuchFact`] for a fact the subject
    /// does not have; [`DomainError::ReviewConflict`] when the fact already
    /// has an open request; not configured without a processing identity.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %subject_id))]
    pub async fn flag(
        &self,
        ctx: &SecurityContext,
        subject_id: Uuid,
        fact_id: &NodeKey,
        comment: Option<String>,
    ) -> Result<ReviewRequest, DomainError> {
        // @cpt-begin:cpt-cf-construct-flow-review-queue-flag:p1:inst-flag-read
        let key = self
            .control
            .own(ctx, subject_id, fact_actions::FLAG)
            .await?;
        let fact = self
            .control
            .facts(key)
            .await?
            .facts
            .into_iter()
            .find(|fact| &fact.node_key == fact_id)
            .ok_or_else(|| {
                DomainError::NoSuchFact(format!("the subject has no live fact {}", fact_id.0))
            })?;
        // @cpt-end:cpt-cf-construct-flow-review-queue-flag:p1:inst-flag-read
        // @cpt-begin:cpt-cf-construct-flow-review-queue-flag:p1:inst-flag-store
        let opened = self
            .requests
            .open(NewReviewRequest {
                id: Uuid::new_v4(),
                key,
                fact_id: fact.node_key,
                category: fact.category,
                property: fact.property,
                value: fact.value,
                comment,
                created_at: OffsetDateTime::now_utc(),
            })
            .await?;
        let Opened::Opened(request) = opened else {
            return Err(DomainError::ReviewConflict(format!(
                "fact {} already has an open review request",
                fact_id.0
            )));
        };
        // @cpt-end:cpt-cf-construct-flow-review-queue-flag:p1:inst-flag-store
        // @cpt-begin:cpt-cf-construct-flow-review-queue-flag:p1:inst-flag-return
        self.events.record(ReviewQueueEvent::Requested {
            tenant_id: request.tenant_id,
            subject_id,
            request_id: request.id,
            fact_id: request.fact_id.clone(),
            category: request.category,
        });
        Ok(*request)
        // @cpt-end:cpt-cf-construct-flow-review-queue-flag:p1:inst-flag-return
    }

    /// The page of the subject's own requests that `query` asks for.
    ///
    /// # Errors
    ///
    /// As [`Self::flag`] for the caller; [`DomainError::InvalidQuery`] for a
    /// filter, order or cursor the list does not take.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %subject_id))]
    pub async fn own_requests(
        &self,
        ctx: &SecurityContext,
        subject_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ReviewRequest>, DomainError> {
        // @cpt-begin:cpt-cf-construct-flow-review-queue-list-own:p1:inst-own-authorize
        let key = self
            .control
            .own(ctx, subject_id, fact_actions::READ)
            .await?;
        // @cpt-end:cpt-cf-construct-flow-review-queue-list-own:p1:inst-own-authorize
        // @cpt-begin:cpt-cf-construct-flow-review-queue-list-own:p1:inst-own-read
        self.requests.subject_page(key, query).await
        // @cpt-end:cpt-cf-construct-flow-review-queue-list-own:p1:inst-own-read
    }

    /// The page of the caller's tenant's open requests that `query` asks
    /// for, oldest first by default.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] without the permission to resolve review
    /// requests; [`DomainError::InvalidQuery`] for a query the list does not
    /// take.
    ///
    /// @cpt-dod:cpt-cf-construct-dod-review-queue-reviewer:p1
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), caller = %ctx.subject_id()))]
    pub async fn open_requests(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ReviewRequest>, DomainError> {
        // @cpt-begin:cpt-cf-construct-flow-review-queue-list-open:p1:inst-open-authorize
        let scope = self.reviewer(ctx).await?;
        // @cpt-end:cpt-cf-construct-flow-review-queue-list-open:p1:inst-open-authorize
        // @cpt-begin:cpt-cf-construct-flow-review-queue-list-open:p1:inst-open-read
        self.requests
            .open_page(&scope, ctx.subject_tenant_id(), query)
            .await
        // @cpt-end:cpt-cf-construct-flow-review-queue-list-open:p1:inst-open-read
    }

    /// Resolve an open request of the caller's tenant.
    ///
    /// NOT cancel-safe: a drop after a delete or a correction was written
    /// leaves the request open; its next resolution finds the fact gone and
    /// closes it as deleted.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] without the permission;
    /// [`DomainError::NoSuchReviewRequest`] for an unknown request;
    /// [`DomainError::ReviewConflict`] for a closed one;
    /// [`DomainError::InvalidValue`] for a correction of another property or
    /// a value its schema does not take; [`DomainError::NotStored`] when the
    /// checks or Admission refuse a correction; not configured for a
    /// correction without the opt-in, or without a processing identity;
    /// unavailable when the profile kept changing.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %request_id, caller = %ctx.subject_id()))]
    pub async fn resolve(
        &self,
        ctx: &SecurityContext,
        request_id: Uuid,
        resolution: Resolution,
    ) -> Result<ReviewRequest, DomainError> {
        // @cpt-begin:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-read
        let deadline = Instant::now() + self.control.budget();
        let scope = self.reviewer(ctx).await?;
        let tenant_id = ctx.subject_tenant_id();
        let request = self
            .requests
            .find(&scope, tenant_id, request_id)
            .await?
            .ok_or_else(|| {
                DomainError::NoSuchReviewRequest(format!(
                    "tenant {tenant_id} has no review request {request_id}"
                ))
            })?;
        if request.state == ReviewState::Closed {
            return Err(closed(request_id));
        }
        let reviewer = ctx.subject_id();
        // @cpt-end:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-read
        let closing = match resolution {
            // @cpt-begin:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-reject
            Resolution::Rejected { reason } => {
                closing(ReviewOutcome::Rejected, reviewer, Some(reason))
            }
            // @cpt-end:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-reject
            // @cpt-begin:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-delete
            Resolution::Deleted { reason } => {
                self.remove(&request, reviewer, deadline).await?;
                closing(ReviewOutcome::Deleted, reviewer, reason)
            }
            // @cpt-end:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-delete
            // @cpt-begin:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-correct
            Resolution::Corrected {
                property,
                value,
                reason,
            } => match self
                .correct(&request, reviewer, property, value, deadline)
                .await?
            {
                Corrected::Stored(key) => Closing {
                    corrected_fact_id: Some(key),
                    ..closing(ReviewOutcome::Corrected, reviewer, reason)
                },
                Corrected::Gone => closing(ReviewOutcome::Deleted, reviewer, reason),
            },
            // @cpt-end:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-correct
        };
        // @cpt-begin:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-return
        let closed_request = self
            .requests
            .close(&scope, tenant_id, request_id, &closing)
            .await?
            .ok_or_else(|| closed(request_id))?;
        if let Some(event) = ReviewQueueEvent::resolved(&closed_request) {
            self.events.record(event);
        }
        Ok(closed_request)
        // @cpt-end:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-return
    }

    /// The platform's scope for resolving the caller's tenant's review
    /// requests.
    async fn reviewer(&self, ctx: &SecurityContext) -> Result<AccessScope, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-review-queue-reviewer-only:p1:inst-reviewer-permission
        let tenant_id = ctx.subject_tenant_id();
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &REVIEW_REQUEST_RESOURCE,
                actions::RESOLVE,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, tenant_id)
                    .require_constraints(true),
            )
            .await?;
        if !reaches_tenant(&scope, tenant_id) {
            return Err(DomainError::forbidden(format!(
                "the policy scope for resolving review requests does not reach tenant {tenant_id}"
            )));
        }
        Ok(scope)
        // @cpt-end:cpt-cf-construct-algo-review-queue-reviewer-only:p1:inst-reviewer-permission
    }

    /// Delete the fact under review, as the subject's delete does, with the
    /// reviewer as origin. A fact already gone is done.
    async fn remove(
        &self,
        request: &ReviewRequest,
        reviewer: Uuid,
        deadline: Instant,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-review-queue-remove:p1:inst-remove-read
        let key = key_of(request);
        let (construct, _) = self.control.contexts().for_tenant(key.tenant_id).await?;
        // @cpt-end:cpt-cf-construct-algo-review-queue-remove:p1:inst-remove-read
        // @cpt-begin:cpt-cf-construct-algo-review-queue-remove:p1:inst-remove-write
        match self
            .control
            .remove(
                &construct,
                key,
                &request.fact_id,
                &Origin::Reviewer { reviewer },
                deadline,
            )
            .await?
        {
            Removed::Removed(_) | Removed::Gone => Ok(()),
        }
        // @cpt-end:cpt-cf-construct-algo-review-queue-remove:p1:inst-remove-write
    }

    /// Store `value` in place of the fact under review: one replace step with
    /// a new node key and the reviewer as origin, through the checks,
    /// Admission and the writer, against the profile's version.
    ///
    /// @cpt-dod:cpt-cf-construct-dod-review-queue-one-step:p1
    async fn correct(
        &self,
        request: &ReviewRequest,
        reviewer: Uuid,
        property: String,
        value: Value,
        deadline: Instant,
    ) -> Result<Corrected, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-start
        let Some(corrections) = &self.corrections else {
            return Err(DomainError::NotConfigured(
                "a correction needs processing.run_without_sensitive_data_checks while no \
                 sensitive-data check exists"
                    .to_owned(),
            ));
        };
        if property != request.property {
            return Err(DomainError::InvalidValue(format!(
                "a correction sets the fact's own property, `{}`",
                request.property
            )));
        }
        let spec = corrections.catalog.get(&property).ok_or_else(|| {
            DomainError::internal(format!(
                "stored fact {} has property `{property}`, which no person type has",
                request.fact_id.0
            ))
        })?;
        spec.check(&value).map_err(DomainError::InvalidValue)?;
        let key = key_of(request);
        let (construct, scope) = self.control.contexts().for_tenant(key.tenant_id).await?;
        let (construct, scope, value) = (&construct, &scope, &value);
        let profiles = &self.control.ports().profiles;
        let writer = &self.control.ports().writer;
        // @cpt-end:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-start
        let corrected = self
            .control
            .with_reruns(deadline, "correction", move || async move {
                // @cpt-begin:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-read
                let current = profiles.facts(construct, key).await?;
                let Some(fact) = current
                    .facts
                    .into_iter()
                    .find(|fact| fact.node_key == request.fact_id)
                else {
                    return Ok(Attempt::Done(Ok(Corrected::Gone)));
                };
                // @cpt-end:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-read
                // @cpt-begin:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-plan
                let new_key = NodeKey::new_in(key.tenant_id);
                let plan = Plan {
                    tenant_id: key.tenant_id,
                    subject_id: key.subject_id,
                    origin: Origin::Reviewer { reviewer },
                    profile_version: current.version,
                    steps: vec![Step {
                        entity_kind: fact.category.name().to_owned(),
                        change: Change::ReplaceEntity {
                            old: fact.node_key,
                            key: new_key.clone(),
                            properties: vec![Property::new(fact.property, value.clone())],
                        },
                        confidence: None,
                    }],
                };
                // @cpt-end:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-plan
                // @cpt-begin:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-admit
                let Checked::Verdicts(verdicts) = corrections.checks.run(&plan).await else {
                    return Ok(Attempt::Done(Err(NotStored::Dropped(
                        DropReason::CheckFailed,
                    ))));
                };
                let admitted = match corrections
                    .admission
                    .admit(construct, scope, plan, &verdicts)
                    .await?
                {
                    AdmissionOutcome::Admitted(admitted) => admitted.plan,
                    AdmissionOutcome::Dropped(reason) => {
                        return Ok(Attempt::Done(Err(NotStored::Dropped(reason))));
                    }
                };
                if admitted.steps.is_empty() {
                    return Ok(Attempt::Done(Err(NotStored::Blocked)));
                }
                // @cpt-end:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-admit
                // @cpt-begin:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-write
                Ok(match writer.write(construct, &admitted).await? {
                    Written::Stored => Attempt::Done(Ok(Corrected::Stored(new_key))),
                    Written::Conflict => Attempt::Conflict,
                })
                // @cpt-end:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-write
            })
            .await?;
        corrected.map_err(DomainError::NotStored)
    }
}

/// Whether some constraint of `scope` grants the whole tenant: all its
/// filters name `tenant_id` as the owner tenant. A reviewer resolves a whole
/// tenant's requests, so a constraint that names anything narrower, or a
/// group or subtree that cannot be decided here, does not make one.
fn reaches_tenant(scope: &AccessScope, tenant_id: Uuid) -> bool {
    let tenant = ScopeValue::Uuid(tenant_id);
    scope.constraints().iter().any(|constraint| {
        constraint.filters().iter().all(|filter| {
            matches!(filter, ScopeFilter::Eq(_) | ScopeFilter::In(_))
                && filter.property() == pep_properties::OWNER_TENANT_ID
                && filter.values().contains(&tenant)
        })
    })
}

fn key_of(request: &ReviewRequest) -> SubjectKey {
    SubjectKey {
        tenant_id: request.tenant_id,
        subject_id: request.subject_id,
    }
}

fn closing(outcome: ReviewOutcome, by: Uuid, reason: Option<String>) -> Closing {
    Closing {
        outcome,
        by,
        reason,
        corrected_fact_id: None,
        at: OffsetDateTime::now_utc(),
    }
}

fn closed(request_id: Uuid) -> DomainError {
    DomainError::ReviewConflict(format!("review request {request_id} is closed"))
}
