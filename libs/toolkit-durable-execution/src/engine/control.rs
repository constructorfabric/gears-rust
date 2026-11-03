//! The periodic control loop shared by the metadata-only controller and the
//! delivery dispatcher: one tick, retry and error triage policy for both.
use std::{future::Future, time::Duration};

use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::engine::retry::{RetryPolicy, wait_retry};
use crate::store::StoreError;

/// What the loop does after a failed tick.
#[derive(Debug, PartialEq, Eq)]
pub enum Triage {
    /// Authorization is missing or denied; wait for the next tick without counting
    /// a failure. Work resumes once the authorizer grants access again.
    Pause,
    /// One run or definition is broken or raced; skip it, the rest continues.
    Skip,
    /// Storage failed; back off and count towards the failure limit.
    Retry,
}

/// Classify a failed tick and log it. Logs carry the error's display text only,
/// never authorization sources.
pub fn triage(error: &StoreError) -> Triage {
    match error {
        StoreError::AuthorizationUnavailable
        | StoreError::Authorization(_)
        | StoreError::Forbidden => {
            tracing::warn!(error = %error, "durable control plane paused: authorization unavailable");
            Triage::Pause
        }
        StoreError::Conflict => Triage::Skip,
        StoreError::Invariant(_) | StoreError::Domain(_) => {
            tracing::error!(error = %error, "durable control plane skipped a broken run");
            Triage::Skip
        }
        _ => Triage::Retry,
    }
}

/// Run `tick` every interval until `stop` fires.
///
/// # Errors
/// The last storage error once retries are exhausted, or the first error that is
/// neither paused, skipped nor retryable.
pub async fn control_loop<F, Fut>(
    config: &Config,
    missed: MissedTickBehavior,
    stop: &CancellationToken,
    mut tick: F,
) -> Result<(), StoreError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), StoreError>>,
{
    let mut interval = tokio::time::interval(Duration::from_secs(config.dispatch_interval_secs));
    interval.set_missed_tick_behavior(missed);
    let mut retry = RetryPolicy::new(config);
    loop {
        tokio::select! {
            biased;
            () = stop.cancelled() => return Ok(()),
            _ = interval.tick() => {}
        }
        // cancel-safe: every tick writes through compare-and-swap transactions, so a
        // dropped tick leaves committed rows consistent and the next tick resumes.
        let result = tokio::select! {
            biased;
            () = stop.cancelled() => return Ok(()),
            result = tick() => result,
        };
        let Err(error) = result else {
            retry.reset();
            continue;
        };
        match triage(&error) {
            Triage::Pause | Triage::Skip => {}
            Triage::Retry => {
                let Some(delay) = retry.retry_delay(&error) else {
                    return Err(error);
                };
                if !wait_retry(delay, stop).await {
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/control_tests.rs"]
mod tests;
