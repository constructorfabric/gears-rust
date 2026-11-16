use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Shared inner types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldViolationV1 {
    pub field: String,
    pub description: String,
    pub reason: String,
}

impl FieldViolationV1 {
    #[must_use]
    pub fn new(
        field: impl Into<String>,
        description: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            field: field.into(),
            description: description.into(),
            reason: reason.into(),
        }
    }
}

pub type FieldViolation = FieldViolationV1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaViolationV1 {
    pub subject: String,
    pub description: String,
    /// Per-violation retry hint. Populated for rate-limit-style violations
    /// where the upstream knows when capacity returns; `None` for hard
    /// exhaustion (memory, quota with no replenishment window). The error-level
    /// hint is the largest among its violations
    /// ([`ResourceExhaustedV1::retry_after_seconds`]); over HTTP it is also
    /// sent as the `Retry-After` header of the `429` response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_seconds: Option<u64>,
}

impl QuotaViolationV1 {
    #[must_use]
    pub fn new(subject: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            description: description.into(),
            retry_after_seconds: None,
        }
    }
}

pub type QuotaViolation = QuotaViolationV1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreconditionViolationV1 {
    #[serde(rename = "type")]
    pub type_: String,
    pub subject: String,
    pub description: String,
}

impl PreconditionViolationV1 {
    #[must_use]
    pub fn new(
        type_: impl Into<String>,
        subject: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            type_: type_.into(),
            subject: subject.into(),
            description: description.into(),
        }
    }
}

pub type PreconditionViolation = PreconditionViolationV1;

// ---------------------------------------------------------------------------
// Per-category context types
// ---------------------------------------------------------------------------

// 01 Cancelled — context: Cancelled
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::empty_structs_with_brackets)]
pub struct CancelledV1 {}

impl CancelledV1 {
    #[must_use]
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for CancelledV1 {
    fn default() -> Self {
        Self::new()
    }
}

pub type Cancelled = CancelledV1;

// 02 Unknown — context: Unknown
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnknownV1 {
    #[serde(skip)]
    pub description: String,
}

impl UnknownV1 {
    #[must_use]
    pub fn new(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
        }
    }
}

pub type Unknown = UnknownV1;

// 03 InvalidArgument — context: InvalidArgument (enum with 3 variants)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InvalidArgumentV1 {
    FieldViolations {
        field_violations: Vec<FieldViolation>,
    },
    Format {
        format: String,
    },
    Constraint {
        constraint: String,
    },
}

impl InvalidArgumentV1 {
    #[must_use]
    pub fn fields(violations: impl Into<Vec<FieldViolation>>) -> Self {
        Self::FieldViolations {
            field_violations: violations.into(),
        }
    }

    #[must_use]
    pub fn format(msg: impl Into<String>) -> Self {
        Self::Format { format: msg.into() }
    }

    #[must_use]
    pub fn constraint(msg: impl Into<String>) -> Self {
        Self::Constraint {
            constraint: msg.into(),
        }
    }
}

pub type InvalidArgument = InvalidArgumentV1;

// 04 DeadlineExceeded — context: DeadlineExceeded
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::empty_structs_with_brackets)]
pub struct DeadlineExceededV1 {}

impl DeadlineExceededV1 {
    #[must_use]
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for DeadlineExceededV1 {
    fn default() -> Self {
        Self::new()
    }
}

pub type DeadlineExceeded = DeadlineExceededV1;

// 05 NotFound — context: NotFound
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::empty_structs_with_brackets)]
pub struct NotFoundV1 {}

impl NotFoundV1 {
    #[must_use]
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for NotFoundV1 {
    fn default() -> Self {
        Self::new()
    }
}

pub type NotFound = NotFoundV1;

// 06 AlreadyExists — context: AlreadyExists
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::empty_structs_with_brackets)]
pub struct AlreadyExistsV1 {}

impl AlreadyExistsV1 {
    #[must_use]
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for AlreadyExistsV1 {
    fn default() -> Self {
        Self::new()
    }
}

pub type AlreadyExists = AlreadyExistsV1;

// 07 PermissionDenied — context: PermissionDenied
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionDeniedV1 {
    pub reason: String,
}

impl PermissionDeniedV1 {
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

pub type PermissionDenied = PermissionDeniedV1;

// 08 ResourceExhausted — context: ResourceExhausted
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceExhaustedV1 {
    pub violations: Vec<QuotaViolation>,
}

impl ResourceExhaustedV1 {
    #[must_use]
    pub fn new(violations: impl Into<Vec<QuotaViolation>>) -> Self {
        Self {
            violations: violations.into(),
        }
    }

    /// The error's retry hint: the largest `retry_after_seconds` among its
    /// violations, so a caller that waits this long has waited for every
    /// violated limit. Violations without a hint do not count; `None` when no
    /// violation has one (hard exhaustion). The HTTP `Retry-After` header and
    /// SDK projections use this value.
    #[must_use]
    pub fn retry_after_seconds(&self) -> Option<u64> {
        self.violations
            .iter()
            .filter_map(|v| v.retry_after_seconds)
            .max()
    }
}

pub type ResourceExhausted = ResourceExhaustedV1;

// 09 FailedPrecondition — context: FailedPrecondition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedPreconditionV1 {
    pub violations: Vec<PreconditionViolation>,
}

impl FailedPreconditionV1 {
    #[must_use]
    pub fn new(violations: impl Into<Vec<PreconditionViolation>>) -> Self {
        Self {
            violations: violations.into(),
        }
    }
}

pub type FailedPrecondition = FailedPreconditionV1;

// 10 Aborted — context: Aborted
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbortedV1 {
    pub reason: String,
}

impl AbortedV1 {
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

pub type Aborted = AbortedV1;

// 11 OutOfRange — context: OutOfRange
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutOfRangeV1 {
    pub field_violations: Vec<FieldViolation>,
}

impl OutOfRangeV1 {
    #[must_use]
    pub fn new(violations: impl Into<Vec<FieldViolation>>) -> Self {
        Self {
            field_violations: violations.into(),
        }
    }
}

pub type OutOfRange = OutOfRangeV1;

// 12 Unimplemented — context: Unimplemented
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::empty_structs_with_brackets)]
pub struct UnimplementedV1 {}

impl UnimplementedV1 {
    #[must_use]
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for UnimplementedV1 {
    fn default() -> Self {
        Self::new()
    }
}

pub type Unimplemented = UnimplementedV1;

// 13 Internal — context: Internal
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InternalV1 {
    #[serde(skip)]
    pub description: String,
}

impl InternalV1 {
    #[must_use]
    pub fn new(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
        }
    }
}

pub type Internal = InternalV1;

// 14 ServiceUnavailable — context: ServiceUnavailable
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceUnavailableV1 {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_seconds: Option<u64>,
}

impl ServiceUnavailableV1 {
    #[must_use]
    pub fn new(retry_after_seconds: Option<u64>) -> Self {
        Self {
            retry_after_seconds,
        }
    }
}

pub type ServiceUnavailable = ServiceUnavailableV1;

// 15 DataLoss — context: DataLoss
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::empty_structs_with_brackets)]
pub struct DataLossV1 {}

impl DataLossV1 {
    #[must_use]
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for DataLossV1 {
    fn default() -> Self {
        Self::new()
    }
}

pub type DataLoss = DataLossV1;

// 16 Unauthenticated — context: Unauthenticated
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnauthenticatedV1 {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl UnauthenticatedV1 {
    #[must_use]
    pub fn new() -> Self {
        Self { reason: None }
    }

    #[must_use]
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

impl Default for UnauthenticatedV1 {
    fn default() -> Self {
        Self::new()
    }
}

pub type Unauthenticated = UnauthenticatedV1;
