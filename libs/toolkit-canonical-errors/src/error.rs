use std::fmt;

// `gts_id!` is the prefix-free GTS-id construction macro (re-exported at the
// crate root from `gts_macros`). It expands at compile time to a `&'static
// str` literal with the configured `GTS_ID_PREFIX`
// prepended, so the canonical error type ids below track the configured
// prefix without hard-coding the literal prefix.
use crate::gts_id;

use crate::context::{
    Aborted, AlreadyExists, Cancelled, DataLoss, DeadlineExceeded, FailedPrecondition, Internal,
    InvalidArgument, NotFound, OutOfRange, PermissionDenied, ResourceExhausted, ServiceUnavailable,
    Unauthenticated, Unimplemented, Unknown,
};

// ---------------------------------------------------------------------------
// CanonicalError Enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum CanonicalError {
    #[non_exhaustive]
    Cancelled {
        ctx: Cancelled,
        detail: String,
        resource_type: Option<String>,
        resource_name: Option<String>,
    },
    #[non_exhaustive]
    Unknown {
        ctx: Unknown,
        detail: String,
        resource_type: Option<String>,
        resource_name: Option<String>,
    },
    #[non_exhaustive]
    InvalidArgument {
        ctx: InvalidArgument,
        detail: String,
        resource_type: Option<String>,
        resource_name: Option<String>,
    },
    #[non_exhaustive]
    DeadlineExceeded {
        ctx: DeadlineExceeded,
        detail: String,
        resource_type: Option<String>,
        resource_name: Option<String>,
    },
    #[non_exhaustive]
    NotFound {
        ctx: NotFound,
        detail: String,
        resource_type: Option<String>,
        resource_name: Option<String>,
    },
    #[non_exhaustive]
    AlreadyExists {
        ctx: AlreadyExists,
        detail: String,
        resource_type: Option<String>,
        resource_name: Option<String>,
    },
    #[non_exhaustive]
    PermissionDenied {
        ctx: PermissionDenied,
        detail: String,
        resource_type: Option<String>,
        resource_name: Option<String>,
    },
    #[non_exhaustive]
    ResourceExhausted {
        ctx: ResourceExhausted,
        detail: String,
        resource_type: Option<String>,
        resource_name: Option<String>,
    },
    #[non_exhaustive]
    FailedPrecondition {
        ctx: FailedPrecondition,
        detail: String,
        resource_type: Option<String>,
        resource_name: Option<String>,
    },
    #[non_exhaustive]
    Aborted {
        ctx: Aborted,
        detail: String,
        resource_type: Option<String>,
        resource_name: Option<String>,
    },
    #[non_exhaustive]
    OutOfRange {
        ctx: OutOfRange,
        detail: String,
        resource_type: Option<String>,
        resource_name: Option<String>,
    },
    #[non_exhaustive]
    Unimplemented {
        ctx: Unimplemented,
        detail: String,
        resource_type: Option<String>,
        resource_name: Option<String>,
    },
    #[non_exhaustive]
    Internal { ctx: Internal, detail: String },
    #[non_exhaustive]
    ServiceUnavailable {
        ctx: ServiceUnavailable,
        detail: String,
        resource_type: Option<String>,
        resource_name: Option<String>,
    },
    #[non_exhaustive]
    DataLoss {
        ctx: DataLoss,
        detail: String,
        resource_type: Option<String>,
        resource_name: Option<String>,
    },
    #[non_exhaustive]
    Unauthenticated {
        ctx: Unauthenticated,
        detail: String,
        resource_type: Option<String>,
        resource_name: Option<String>,
    },
}

impl CanonicalError {
    // --- Ergonomic constructors (one per category) ---

    #[doc(hidden)]
    #[must_use]
    pub(crate) fn __cancelled(ctx: Cancelled) -> Self {
        Self::Cancelled {
            ctx,
            detail: String::from("Operation cancelled by the client"),
            resource_type: None,
            resource_name: None,
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub(crate) fn __unknown(ctx: Unknown) -> Self {
        Self::Unknown {
            ctx,
            detail: String::from("An unknown error occurred"),
            resource_type: None,
            resource_name: None,
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub(crate) fn __invalid_argument(ctx: InvalidArgument) -> Self {
        let detail = match &ctx {
            InvalidArgument::FieldViolations { .. } => String::from("Request validation failed"),
            InvalidArgument::Format { format } => format.clone(),
            InvalidArgument::Constraint { constraint } => constraint.clone(),
        };
        Self::InvalidArgument {
            ctx,
            detail,
            resource_type: None,
            resource_name: None,
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub(crate) fn __deadline_exceeded(ctx: DeadlineExceeded) -> Self {
        Self::DeadlineExceeded {
            ctx,
            detail: String::from("Operation did not complete within the allowed time"),
            resource_type: None,
            resource_name: None,
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub(crate) fn __not_found(ctx: NotFound) -> Self {
        Self::NotFound {
            ctx,
            detail: String::from("Resource not found"),
            resource_type: None,
            resource_name: None,
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub(crate) fn __already_exists(ctx: AlreadyExists) -> Self {
        Self::AlreadyExists {
            ctx,
            detail: String::from("Resource already exists"),
            resource_type: None,
            resource_name: None,
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub(crate) fn __permission_denied(ctx: PermissionDenied) -> Self {
        Self::PermissionDenied {
            ctx,
            detail: String::from("You do not have permission to perform this operation"),
            resource_type: None,
            resource_name: None,
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub(crate) fn __resource_exhausted(ctx: ResourceExhausted) -> Self {
        Self::ResourceExhausted {
            ctx,
            detail: String::from("Quota exceeded"),
            resource_type: None,
            resource_name: None,
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub(crate) fn __failed_precondition(ctx: FailedPrecondition) -> Self {
        Self::FailedPrecondition {
            ctx,
            detail: String::from("Operation precondition not met"),
            resource_type: None,
            resource_name: None,
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub(crate) fn __aborted(ctx: Aborted) -> Self {
        Self::Aborted {
            ctx,
            detail: String::from("Operation aborted due to concurrency conflict"),
            resource_type: None,
            resource_name: None,
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub(crate) fn __out_of_range(ctx: OutOfRange) -> Self {
        Self::OutOfRange {
            ctx,
            detail: String::from("Value out of range"),
            resource_type: None,
            resource_name: None,
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub(crate) fn __unimplemented(ctx: Unimplemented) -> Self {
        Self::Unimplemented {
            ctx,
            detail: String::from("This operation is not implemented"),
            resource_type: None,
            resource_name: None,
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub(crate) fn __internal(ctx: Internal) -> Self {
        Self::Internal {
            ctx,
            detail: String::from("An internal error occurred. Please retry later."),
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub(crate) fn __service_unavailable(ctx: ServiceUnavailable) -> Self {
        Self::ServiceUnavailable {
            ctx,
            detail: String::from("Service temporarily unavailable"),
            resource_type: None,
            resource_name: None,
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub(crate) fn __data_loss(ctx: DataLoss) -> Self {
        Self::DataLoss {
            ctx,
            detail: String::from("Data loss detected"),
            resource_type: None,
            resource_name: None,
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub(crate) fn __unauthenticated(ctx: Unauthenticated) -> Self {
        Self::Unauthenticated {
            ctx,
            detail: String::from("Authentication required"),
            resource_type: None,
            resource_name: None,
        }
    }

    // --- Builder methods ---

    #[must_use]
    pub(crate) fn with_detail(mut self, msg: impl Into<String>) -> Self {
        let msg = msg.into();
        match &mut self {
            Self::Cancelled { detail, .. }
            | Self::Unknown { detail, .. }
            | Self::InvalidArgument { detail, .. }
            | Self::DeadlineExceeded { detail, .. }
            | Self::NotFound { detail, .. }
            | Self::AlreadyExists { detail, .. }
            | Self::PermissionDenied { detail, .. }
            | Self::ResourceExhausted { detail, .. }
            | Self::FailedPrecondition { detail, .. }
            | Self::Aborted { detail, .. }
            | Self::OutOfRange { detail, .. }
            | Self::Unimplemented { detail, .. }
            | Self::ServiceUnavailable { detail, .. }
            | Self::DataLoss { detail, .. }
            | Self::Unauthenticated { detail, .. }
            | Self::Internal { detail, .. } => *detail = msg,
        }
        self
    }

    #[must_use]
    pub(crate) fn with_resource_type(mut self, rt: impl Into<String>) -> Self {
        let rt = Some(rt.into());
        match &mut self {
            Self::Cancelled { resource_type, .. }
            | Self::Unknown { resource_type, .. }
            | Self::InvalidArgument { resource_type, .. }
            | Self::DeadlineExceeded { resource_type, .. }
            | Self::NotFound { resource_type, .. }
            | Self::AlreadyExists { resource_type, .. }
            | Self::PermissionDenied { resource_type, .. }
            | Self::ResourceExhausted { resource_type, .. }
            | Self::FailedPrecondition { resource_type, .. }
            | Self::Aborted { resource_type, .. }
            | Self::OutOfRange { resource_type, .. }
            | Self::Unimplemented { resource_type, .. }
            | Self::ServiceUnavailable { resource_type, .. }
            | Self::DataLoss { resource_type, .. }
            | Self::Unauthenticated { resource_type, .. } => *resource_type = rt,
            Self::Internal { .. } => {}
        }
        self
    }

    #[must_use]
    pub(crate) fn with_resource(mut self, rn: impl Into<String>) -> Self {
        let rn = Some(rn.into());
        match &mut self {
            Self::Cancelled { resource_name, .. }
            | Self::Unknown { resource_name, .. }
            | Self::InvalidArgument { resource_name, .. }
            | Self::DeadlineExceeded { resource_name, .. }
            | Self::NotFound { resource_name, .. }
            | Self::AlreadyExists { resource_name, .. }
            | Self::PermissionDenied { resource_name, .. }
            | Self::ResourceExhausted { resource_name, .. }
            | Self::FailedPrecondition { resource_name, .. }
            | Self::Aborted { resource_name, .. }
            | Self::OutOfRange { resource_name, .. }
            | Self::Unimplemented { resource_name, .. }
            | Self::ServiceUnavailable { resource_name, .. }
            | Self::DataLoss { resource_name, .. }
            | Self::Unauthenticated { resource_name, .. } => *resource_name = rn,
            Self::Internal { .. } => {}
        }
        self
    }

    // --- Accessors ---

    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            Self::Cancelled { detail, .. }
            | Self::Unknown { detail, .. }
            | Self::InvalidArgument { detail, .. }
            | Self::DeadlineExceeded { detail, .. }
            | Self::NotFound { detail, .. }
            | Self::AlreadyExists { detail, .. }
            | Self::PermissionDenied { detail, .. }
            | Self::ResourceExhausted { detail, .. }
            | Self::FailedPrecondition { detail, .. }
            | Self::Aborted { detail, .. }
            | Self::OutOfRange { detail, .. }
            | Self::Unimplemented { detail, .. }
            | Self::ServiceUnavailable { detail, .. }
            | Self::DataLoss { detail, .. }
            | Self::Unauthenticated { detail, .. }
            | Self::Internal { detail, .. } => detail,
        }
    }

    #[must_use]
    pub fn resource_type(&self) -> Option<&str> {
        match self {
            Self::Cancelled { resource_type, .. }
            | Self::Unknown { resource_type, .. }
            | Self::InvalidArgument { resource_type, .. }
            | Self::DeadlineExceeded { resource_type, .. }
            | Self::NotFound { resource_type, .. }
            | Self::AlreadyExists { resource_type, .. }
            | Self::PermissionDenied { resource_type, .. }
            | Self::ResourceExhausted { resource_type, .. }
            | Self::FailedPrecondition { resource_type, .. }
            | Self::Aborted { resource_type, .. }
            | Self::OutOfRange { resource_type, .. }
            | Self::Unimplemented { resource_type, .. }
            | Self::ServiceUnavailable { resource_type, .. }
            | Self::DataLoss { resource_type, .. }
            | Self::Unauthenticated { resource_type, .. } => resource_type.as_deref(),
            Self::Internal { .. } => None,
        }
    }

    #[must_use]
    pub fn resource_name(&self) -> Option<&str> {
        match self {
            Self::Cancelled { resource_name, .. }
            | Self::Unknown { resource_name, .. }
            | Self::InvalidArgument { resource_name, .. }
            | Self::DeadlineExceeded { resource_name, .. }
            | Self::NotFound { resource_name, .. }
            | Self::AlreadyExists { resource_name, .. }
            | Self::PermissionDenied { resource_name, .. }
            | Self::ResourceExhausted { resource_name, .. }
            | Self::FailedPrecondition { resource_name, .. }
            | Self::Aborted { resource_name, .. }
            | Self::OutOfRange { resource_name, .. }
            | Self::Unimplemented { resource_name, .. }
            | Self::ServiceUnavailable { resource_name, .. }
            | Self::DataLoss { resource_name, .. }
            | Self::Unauthenticated { resource_name, .. } => resource_name.as_deref(),
            Self::Internal { .. } => None,
        }
    }

    /// Returns the internal diagnostic string for `Internal` and `Unknown`
    /// variants, or `None` for all other categories.
    ///
    /// Middleware should call this **before** converting to `Problem` so
    /// that the real cause can be logged server-side with the `trace_id`.
    /// The diagnostic is never included in production wire responses.
    #[must_use]
    pub fn diagnostic(&self) -> Option<&str> {
        match self {
            Self::Internal { ctx, .. } => Some(&ctx.description),
            Self::Unknown { ctx, .. } => Some(&ctx.description),
            _ => None,
        }
    }

    // --- Metadata accessors (direct match) ---

    #[must_use]
    pub fn gts_type(&self) -> &'static str {
        match self {
            Self::Cancelled { .. } => gts_id!("cf.core.errors.err.v1~cf.core.err.cancelled.v1~"),
            Self::Unknown { .. } => gts_id!("cf.core.errors.err.v1~cf.core.err.unknown.v1~"),
            Self::InvalidArgument { .. } => {
                gts_id!("cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~")
            }
            Self::DeadlineExceeded { .. } => {
                gts_id!("cf.core.errors.err.v1~cf.core.err.deadline_exceeded.v1~")
            }
            Self::NotFound { .. } => gts_id!("cf.core.errors.err.v1~cf.core.err.not_found.v1~"),
            Self::AlreadyExists { .. } => {
                gts_id!("cf.core.errors.err.v1~cf.core.err.already_exists.v1~")
            }
            Self::PermissionDenied { .. } => {
                gts_id!("cf.core.errors.err.v1~cf.core.err.permission_denied.v1~")
            }
            Self::ResourceExhausted { .. } => {
                gts_id!("cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~")
            }
            Self::FailedPrecondition { .. } => {
                gts_id!("cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~")
            }
            Self::Aborted { .. } => gts_id!("cf.core.errors.err.v1~cf.core.err.aborted.v1~"),
            Self::OutOfRange { .. } => {
                gts_id!("cf.core.errors.err.v1~cf.core.err.out_of_range.v1~")
            }
            Self::Unimplemented { .. } => {
                gts_id!("cf.core.errors.err.v1~cf.core.err.unimplemented.v1~")
            }
            Self::Internal { .. } => gts_id!("cf.core.errors.err.v1~cf.core.err.internal.v1~"),
            Self::ServiceUnavailable { .. } => {
                gts_id!("cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~")
            }
            Self::DataLoss { .. } => gts_id!("cf.core.errors.err.v1~cf.core.err.data_loss.v1~"),
            Self::Unauthenticated { .. } => {
                gts_id!("cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~")
            }
        }
    }

    #[must_use]
    pub fn status_code(&self) -> u16 {
        match self {
            Self::InvalidArgument { .. }
            | Self::FailedPrecondition { .. }
            | Self::OutOfRange { .. } => 400,
            Self::Unauthenticated { .. } => 401,
            Self::PermissionDenied { .. } => 403,
            Self::NotFound { .. } => 404,
            Self::AlreadyExists { .. } | Self::Aborted { .. } => 409,
            Self::ResourceExhausted { .. } => 429,
            Self::Cancelled { .. } => 499,
            Self::Unknown { .. } | Self::Internal { .. } | Self::DataLoss { .. } => 500,
            Self::Unimplemented { .. } => 501,
            Self::ServiceUnavailable { .. } => 503,
            Self::DeadlineExceeded { .. } => 504,
        }
    }

    #[must_use]
    pub fn title(&self) -> &'static str {
        match self {
            Self::Cancelled { .. } => "Cancelled",
            Self::Unknown { .. } => "Unknown",
            Self::InvalidArgument { .. } => "Invalid Argument",
            Self::DeadlineExceeded { .. } => "Deadline Exceeded",
            Self::NotFound { .. } => "Not Found",
            Self::AlreadyExists { .. } => "Already Exists",
            Self::PermissionDenied { .. } => "Permission Denied",
            Self::ResourceExhausted { .. } => "Resource Exhausted",
            Self::FailedPrecondition { .. } => "Failed Precondition",
            Self::Aborted { .. } => "Aborted",
            Self::OutOfRange { .. } => "Out of Range",
            Self::Unimplemented { .. } => "Unimplemented",
            Self::Internal { .. } => "Internal",
            Self::ServiceUnavailable { .. } => "Service Unavailable",
            Self::DataLoss { .. } => "Data Loss",
            Self::Unauthenticated { .. } => "Unauthenticated",
        }
    }

    fn category_name(&self) -> &'static str {
        match self {
            Self::Cancelled { .. } => "cancelled",
            Self::Unknown { .. } => "unknown",
            Self::InvalidArgument { .. } => "invalid_argument",
            Self::DeadlineExceeded { .. } => "deadline_exceeded",
            Self::NotFound { .. } => "not_found",
            Self::AlreadyExists { .. } => "already_exists",
            Self::PermissionDenied { .. } => "permission_denied",
            Self::ResourceExhausted { .. } => "resource_exhausted",
            Self::FailedPrecondition { .. } => "failed_precondition",
            Self::Aborted { .. } => "aborted",
            Self::OutOfRange { .. } => "out_of_range",
            Self::Unimplemented { .. } => "unimplemented",
            Self::Internal { .. } => "internal",
            Self::ServiceUnavailable { .. } => "service_unavailable",
            Self::DataLoss { .. } => "data_loss",
            Self::Unauthenticated { .. } => "unauthenticated",
        }
    }
}

impl fmt::Display for CanonicalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.category_name(), self.detail())
    }
}

impl std::error::Error for CanonicalError {}

// ---------------------------------------------------------------------------
// From impls for common library errors (? propagation)
// ---------------------------------------------------------------------------

// TODO(DE1302): `Internal` only carries a description string today, so these
// From impls lose the source error chain. Refactor `Internal` to also carry a
// boxed source so `.source()` returns the original error, then remove these
// allows.
#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<std::io::Error> for CanonicalError {
    fn from(err: std::io::Error) -> Self {
        Self::__internal(Internal::new(err.to_string()))
    }
}

#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<serde_json::Error> for CanonicalError {
    fn from(err: serde_json::Error) -> Self {
        Self::__invalid_argument(InvalidArgument::format(err.to_string()))
    }
}

#[cfg(feature = "sea-orm")]
#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<sea_orm::DbErr> for CanonicalError {
    fn from(err: sea_orm::DbErr) -> Self {
        Self::__internal(Internal::new(err.to_string()))
    }
}
