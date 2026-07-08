use crate::context::{
    Aborted, AlreadyExists, Cancelled, DataLoss, DeadlineExceeded, FailedPrecondition,
    FieldViolation, Internal, InvalidArgument, NotFound, OutOfRange, PermissionDenied,
    PreconditionViolation, QuotaViolation, ResourceExhausted, ServiceUnavailable, Unauthenticated,
    Unimplemented, Unknown,
};
use crate::error::CanonicalError;

// ---------------------------------------------------------------------------
// Resource markers
// ---------------------------------------------------------------------------

#[doc(hidden)]
pub struct ResourceAbsent;
#[doc(hidden)]
pub struct ResourceOptional;
#[doc(hidden)]
pub struct ResourceMissing;
#[doc(hidden)]
pub struct ResourceSet(String);

// ---------------------------------------------------------------------------
// Context markers
// ---------------------------------------------------------------------------

#[doc(hidden)]
pub struct NoContext;
#[doc(hidden)]
pub struct NeedsFieldViolation;
#[doc(hidden)]
pub struct HasFieldViolations(Vec<FieldViolation>);
#[doc(hidden)]
pub struct NeedsPreconditionViolation;
#[doc(hidden)]
pub struct HasPreconditionViolations(Vec<PreconditionViolation>);
#[doc(hidden)]
pub struct NeedsQuotaViolation;
#[doc(hidden)]
pub struct HasQuotaViolations(Vec<QuotaViolation>);
#[doc(hidden)]
pub struct HasFormatMessage(String);
#[doc(hidden)]
pub struct HasConstraintMessage(String);
#[doc(hidden)]
pub struct NeedsReason;
#[doc(hidden)]
pub struct HasReason(String);

// ---------------------------------------------------------------------------
// Traits gating build()
// ---------------------------------------------------------------------------

#[doc(hidden)]
pub trait ResourceResolved {
    fn resolve(self) -> Option<String>;
}

impl ResourceResolved for ResourceAbsent {
    fn resolve(self) -> Option<String> {
        None
    }
}

impl ResourceResolved for ResourceOptional {
    fn resolve(self) -> Option<String> {
        None
    }
}

impl ResourceResolved for ResourceSet {
    fn resolve(self) -> Option<String> {
        Some(self.0)
    }
}

#[doc(hidden)]
pub struct ContextData {
    pub field_violations: Vec<FieldViolation>,
    pub precondition_violations: Vec<PreconditionViolation>,
    pub quota_violations: Vec<QuotaViolation>,
    pub format_message: Option<String>,
    pub constraint_message: Option<String>,
    pub reason: String,
}

#[doc(hidden)]
pub trait ContextResolved {
    fn into_context_data(self) -> ContextData;
}

impl ContextResolved for NoContext {
    fn into_context_data(self) -> ContextData {
        ContextData {
            field_violations: Vec::new(),
            precondition_violations: Vec::new(),
            quota_violations: Vec::new(),
            format_message: None,
            constraint_message: None,
            reason: String::new(),
        }
    }
}

impl ContextResolved for HasFieldViolations {
    fn into_context_data(self) -> ContextData {
        ContextData {
            field_violations: self.0,
            precondition_violations: Vec::new(),
            quota_violations: Vec::new(),
            format_message: None,
            constraint_message: None,
            reason: String::new(),
        }
    }
}

impl ContextResolved for HasFormatMessage {
    fn into_context_data(self) -> ContextData {
        ContextData {
            field_violations: Vec::new(),
            precondition_violations: Vec::new(),
            quota_violations: Vec::new(),
            format_message: Some(self.0),
            constraint_message: None,
            reason: String::new(),
        }
    }
}

impl ContextResolved for HasConstraintMessage {
    fn into_context_data(self) -> ContextData {
        ContextData {
            field_violations: Vec::new(),
            precondition_violations: Vec::new(),
            quota_violations: Vec::new(),
            format_message: None,
            constraint_message: Some(self.0),
            reason: String::new(),
        }
    }
}

impl ContextResolved for HasPreconditionViolations {
    fn into_context_data(self) -> ContextData {
        ContextData {
            field_violations: Vec::new(),
            precondition_violations: self.0,
            quota_violations: Vec::new(),
            format_message: None,
            constraint_message: None,
            reason: String::new(),
        }
    }
}

impl ContextResolved for HasQuotaViolations {
    fn into_context_data(self) -> ContextData {
        ContextData {
            field_violations: Vec::new(),
            precondition_violations: Vec::new(),
            quota_violations: self.0,
            format_message: None,
            constraint_message: None,
            reason: String::new(),
        }
    }
}

impl ContextResolved for HasReason {
    fn into_context_data(self) -> ContextData {
        ContextData {
            field_violations: Vec::new(),
            precondition_violations: Vec::new(),
            quota_violations: Vec::new(),
            format_message: None,
            constraint_message: None,
            reason: self.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Error variant discriminant
// ---------------------------------------------------------------------------

#[doc(hidden)]
#[derive(Debug, Clone, Copy)]
pub enum ErrorVariant {
    Cancelled,
    Unknown,
    InvalidArgument,
    DeadlineExceeded,
    NotFound,
    AlreadyExists,
    PermissionDenied,
    ResourceExhausted,
    FailedPrecondition,
    Aborted,
    OutOfRange,
    Unimplemented,
    Internal,
    DataLoss,
    Unauthenticated,
}

// ---------------------------------------------------------------------------
// ResourceErrorBuilder
// ---------------------------------------------------------------------------

pub struct ResourceErrorBuilder<Resource, Context> {
    resource_type: Option<&'static str>,
    detail: String,
    variant: ErrorVariant,
    resource: Resource,
    context: Context,
}

// ---------------------------------------------------------------------------
// #[doc(hidden)] constructors — called by the macro
// ---------------------------------------------------------------------------

impl ResourceErrorBuilder<ResourceMissing, NoContext> {
    #[doc(hidden)]
    pub fn __not_found(resource_type: &'static str, detail: impl Into<String>) -> Self {
        ResourceErrorBuilder {
            resource_type: Some(resource_type),
            detail: detail.into(),
            variant: ErrorVariant::NotFound,
            resource: ResourceMissing,
            context: NoContext,
        }
    }

    #[doc(hidden)]
    pub fn __already_exists(resource_type: &'static str, detail: impl Into<String>) -> Self {
        ResourceErrorBuilder {
            resource_type: Some(resource_type),
            detail: detail.into(),
            variant: ErrorVariant::AlreadyExists,
            resource: ResourceMissing,
            context: NoContext,
        }
    }

    #[doc(hidden)]
    pub fn __data_loss(resource_type: &'static str, detail: impl Into<String>) -> Self {
        ResourceErrorBuilder {
            resource_type: Some(resource_type),
            detail: detail.into(),
            variant: ErrorVariant::DataLoss,
            resource: ResourceMissing,
            context: NoContext,
        }
    }
}

impl ResourceErrorBuilder<ResourceOptional, NeedsReason> {
    #[doc(hidden)]
    pub fn __aborted(resource_type: &'static str, detail: impl Into<String>) -> Self {
        ResourceErrorBuilder {
            resource_type: Some(resource_type),
            detail: detail.into(),
            variant: ErrorVariant::Aborted,
            resource: ResourceOptional,
            context: NeedsReason,
        }
    }
}

impl ResourceErrorBuilder<ResourceOptional, NoContext> {
    #[doc(hidden)]
    pub fn __unknown(resource_type: &'static str, detail: impl Into<String>) -> Self {
        ResourceErrorBuilder {
            resource_type: Some(resource_type),
            detail: detail.into(),
            variant: ErrorVariant::Unknown,
            resource: ResourceOptional,
            context: NoContext,
        }
    }

    #[doc(hidden)]
    pub fn __deadline_exceeded(resource_type: &'static str, detail: impl Into<String>) -> Self {
        ResourceErrorBuilder {
            resource_type: Some(resource_type),
            detail: detail.into(),
            variant: ErrorVariant::DeadlineExceeded,
            resource: ResourceOptional,
            context: NoContext,
        }
    }

    #[doc(hidden)]
    pub fn __unimplemented(resource_type: &'static str, detail: impl Into<String>) -> Self {
        ResourceErrorBuilder {
            resource_type: Some(resource_type),
            detail: detail.into(),
            variant: ErrorVariant::Unimplemented,
            resource: ResourceOptional,
            context: NoContext,
        }
    }
}

impl ResourceErrorBuilder<ResourceAbsent, NeedsReason> {
    #[doc(hidden)]
    pub fn __permission_denied(resource_type: &'static str, detail: impl Into<String>) -> Self {
        ResourceErrorBuilder {
            resource_type: Some(resource_type),
            detail: detail.into(),
            variant: ErrorVariant::PermissionDenied,
            resource: ResourceAbsent,
            context: NeedsReason,
        }
    }
}

impl ResourceErrorBuilder<ResourceAbsent, NoContext> {
    #[doc(hidden)]
    pub fn __cancelled(resource_type: &'static str, detail: impl Into<String>) -> Self {
        ResourceErrorBuilder {
            resource_type: Some(resource_type),
            detail: detail.into(),
            variant: ErrorVariant::Cancelled,
            resource: ResourceAbsent,
            context: NoContext,
        }
    }
}

impl ResourceErrorBuilder<ResourceOptional, NeedsFieldViolation> {
    #[doc(hidden)]
    pub fn __invalid_argument(resource_type: &'static str, detail: impl Into<String>) -> Self {
        ResourceErrorBuilder {
            resource_type: Some(resource_type),
            detail: detail.into(),
            variant: ErrorVariant::InvalidArgument,
            resource: ResourceOptional,
            context: NeedsFieldViolation,
        }
    }

    #[doc(hidden)]
    pub fn __out_of_range(resource_type: &'static str, detail: impl Into<String>) -> Self {
        ResourceErrorBuilder {
            resource_type: Some(resource_type),
            detail: detail.into(),
            variant: ErrorVariant::OutOfRange,
            resource: ResourceOptional,
            context: NeedsFieldViolation,
        }
    }
}

impl ResourceErrorBuilder<ResourceOptional, NeedsQuotaViolation> {
    #[doc(hidden)]
    pub fn __resource_exhausted(resource_type: &'static str, detail: impl Into<String>) -> Self {
        ResourceErrorBuilder {
            resource_type: Some(resource_type),
            detail: detail.into(),
            variant: ErrorVariant::ResourceExhausted,
            resource: ResourceOptional,
            context: NeedsQuotaViolation,
        }
    }
}

impl ResourceErrorBuilder<ResourceOptional, NeedsPreconditionViolation> {
    #[doc(hidden)]
    pub fn __failed_precondition(resource_type: &'static str, detail: impl Into<String>) -> Self {
        ResourceErrorBuilder {
            resource_type: Some(resource_type),
            detail: detail.into(),
            variant: ErrorVariant::FailedPrecondition,
            resource: ResourceOptional,
            context: NeedsPreconditionViolation,
        }
    }
}

// ---------------------------------------------------------------------------
// with_resource() — available for ResourceMissing and ResourceOptional
// ---------------------------------------------------------------------------

impl<Context> ResourceErrorBuilder<ResourceMissing, Context> {
    #[must_use]
    pub fn with_resource(
        self,
        resource: impl Into<String>,
    ) -> ResourceErrorBuilder<ResourceSet, Context> {
        ResourceErrorBuilder {
            resource_type: self.resource_type,
            detail: self.detail,
            variant: self.variant,
            resource: ResourceSet(resource.into()),
            context: self.context,
        }
    }
}

impl<Context> ResourceErrorBuilder<ResourceOptional, Context> {
    #[must_use]
    pub fn with_resource(
        self,
        resource: impl Into<String>,
    ) -> ResourceErrorBuilder<ResourceSet, Context> {
        ResourceErrorBuilder {
            resource_type: self.resource_type,
            detail: self.detail,
            variant: self.variant,
            resource: ResourceSet(resource.into()),
            context: self.context,
        }
    }
}

// ---------------------------------------------------------------------------
// with_field_violation() — NeedsFieldViolation → HasFieldViolations, then self
// ---------------------------------------------------------------------------

impl<Resource> ResourceErrorBuilder<Resource, NeedsFieldViolation> {
    #[must_use]
    pub fn with_field_violation(
        self,
        field: impl Into<String>,
        description: impl Into<String>,
        reason: impl Into<String>,
    ) -> ResourceErrorBuilder<Resource, HasFieldViolations> {
        ResourceErrorBuilder {
            resource_type: self.resource_type,
            detail: self.detail,
            variant: self.variant,
            resource: self.resource,
            context: HasFieldViolations(vec![FieldViolation::new(field, description, reason)]),
        }
    }

    #[must_use]
    pub fn with_format(
        self,
        message: impl Into<String>,
    ) -> ResourceErrorBuilder<Resource, HasFormatMessage> {
        let msg = message.into();
        ResourceErrorBuilder {
            resource_type: self.resource_type,
            detail: msg.clone(),
            variant: self.variant,
            resource: self.resource,
            context: HasFormatMessage(msg),
        }
    }

    #[must_use]
    pub fn with_constraint(
        self,
        message: impl Into<String>,
    ) -> ResourceErrorBuilder<Resource, HasConstraintMessage> {
        let msg = message.into();
        ResourceErrorBuilder {
            resource_type: self.resource_type,
            detail: msg.clone(),
            variant: self.variant,
            resource: self.resource,
            context: HasConstraintMessage(msg),
        }
    }
}

impl<Resource> ResourceErrorBuilder<Resource, HasFieldViolations> {
    #[must_use]
    pub fn with_field_violation(
        mut self,
        field: impl Into<String>,
        description: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        self.context
            .0
            .push(FieldViolation::new(field, description, reason));
        self
    }
}

// ---------------------------------------------------------------------------
// with_precondition_violation() — NeedsPreconditionViolation → HasPreconditionViolations
// ---------------------------------------------------------------------------

impl<Resource> ResourceErrorBuilder<Resource, NeedsPreconditionViolation> {
    #[must_use]
    pub fn with_precondition_violation(
        self,
        subject: impl Into<String>,
        description: impl Into<String>,
        type_: impl Into<String>,
    ) -> ResourceErrorBuilder<Resource, HasPreconditionViolations> {
        ResourceErrorBuilder {
            resource_type: self.resource_type,
            detail: self.detail,
            variant: self.variant,
            resource: self.resource,
            context: HasPreconditionViolations(vec![PreconditionViolation::new(
                type_,
                subject,
                description,
            )]),
        }
    }
}

impl<Resource> ResourceErrorBuilder<Resource, HasPreconditionViolations> {
    #[must_use]
    pub fn with_precondition_violation(
        mut self,
        subject: impl Into<String>,
        description: impl Into<String>,
        type_: impl Into<String>,
    ) -> Self {
        self.context
            .0
            .push(PreconditionViolation::new(type_, subject, description));
        self
    }
}

// ---------------------------------------------------------------------------
// with_quota_violation() — NeedsQuotaViolation → HasQuotaViolations
// ---------------------------------------------------------------------------

impl<Resource> ResourceErrorBuilder<Resource, NeedsQuotaViolation> {
    #[must_use]
    pub fn with_quota_violation(
        self,
        subject: impl Into<String>,
        description: impl Into<String>,
    ) -> ResourceErrorBuilder<Resource, HasQuotaViolations> {
        ResourceErrorBuilder {
            resource_type: self.resource_type,
            detail: self.detail,
            variant: self.variant,
            resource: self.resource,
            context: HasQuotaViolations(vec![QuotaViolation::new(subject, description)]),
        }
    }
}

impl<Resource> ResourceErrorBuilder<Resource, HasQuotaViolations> {
    #[must_use]
    pub fn with_quota_violation(
        mut self,
        subject: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        self.context
            .0
            .push(QuotaViolation::new(subject, description));
        self
    }

    /// Attach a retry hint to the most-recently-pushed quota violation.
    /// Use for rate-limit-style violations where the upstream knows when
    /// capacity returns; omit for hard exhaustion (memory, non-replenishing
    /// quota) where there is no useful retry window.
    ///
    /// Silently noop if the violation list is empty — the typestate
    /// already guarantees at least one violation is present on
    /// `HasQuotaViolations`, so this branch is unreachable in normal use.
    #[must_use]
    pub fn with_quota_violation_retry_after_seconds(mut self, seconds: u64) -> Self {
        if let Some(last) = self.context.0.last_mut() {
            last.retry_after_seconds = Some(seconds);
        }
        self
    }
}

// ---------------------------------------------------------------------------
// with_reason() — NeedsReason → HasReason
// ---------------------------------------------------------------------------

impl<Resource> ResourceErrorBuilder<Resource, NeedsReason> {
    #[must_use]
    pub fn with_reason(
        self,
        reason: impl Into<String>,
    ) -> ResourceErrorBuilder<Resource, HasReason> {
        ResourceErrorBuilder {
            resource_type: self.resource_type,
            detail: self.detail,
            variant: self.variant,
            resource: self.resource,
            context: HasReason(reason.into()),
        }
    }
}

// ---------------------------------------------------------------------------
// Public builder-returning constructors on CanonicalError (non-macro categories)
// ---------------------------------------------------------------------------

impl CanonicalError {
    #[must_use]
    pub fn internal(detail: impl Into<String>) -> ResourceErrorBuilder<ResourceAbsent, NoContext> {
        ResourceErrorBuilder {
            resource_type: None,
            detail: detail.into(),
            variant: ErrorVariant::Internal,
            resource: ResourceAbsent,
            context: NoContext,
        }
    }

    #[must_use]
    pub fn service_unavailable() -> ServiceUnavailableBuilder {
        ServiceUnavailableBuilder {
            retry_after_seconds: None,
            detail: None,
        }
    }

    #[must_use]
    pub fn unauthenticated() -> ResourceErrorBuilder<ResourceAbsent, NeedsReason> {
        ResourceErrorBuilder {
            resource_type: None,
            detail: String::from("Authentication required"),
            variant: ErrorVariant::Unauthenticated,
            resource: ResourceAbsent,
            context: NeedsReason,
        }
    }
}

// ---------------------------------------------------------------------------
// Runtime-scope constructors
// ---------------------------------------------------------------------------
//
// These mirror the constructors generated by `#[resource_error]` but accept the
// GTS resource-type (scope) at *runtime* rather than binding it at compile time.
// They let generic, reusable middleware (e.g. `toolkit-http-middleware`) emit
// canonical errors under a scope chosen by the consuming gear, without that
// gear's `#[resource_error]` type being reachable from the shared crate.

impl CanonicalError {
    /// `invalid_argument` builder with a runtime GTS `scope` as the resource type.
    #[must_use]
    pub fn scoped_invalid_argument(
        scope: &'static str,
    ) -> ResourceErrorBuilder<ResourceOptional, NeedsFieldViolation> {
        ResourceErrorBuilder::__invalid_argument(scope, "Request validation failed")
    }

    /// `permission_denied` builder with a runtime GTS `scope` as the resource type.
    #[must_use]
    pub fn scoped_permission_denied(
        scope: &'static str,
    ) -> ResourceErrorBuilder<ResourceAbsent, NeedsReason> {
        ResourceErrorBuilder::__permission_denied(
            scope,
            "You do not have permission to perform this operation",
        )
    }

    /// `resource_exhausted` builder with a runtime GTS `scope` as the resource type.
    #[must_use]
    pub fn scoped_resource_exhausted(
        scope: &'static str,
    ) -> ResourceErrorBuilder<ResourceOptional, NeedsQuotaViolation> {
        ResourceErrorBuilder::__resource_exhausted(scope, "Quota exceeded")
    }
}

// ---------------------------------------------------------------------------
// create() — gated by Resource + Context Resolved traits
// ---------------------------------------------------------------------------

impl<Resource, Context> ResourceErrorBuilder<Resource, Context>
where
    Resource: ResourceResolved,
    Context: ContextResolved,
{
    #[must_use]
    pub fn create(self) -> CanonicalError {
        let resource_name = self.resource.resolve();
        let ctx_data = self.context.into_context_data();

        let err = match self.variant {
            ErrorVariant::NotFound => CanonicalError::__not_found(NotFound::new()),
            ErrorVariant::AlreadyExists => CanonicalError::__already_exists(AlreadyExists::new()),
            ErrorVariant::Aborted => CanonicalError::__aborted(Aborted::new(&ctx_data.reason)),
            ErrorVariant::Unknown => CanonicalError::__unknown(Unknown::new(&self.detail)),
            ErrorVariant::DeadlineExceeded => {
                CanonicalError::__deadline_exceeded(DeadlineExceeded::new())
            }
            ErrorVariant::PermissionDenied => {
                CanonicalError::__permission_denied(PermissionDenied::new(&ctx_data.reason))
            }
            ErrorVariant::InvalidArgument => {
                let ctx = if let Some(fmt) = ctx_data.format_message {
                    InvalidArgument::format(fmt)
                } else if let Some(cst) = ctx_data.constraint_message {
                    InvalidArgument::constraint(cst)
                } else {
                    InvalidArgument::fields(ctx_data.field_violations)
                };
                CanonicalError::__invalid_argument(ctx)
            }
            ErrorVariant::OutOfRange => {
                CanonicalError::__out_of_range(OutOfRange::new(ctx_data.field_violations))
            }
            ErrorVariant::ResourceExhausted => CanonicalError::__resource_exhausted(
                ResourceExhausted::new(ctx_data.quota_violations),
            ),
            ErrorVariant::FailedPrecondition => CanonicalError::__failed_precondition(
                FailedPrecondition::new(ctx_data.precondition_violations),
            ),
            ErrorVariant::Cancelled => CanonicalError::__cancelled(Cancelled::new()),
            ErrorVariant::Unimplemented => CanonicalError::__unimplemented(Unimplemented::new()),
            ErrorVariant::Internal => CanonicalError::__internal(Internal::new(&self.detail)),
            ErrorVariant::DataLoss => CanonicalError::__data_loss(DataLoss::new()),
            ErrorVariant::Unauthenticated => {
                let mut ctx = Unauthenticated::new();
                if !ctx_data.reason.is_empty() {
                    ctx = ctx.with_reason(ctx_data.reason);
                }
                CanonicalError::__unauthenticated(ctx)
            }
        };

        let mut err = if matches!(
            err,
            CanonicalError::Internal { .. } | CanonicalError::Unknown { .. }
        ) {
            err
        } else {
            err.with_detail(&self.detail)
        };

        if let Some(rt) = self.resource_type {
            err = err.with_resource_type(rt);
        }

        if let Some(rn) = resource_name {
            err.with_resource(rn)
        } else {
            err
        }
    }
}

// ---------------------------------------------------------------------------
// ServiceUnavailableBuilder — dedicated builder for ServiceUnavailable
// ---------------------------------------------------------------------------

pub struct ServiceUnavailableBuilder {
    retry_after_seconds: Option<u64>,
    detail: Option<String>,
}

impl ServiceUnavailableBuilder {
    #[must_use]
    pub fn with_retry_after_seconds(mut self, seconds: u64) -> Self {
        self.retry_after_seconds = Some(seconds);
        self
    }

    /// Override the default `"Service temporarily unavailable"`
    /// `Problem.detail` text. Callers that already curated a safe,
    /// non-secret detail string upstream (e.g. `"authorization
    /// evaluation failed"`, `"IdP plugin unreachable"`) pass it
    /// here so the canonical envelope preserves the precise reason
    /// for the outage rather than collapsing every 503 into the
    /// same opaque message.
    ///
    /// **Caller contract:** the string MUST be safe for the public
    /// `Problem` body — no DSN fragments, no driver text, no
    /// hostnames, no operator-supplied config strings. Sources that
    /// can carry such fragments (raw `DbErr`, vendor SDK error
    /// `Display`) MUST pass through a redaction step (e.g.
    /// `redacted_db_diagnostic`) before calling this builder.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    #[must_use]
    pub fn create(self) -> CanonicalError {
        let detail = self
            .detail
            .unwrap_or_else(|| "Service temporarily unavailable".to_owned());
        CanonicalError::__service_unavailable(ServiceUnavailable::new(self.retry_after_seconds))
            .with_detail(detail)
    }
}
