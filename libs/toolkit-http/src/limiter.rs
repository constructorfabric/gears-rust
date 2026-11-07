//! A concurrency cap that can be shared between clients.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tower::{Layer, Service};

use crate::error::HttpError;

/// A cap on in-flight requests that the caller owns and may share.
///
/// Install it with
/// [`HttpClientBuilder::concurrency_limiter`](crate::HttpClientBuilder::concurrency_limiter)
/// on one or more clients, and take slots with [`try_acquire`](Self::try_acquire)
/// for requests sent through another engine: all of them then draw on one
/// budget. Like the built-in limit, a request holds its slot until its
/// response head arrives, and a request beyond the cap fails fast with
/// [`HttpError::Overloaded`].
#[derive(Debug, Clone)]
pub struct ConcurrencyLimiter {
    semaphore: Arc<Semaphore>,
}

impl ConcurrencyLimiter {
    /// A limiter admitting `max_concurrent_requests` at once (`0` is clamped
    /// to `1`, so it can never shed every request).
    #[must_use]
    pub fn new(max_concurrent_requests: usize) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(max_concurrent_requests.max(1))),
        }
    }

    /// Take a slot for a request sent outside an [`HttpClient`](crate::HttpClient).
    /// Release it by dropping the returned slot once the response head
    /// arrived or the request failed.
    ///
    /// # Errors
    /// [`HttpError::Overloaded`] when every slot is taken.
    pub fn try_acquire(&self) -> Result<ConcurrencySlot, HttpError> {
        Arc::clone(&self.semaphore)
            .try_acquire_owned()
            .map(|permit| ConcurrencySlot { _permit: permit })
            .map_err(|_| HttpError::Overloaded)
    }
}

/// One taken slot of a [`ConcurrencyLimiter`], released on drop.
#[derive(Debug)]
pub struct ConcurrencySlot {
    _permit: OwnedSemaphorePermit,
}

/// Fails a request fast with [`HttpError::Overloaded`] when the limiter has
/// no free slot, and otherwise holds a slot until the inner response future
/// completes (the response head).
///
/// Takes the slot with `try_acquire` in `call` rather than waiting for one in
/// `poll_ready`: a `LoadShed` + `ConcurrencyLimit` pair leaves the shed
/// request's semaphore acquisition queued, and the next released slot is
/// reserved for it, which starves other users of a shared limiter.
#[derive(Clone)]
pub struct LimitLayer {
    limiter: ConcurrencyLimiter,
}

impl LimitLayer {
    /// A layer drawing on `limiter`.
    pub fn new(limiter: ConcurrencyLimiter) -> Self {
        Self { limiter }
    }
}

impl<S> Layer<S> for LimitLayer {
    type Service = LimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        LimitService {
            inner,
            limiter: self.limiter.clone(),
        }
    }
}

/// The service [`LimitLayer`] wraps around `S`.
#[derive(Clone)]
pub struct LimitService<S> {
    inner: S,
    limiter: ConcurrencyLimiter,
}

impl<S, R> Service<R> for LimitService<S>
where
    S: Service<R, Error = HttpError>,
    S::Future: Send + 'static,
    S::Response: Send + 'static,
{
    type Response = S::Response;
    type Error = HttpError;
    type Future = Pin<Box<dyn Future<Output = Result<S::Response, HttpError>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), HttpError>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: R) -> Self::Future {
        let slot = match self.limiter.try_acquire() {
            Ok(slot) => slot,
            Err(err) => return Box::pin(std::future::ready(Err(err))),
        };
        let response = self.inner.call(req);
        Box::pin(async move {
            let result = response.await;
            drop(slot);
            result
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn slots_are_released_on_drop() {
        let limiter = ConcurrencyLimiter::new(1);
        let slot = limiter.try_acquire().unwrap();
        assert!(matches!(limiter.try_acquire(), Err(HttpError::Overloaded)));
        drop(slot);
        limiter.try_acquire().unwrap();
    }

    #[test]
    fn zero_is_clamped_to_one() {
        let limiter = ConcurrencyLimiter::new(0);
        limiter.try_acquire().unwrap();
    }
}
