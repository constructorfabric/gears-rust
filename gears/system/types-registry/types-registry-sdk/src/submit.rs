//! Submit-and-poll under a deadline and cancellation; reconciliation's submit step.

use std::time::Duration;

use tokio_util::sync::CancellationToken;
use toolkit::tokio::time::{Instant, sleep_until};
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

/// Submit and poll under one deadline; reconciliation's submit step.
pub async fn await_registration<A: PlatformTypesRegistryApi + ?Sized>(
    api: &A,
    ctx: &PlatformSecurityContext,
    key: IdempotencyKey,
    request: RegisterEntitiesRequest,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<RegistrationOperation, CanonicalError> {
    let mut operation = bounded(
        deadline,
        cancel,
        None,
        api.register_entities(ctx, key, request),
    )
    .await??;
    let operation_id = operation.operation_id;
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
        let polled = bounded(
            deadline,
            cancel,
            Some(operation_id),
            api.get_operation(ctx, operation_id),
        )
        .await??;
        let crate::models::Operation::Registration(polled) = polled else {
            return Err(OperationResource::unknown(format!(
                "the registry answered a poll of registration operation {operation_id} \
                 with a deletion"
            ))
            .with_resource(operation_id.to_string())
            .create());
        };
        operation = polled;
    }
    Ok(operation)
}

/// Equal jitter in [backoff/2, backoff] spreads concurrent retries.
pub fn jittered(backoff: Duration) -> Duration {
    use rand::RngExt as _;
    let half = backoff / 2;
    let spread = u64::try_from(half.as_nanos()).unwrap_or(u64::MAX);
    half + Duration::from_nanos(rand::rng().random_range(0..=spread))
}

/// `now + budget`, or `InvalidArgument` for a budget no clock can represent.
pub fn deadline_from_now(budget: Duration) -> Result<Instant, CanonicalError> {
    Instant::now().checked_add(budget).ok_or_else(|| {
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
) -> Result<F::Output, CanonicalError> {
    if cancel.is_cancelled() {
        return Err(stopped(operation_id, StopCause::Cancelled));
    }
    if Instant::now() >= deadline {
        return Err(stopped(operation_id, StopCause::Deadline));
    }
    toolkit::tokio::select! {
        biased;
        () = cancel.cancelled() => Err(stopped(operation_id, StopCause::Cancelled)),
        () = sleep_until(deadline) => Err(stopped(operation_id, StopCause::Deadline)),
        outcome = future => Ok(outcome),
    }
}

enum StopCause {
    Cancelled,
    Deadline,
}

/// Stop waiting while accepted writes continue; timeout names the operation, cancellation
/// requires replay with the caller’s key.
fn stopped(operation_id: Option<Uuid>, cause: StopCause) -> CanonicalError {
    match (cause, operation_id) {
        (StopCause::Cancelled, _) => OperationResource::cancelled().create(),
        (StopCause::Deadline, Some(id)) => OperationResource::deadline_exceeded(format!(
            "the deadline passed while operation {id} was still running; it was not \
             cancelled, and retrying with the same idempotency key replays it"
        ))
        .with_resource(id.to_string())
        .create(),
        (StopCause::Deadline, None) => OperationResource::deadline_exceeded(
            "the deadline passed before the registry acknowledged the submission; retry \
             with the same idempotency key to learn its outcome",
        )
        .create(),
    }
}
