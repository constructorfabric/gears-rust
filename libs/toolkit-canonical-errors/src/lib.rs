extern crate self as toolkit_canonical_errors;

pub mod builder;
pub mod context;
pub mod error;
pub mod problem;

pub use builder::{ResourceErrorBuilder, ServiceUnavailableBuilder};
pub use context::{
    Aborted, AbortedV1, AlreadyExists, AlreadyExistsV1, Cancelled, CancelledV1, DataLoss,
    DataLossV1, DeadlineExceeded, DeadlineExceededV1, FailedPrecondition, FailedPreconditionV1,
    FieldViolation, FieldViolationV1, Internal, InternalV1, InvalidArgument, InvalidArgumentV1,
    NotFound, NotFoundV1, OutOfRange, OutOfRangeV1, PermissionDenied, PermissionDeniedV1,
    PreconditionViolation, PreconditionViolationV1, QuotaViolation, QuotaViolationV1,
    ResourceExhausted, ResourceExhaustedV1, ServiceUnavailable, ServiceUnavailableV1,
    Unauthenticated, UnauthenticatedV1, Unimplemented, UnimplementedV1, Unknown, UnknownV1,
};
pub use error::CanonicalError;
pub use problem::{Problem, ProblemCategory, ProblemConversionError};
pub use toolkit_canonical_errors_macro::resource_error;
