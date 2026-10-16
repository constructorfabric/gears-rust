//! Submit-and-poll under a deadline and cancellation; reconciliation's submit step.

use std::time::Duration;

use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;
use uuid::Uuid;

use crate::contract::PlatformTypesRegistryApi;
use crate::field;
use crate::gts::{OperationResource, TypeResource};
use crate::models::{
    IdempotencyKey, OperationStatus, RegisterEntitiesRequest, RegistrationOperation,
};

/// Initial polling backoff, doubling to [`POLL_INTERVAL_MAX`].
/// Transports apply Retry-After inside `get_operation`; semantic models carry no pacing hints.
pub const POLL_INTERVAL_INITIAL: Duration = Duration::from_millis(50);

/// Longest interval between two operation polls.
pub const POLL_INTERVAL_MAX: Duration = Duration::from_secs(1);

/// Registry error by call stage.
#[derive(Debug, Clone)]
pub enum Failure {
    /// `register_entities` error; acceptance is possible, so retries reuse the key.
    Submit(CanonicalError),
    /// Error reading an accepted operation.
    Poll(CanonicalError),
}

impl Failure {
    /// Extract the error without its call stage.
    pub fn into_error(self) -> CanonicalError {
        match self {
            Self::Submit(error) | Self::Poll(error) => error,
        }
    }
}

/// Submit and poll under one deadline; reconciliation's submit step.
///
/// The outer error is a local stop; the inner error is the registry's answer.
pub async fn await_registration<A: PlatformTypesRegistryApi + ?Sized>(
    api: &A,
    ctx: &PlatformSecurityContext,
    key: IdempotencyKey,
    request: RegisterEntitiesRequest,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<Result<RegistrationOperation, Failure>, Stop> {
    let mut operation = match bounded(
        deadline,
        cancel,
        None,
        api.register_entities(ctx, key, request),
    )
    .await?
    {
        Ok(operation) => operation,
        Err(error) => return Ok(Err(Failure::Submit(error))),
    };
    let operation_id = operation.operation_id;
    // Known at once, so every later event of the submission — and of a lost read — names it.
    tracing::Span::current().record("operation_id", tracing::field::display(operation_id));
    tracing::info!(
        log_kind = "batch_accepted",
        %operation_id,
        status = ?operation.status,
        "the registry accepted the submission"
    );
    let mut interval = POLL_INTERVAL_INITIAL;
    while operation.status != OperationStatus::Completed {
        bounded(
            deadline,
            cancel,
            Some(operation_id),
            sleep_until(Instant::now() + interval),
        )
        .await?;
        interval = (interval * 2).min(POLL_INTERVAL_MAX);
        let polled = match bounded(
            deadline,
            cancel,
            Some(operation_id),
            api.get_operation(ctx, operation_id),
        )
        .await?
        {
            Ok(polled) => polled,
            Err(error) => return Ok(Err(Failure::Poll(error))),
        };
        let crate::models::Operation::Registration(polled) = polled else {
            return Ok(Err(Failure::Poll(
                OperationResource::aborted(format!(
                    "the registry answered a poll of registration operation {operation_id} \
                     with a deletion"
                ))
                .with_reason(crate::reason::aborted::RECONCILIATION_PROTOCOL_ERROR)
                .with_resource(operation_id.to_string())
                .create(),
            )));
        };
        operation = polled;
    }
    Ok(Ok(operation))
}

/// Equal jitter in [backoff/2, backoff] spreads concurrent retries.
pub fn jittered(backoff: Duration) -> Duration {
    use rand::RngExt as _;
    let half = backoff / 2;
    let spread = u64::try_from(half.as_nanos()).unwrap_or(u64::MAX);
    half + Duration::from_nanos(rand::rng().random_range(0..=spread))
}

/// `start + budget`, or `InvalidArgument` for a budget no clock can represent.
pub fn deadline_after(start: Instant, budget: Duration) -> Result<Instant, CanonicalError> {
    start.checked_add(budget).ok_or_else(|| {
        TypeResource::invalid_argument()
            .with_field_violation(
                field::DEADLINE_FIELD,
                format!("a deadline of {budget:?} from now cannot be represented"),
                field::INVALID_DEADLINE,
            )
            .create()
    })
}

/// Deadline/cancellation win before the first poll and when simultaneously ready.
/// `timeout_at` alone polls first, allowing a ready write after expiry.
pub async fn bounded<F: std::future::Future>(
    deadline: Instant,
    cancel: &CancellationToken,
    operation_id: Option<Uuid>,
    future: F,
) -> Result<F::Output, Stop> {
    if cancel.is_cancelled() {
        return Err(Stop::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(Stop::Deadline(operation_id));
    }
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(Stop::Cancelled),
        () = sleep_until(deadline) => Err(Stop::Deadline(operation_id)),
        outcome = future => Ok(outcome),
    }
}

/// Why a call stopped waiting, as opposed to what the registry answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// The caller's cancellation token fired.
    Cancelled,
    /// The deadline passed, while the named operation was running if it was accepted.
    Deadline(Option<Uuid>),
}

impl Stop {
    /// The canonical error reporting the stop: accepted writes continue; a timeout names
    /// the operation, and cancellation requires replay with the caller's key.
    pub fn into_error(self) -> CanonicalError {
        match self {
            Self::Cancelled => OperationResource::cancelled().create(),
            Self::Deadline(Some(id)) => OperationResource::deadline_exceeded(format!(
                "the deadline passed while operation {id} was still running; it was not \
                 cancelled, and retrying with the same idempotency key replays it"
            ))
            .with_resource(id.to_string())
            .create(),
            Self::Deadline(None) => OperationResource::deadline_exceeded(
                "the deadline passed before the registry acknowledged the submission; retry \
                 with the same idempotency key to learn its outcome",
            )
            .create(),
        }
    }
}
