//! Test-only holds at the statements of the activation repository: a test parks
//! a writer between the read its decision is taken on and its guarded
//! statement, or between two statements of one transaction, so a race of
//! DESIGN-activation §7 *Concurrency Testing* is ordered by the test and not by
//! timing. Compiled for tests and the `integration` feature only; the
//! production build has neither the registry nor the calls.
//!
//! A hold is keyed on the repository step and the setting key, so tests of one
//! process, each on its own key, never meet each other's holds. The first
//! `to_park` arrivals at the step park until the test releases; every later
//! arrival, and every arrival after the release, goes straight through.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::{Notify, Semaphore};

/// Where in the repository a writer can be held, always *before* the statement
/// the step names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The guarded transition of a report, after the report's reads.
    Report,
    /// The guarded re-arm of a `silent` state, by a report or a registration.
    Rearm,
    /// The guarded call of silence on an `awaiting` state.
    CallSilent,
    /// The clear of a subscription's `silent_since`, by a report or a
    /// registration: the last statement of the report.
    ClearSilent,
    /// The insert or the refresh of a registration's subscription, before the
    /// registration looks its subscription up.
    Register,
    /// The refresh of a registration's existing subscription: after the
    /// lookup found it, before its label is written.
    RegisterRefresh,
    /// The `silent` record of a call of silence: after the state's and the
    /// subscription's statements, before the record — the call's writes made
    /// and not yet committed.
    SilentRecord,
    /// The forgetting's first statement, the delete of the instance's state.
    Forget,
    /// The forgetting's second statement, the delete of the subscription,
    /// after the state's delete.
    ForgetSubscription,
}

/// The permits a release adds: more than any test parks.
const RELEASED: usize = 1 << 20;

/// One hold: signals that a writer arrived and parks it until released.
pub struct Hold {
    arrived: Notify,
    gate: Semaphore,
    to_park: AtomicUsize,
    arrivals: AtomicUsize,
}

impl Hold {
    /// Wait until a writer has reached the hold (parked or not).
    pub async fn arrived(&self) {
        self.arrived.notified().await;
    }

    /// How many writers reached the hold.
    #[must_use]
    pub fn arrivals(&self) -> usize {
        self.arrivals.load(Ordering::SeqCst)
    }

    /// Let the parked writers go on, and every later arrival by.
    pub fn release(&self) {
        self.gate.add_permits(RELEASED);
    }
}

type Holds = Vec<(Step, String, Arc<Hold>)>;

static HOLDS: Mutex<Holds> = Mutex::new(Vec::new());

fn holds() -> std::sync::MutexGuard<'static, Holds> {
    HOLDS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Hold the first `to_park` writers reaching `step` for `key`.
#[must_use]
pub fn hold(step: Step, key: &str, to_park: usize) -> Arc<Hold> {
    let hold = Arc::new(Hold {
        arrived: Notify::new(),
        gate: Semaphore::new(0),
        to_park: AtomicUsize::new(to_park),
        arrivals: AtomicUsize::new(0),
    });
    let mut all = holds();
    all.retain(|(s, k, _)| !(*s == step && k == key));
    all.push((step, key.to_owned(), Arc::clone(&hold)));
    hold
}

/// Called by the repository before the statement of `step`.
pub async fn reached(step: Step, key: &str) {
    let found = holds()
        .iter()
        .find(|(s, k, _)| *s == step && k == key)
        .map(|(_, _, hold)| Arc::clone(hold));
    let Some(hold) = found else {
        return;
    };
    hold.arrivals.fetch_add(1, Ordering::SeqCst);
    let parks = hold
        .to_park
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_ok();
    hold.arrived.notify_one();
    if parks && let Ok(permit) = hold.gate.acquire().await {
        permit.forget();
    }
}
