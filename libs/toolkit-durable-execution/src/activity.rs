//! Retry policies, safe failure codes and per-invocation metadata.
use crate::{
    ActivityId, ActivityInstanceId, DefinitionError, DefinitionErrorKind, ExecutionOwner, RunId,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, time::Duration};
use tokio_util::sync::CancellationToken;

/// Automatic retry budget and bounded exponential backoff for one activity instance.
///
/// Construct with [`RetryPolicy::exponential`] or [`RetryPolicy::no_retry`]; the
/// fields stay readable for diagnostics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RetryPolicy {
    /// Total automatic attempts per epoch, including the initial invocation.
    pub max_attempts: u32,
    /// Base delay after the first retryable failure.
    pub initial_delay: Duration,
    /// Maximum base backoff. Additive jitter can add up to 20%.
    pub max_delay: Duration,
}
impl Default for RetryPolicy {
    /// Ten attempts, backing off from 15 seconds to 5 minutes.
    fn default() -> Self {
        Self::exponential(10, Duration::from_secs(15), Duration::from_mins(5))
    }
}
impl RetryPolicy {
    /// Largest accepted `max_delay`.
    pub const MAX_DELAY: Duration = Duration::from_hours(24);

    /// Retry up to `max_attempts` invocations, doubling the delay from `initial_delay`
    /// up to `max_delay`. [`RetryPolicy::validate`] runs when the workflow is built.
    #[must_use]
    pub const fn exponential(
        max_attempts: u32,
        initial_delay: Duration,
        max_delay: Duration,
    ) -> Self {
        Self {
            max_attempts,
            initial_delay,
            max_delay,
        }
    }

    /// A single invocation without automatic retries.
    #[must_use]
    pub const fn no_retry() -> Self {
        Self::exponential(1, Duration::from_secs(1), Duration::from_secs(1))
    }

    /// # Errors
    /// [`DefinitionErrorKind::InvalidRetryPolicy`] for a zero budget, a zero initial
    /// delay, or a maximum delay below the initial delay or above [`Self::MAX_DELAY`].
    pub fn validate(&self) -> Result<(), DefinitionError> {
        if self.max_attempts == 0
            || self.initial_delay.is_zero()
            || self.max_delay < self.initial_delay
            || self.max_delay > Self::MAX_DELAY
        {
            return Err(DefinitionError::new(
                DefinitionErrorKind::InvalidRetryPolicy,
            ));
        }
        Ok(())
    }

    /// Runtime scheduling helper. `jitter_sample` is a 0..=1000 sample; jitter only
    /// adds time, so a provider's `retry_after` is never shortened.
    #[doc(hidden)]
    #[must_use]
    #[expect(
        clippy::integer_division,
        reason = "Nanosecond jitter deliberately rounds down."
    )]
    pub fn delay(
        &self,
        attempt: u32,
        retry_after: Option<Duration>,
        jitter_sample: u16,
    ) -> Duration {
        let factor = 1u32
            .checked_shl(attempt.saturating_sub(1).min(31))
            .unwrap_or(u32::MAX);
        let base = self
            .initial_delay
            .saturating_mul(factor)
            .min(self.max_delay);
        // Nanosecond arithmetic keeps sub-millisecond delays exact.
        let jitter = base.as_nanos() * u128::from(jitter_sample.min(1000)) / 5000;
        let jitter = Duration::from_nanos(u64::try_from(jitter).unwrap_or(u64::MAX));
        base.saturating_add(jitter)
            .max(retry_after.unwrap_or_default())
    }
}

/// Safe machine-readable failure code: 1..=64 bytes of `[a-z0-9_]`.
///
/// Codes are persisted and shown to callers, so they never carry error details.
/// Text that is not a valid code becomes [`ErrorCode::INVALID`] instead of leaking.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ErrorCode(Cow<'static, str>);
impl ErrorCode {
    /// Maximum code length in bytes.
    pub const MAX_LEN: usize = 64;
    /// Replacement for text that is not a valid code.
    pub const INVALID: Self = Self::from_static("invalid_error_code");
    /// Code reported for confirmed cancellation.
    pub const CANCELLED: Self = Self::from_static(CANCELLED_CODE);

    /// Validate a literal code.
    ///
    /// The check runs at compile time only in a `const` context; a plain call checks
    /// at run time and panics there. Declare codes as constants:
    ///
    /// ```
    /// use toolkit_durable_execution::{ActivityError, ErrorCode};
    /// const STOCK_UNAVAILABLE: ErrorCode = ErrorCode::from_static("stock_unavailable");
    /// let error = ActivityError::permanent(STOCK_UNAVAILABLE);
    /// assert_eq!(error.code(), "stock_unavailable");
    /// ```
    ///
    /// ```compile_fail
    /// use toolkit_durable_execution::ErrorCode;
    /// const BAD: ErrorCode = ErrorCode::from_static("Not A Code");
    /// ```
    ///
    /// # Panics
    /// Panics when `code` is not 1..=64 bytes of `[a-z0-9_]` and the call is not in a
    /// `const` context; in a `const` item the build fails instead.
    #[must_use]
    pub const fn from_static(code: &'static str) -> Self {
        assert!(
            is_valid_code(code),
            "error code must be 1..=64 bytes of [a-z0-9_]"
        );
        Self(Cow::Borrowed(code))
    }

    /// Strict parse; `None` when `code` is not a valid code.
    #[must_use]
    pub fn new(code: &str) -> Option<Self> {
        is_valid_code(code).then(|| Self(Cow::Owned(code.to_owned())))
    }

    /// Accept a valid code or substitute [`ErrorCode::INVALID`].
    #[must_use]
    pub fn sanitize(code: &str) -> Self {
        Self::new(code).unwrap_or(Self::INVALID)
    }

    /// The validated code.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
const CANCELLED_CODE: &str = "cancelled";
const fn is_valid_code(code: &str) -> bool {
    let bytes = code.as_bytes();
    if bytes.is_empty() || bytes.len() > ErrorCode::MAX_LEN {
        return false;
    }
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if !(b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') {
            return false;
        }
        i += 1;
    }
    true
}
impl From<&str> for ErrorCode {
    fn from(code: &str) -> Self {
        Self::sanitize(code)
    }
}
impl From<String> for ErrorCode {
    fn from(code: String) -> Self {
        if is_valid_code(&code) {
            Self(Cow::Owned(code))
        } else {
            Self::INVALID
        }
    }
}
impl From<&String> for ErrorCode {
    fn from(code: &String) -> Self {
        Self::sanitize(code)
    }
}
impl AsRef<str> for ErrorCode {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}
impl PartialEq<str> for ErrorCode {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}
impl PartialEq<&str> for ErrorCode {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}
impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
impl std::fmt::Debug for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_str(), f)
    }
}

/// Outcome of a failed invocation. Carries safe codes only; keep error detail in
/// redacted diagnostics owned by the provider client.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ActivityError {
    /// A transient failure eligible for the policy retry budget.
    Retryable {
        /// Safe machine-readable error code.
        code: ErrorCode,
        /// Minimum provider-requested delay before the next attempt.
        retry_after: Option<Duration>,
    },
    /// A definitive failure that ends forward execution or stops reversal.
    Permanent {
        /// Safe machine-readable error code.
        code: ErrorCode,
    },
    /// Cancellation confirmed after owned work has stopped.
    Cancelled,
}
impl ActivityError {
    /// A transient failure; see [`ErrorCode`] for accepted codes.
    #[must_use]
    pub fn retryable(code: impl Into<ErrorCode>) -> Self {
        Self::Retryable {
            code: code.into(),
            retry_after: None,
        }
    }
    /// A transient failure that honours a provider-requested minimum delay.
    #[must_use]
    pub fn retryable_after(code: impl Into<ErrorCode>, delay: Duration) -> Self {
        Self::Retryable {
            code: code.into(),
            retry_after: Some(delay),
        }
    }
    /// A definitive failure; see [`ErrorCode`] for accepted codes.
    #[must_use]
    pub fn permanent(code: impl Into<ErrorCode>) -> Self {
        Self::Permanent { code: code.into() }
    }
    /// Safe machine-readable code; `cancelled` for [`ActivityError::Cancelled`].
    #[must_use]
    pub fn code(&self) -> &str {
        match self {
            Self::Retryable { code, .. } | Self::Permanent { code } => code.as_str(),
            Self::Cancelled => CANCELLED_CODE,
        }
    }
    /// Whether the runtime may retry automatically within the policy budget.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Retryable { .. })
    }
}
impl std::fmt::Display for ActivityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for ActivityError {}

/// Classify provider failures with `?`.
///
/// The original error is dropped on purpose: it may contain payloads or
/// credentials. Log it in redacted form before converting when diagnostics matter.
///
/// ```
/// use toolkit_durable_execution::{ActivityError, ActivityResultExt};
/// fn parse(raw: &str) -> Result<u32, ActivityError> {
///     raw.parse::<u32>().or_permanent("quantity_invalid")
/// }
/// assert_eq!(parse("x").unwrap_err().code(), "quantity_invalid");
/// ```
pub trait ActivityResultExt<T> {
    /// Map any error to [`ActivityError::Retryable`].
    ///
    /// # Errors
    /// Returns the retryable error when `self` is an error.
    fn or_retryable(self, code: impl Into<ErrorCode>) -> Result<T, ActivityError>;
    /// Map any error to [`ActivityError::Permanent`].
    ///
    /// # Errors
    /// Returns the permanent error when `self` is an error.
    fn or_permanent(self, code: impl Into<ErrorCode>) -> Result<T, ActivityError>;
}
impl<T, E> ActivityResultExt<T> for Result<T, E> {
    fn or_retryable(self, code: impl Into<ErrorCode>) -> Result<T, ActivityError> {
        self.map_err(|_| ActivityError::retryable(code))
    }
    fn or_permanent(self, code: impl Into<ErrorCode>) -> Result<T, ActivityError> {
        self.map_err(|_| ActivityError::permanent(code))
    }
}

/// Metadata for one forward or reverse invocation. Contains no payloads or credentials.
#[derive(Clone)]
pub struct ActivityContext {
    instance: ActivityInstanceId,
    run_id: RunId,
    activity_id: ActivityId,
    attempt: u32,
    execution_epoch: u64,
    idempotency_key: String,
    owner: ExecutionOwner,
    deadline: DateTime<Utc>,
    cancellation: CancellationToken,
}
impl ActivityContext {
    /// Template, optional fan-out item index and phase of this invocation.
    #[must_use]
    pub fn instance(&self) -> &ActivityInstanceId {
        &self.instance
    }
    /// Durable run identifier.
    #[must_use]
    pub fn run_id(&self) -> RunId {
        self.run_id
    }
    /// Encoded instance ID used for delivery and fencing.
    #[must_use]
    pub fn activity_id(&self) -> &ActivityId {
        &self.activity_id
    }
    /// Lifetime invocation number, starting at one.
    #[must_use]
    pub fn attempt(&self) -> u32 {
        self.attempt
    }
    /// Epoch advanced by an explicit Retry or Resume.
    #[must_use]
    pub fn execution_epoch(&self) -> u64 {
        self.execution_epoch
    }
    /// Stable provider deduplication key. Forward and reverse invocations get
    /// distinct keys. Never use it as a credential.
    #[must_use]
    pub fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }
    /// Original submitting identity. Domain access still needs fresh authorization.
    #[must_use]
    pub fn owner(&self) -> &ExecutionOwner {
        &self.owner
    }
    /// Wall-clock deadline of this invocation.
    #[must_use]
    pub fn deadline(&self) -> DateTime<Utc> {
        self.deadline
    }
    /// Cooperative cancellation. Return only after owned work has stopped.
    #[must_use]
    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }
}
impl std::fmt::Debug for ActivityContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActivityContext")
            .field("instance", &self.instance)
            .field("run_id", &self.run_id)
            .field("activity_id", &self.activity_id)
            .field("attempt", &self.attempt)
            .field("execution_epoch", &self.execution_epoch)
            .field("owner", &self.owner)
            .field("deadline", &self.deadline)
            .finish_non_exhaustive()
    }
}

/// Runtime-side constructor input for [`ActivityContext`].
#[doc(hidden)]
#[derive(Debug)]
pub struct ActivityContextInit {
    pub instance: ActivityInstanceId,
    pub run_id: RunId,
    pub activity_id: ActivityId,
    pub attempt: u32,
    pub execution_epoch: u64,
    pub idempotency_key: String,
    pub owner: ExecutionOwner,
    pub deadline: DateTime<Utc>,
    pub cancellation: CancellationToken,
}
impl From<ActivityContextInit> for ActivityContext {
    fn from(init: ActivityContextInit) -> Self {
        Self {
            instance: init.instance,
            run_id: init.run_id,
            activity_id: init.activity_id,
            attempt: init.attempt,
            execution_epoch: init.execution_epoch,
            idempotency_key: init.idempotency_key,
            owner: init.owner,
            deadline: init.deadline,
            cancellation: init.cancellation,
        }
    }
}
