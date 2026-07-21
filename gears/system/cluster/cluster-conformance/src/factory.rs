// Created: 2026-07-15 by Constructor Tech
//! [`ScenarioBackend`] — an async-built backend plus its teardown, the unit a
//! `run_*_conformance` runner drives one scenario against.
//!
//! The runners are generic over an **async** factory `Fn() -> Future<Output =
//! ScenarioBackend<_>>` so a backend whose construction is genuinely async
//! (opening a pool, running migrations, starting a container) can be built
//! fresh per scenario — an in-memory fixture just wraps a ready value. Each
//! backend is paired with a [`Teardown`] the runner awaits once the scenario
//! finishes, so a backend that needs an explicit async shutdown (e.g. a plugin
//! handle whose `Drop` guard panics if `stop()` was skipped) is released
//! deterministically between scenarios rather than leaked.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// The async teardown a runner awaits after a scenario. `Send` so the runner
/// future stays `Send` for `multi_thread` callers; a no-op for fixtures.
pub type Teardown = Pin<Box<dyn Future<Output = ()> + Send>>;

/// A backend built for one conformance scenario, plus the teardown to run once
/// the scenario completes.
pub struct ScenarioBackend<T: ?Sized> {
    /// The backend the scenario exercises.
    pub backend: Arc<T>,
    /// Awaited by the runner after the scenario — closes pools / stops handles /
    /// drops containers. [`ScenarioBackend::bare`] makes this a no-op.
    pub teardown: Teardown,
}

impl<T: ?Sized> ScenarioBackend<T> {
    /// A backend that needs no teardown (an in-memory fixture: dropping the
    /// `Arc` is sufficient).
    #[must_use]
    pub fn bare(backend: Arc<T>) -> Self {
        Self {
            backend,
            teardown: Box::pin(std::future::ready(())),
        }
    }

    /// A backend with an async `teardown` (e.g. `async move { handle.stop().await }`),
    /// awaited by the runner once the scenario returns.
    #[must_use]
    pub fn with_teardown<F>(backend: Arc<T>, teardown: F) -> Self
    where
        F: Future<Output = ()> + Send + 'static,
    {
        Self {
            backend,
            teardown: Box::pin(teardown),
        }
    }
}

/// Brackets one scenario: awaits the factory to build the backend, runs `run`
/// against it, then awaits the teardown. The single place every runner threads
/// a `ScenarioBackend` through, so the build → run → teardown order is
/// consistent across all suites.
///
/// The scenario runs behind an async panic boundary so `teardown` is awaited
/// **even when a scenario assertion panics** (`AssertUnwindSafe` +
/// [`FutureExt::catch_unwind`](futures::FutureExt::catch_unwind)). Without it, a
/// panic would drop the `teardown` future un-awaited; a teardown owning a handle
/// with a panic-on-drop guard (e.g. `PostgresClusterHandle`) would then panic
/// during unwind and abort the whole test process, masking the original failure.
/// The captured panic is re-raised after teardown so the assertion failure still
/// surfaces normally.
pub(crate) async fn run_scenario<T, Fut, S, SFut>(make_fut: Fut, run: S)
where
    T: ?Sized,
    Fut: Future<Output = ScenarioBackend<T>>,
    S: FnOnce(Arc<T>) -> SFut,
    SFut: Future<Output = ()>,
{
    use futures::FutureExt;

    let scenario_backend = make_fut.await;
    let outcome = std::panic::AssertUnwindSafe(run(scenario_backend.backend))
        .catch_unwind()
        .await;
    scenario_backend.teardown.await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
