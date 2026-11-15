//! The refresh of an open stream's consume scope, off the delivery path.
//!
//! A stream open stores the member's consume scope, one window per interest
//! group, and spawns one worker for the stream. The worker re-evaluates each
//! group one refresh lead ([`StreamingConfig::refresh_lead`]) before the group
//! expires, as the opener, and stores the result with a compare-and-swap on the
//! generation it read. The session only reads the coordinator's watch of the
//! scope, so delivery never waits on a refresh, and it ends the stream itself
//! once the scope's earliest group expiry passes, so a worker that died or
//! fell behind never extends a window. The session holds the worker's
//! cancellation guard in its [`ScopeFeed`], so dropping the stream stops the
//! worker: a member with no open stream has no worker, and its next stream open
//! evaluates any expired group itself.
//!
//! A pass starts each group's renewal once the group is due, earliest
//! expiry first, with at most [`REFRESH_CONCURRENCY`] running at once; a
//! due group past that waits for a free slot. The pass reads the
//! clock again after every wait, and waits no later than the scope's
//! earliest group expiry, taken again after every store. A scope holding a
//! group at or past its expiry (its retries ran out, or its renewal did not
//! end in time) is cleared there, which tears the stream down. A renewal is
//! the candidate query, then I for an `ignore` group, then E, each PDP call
//! under the delivery enforcer's deadline. A memo entry is reused only when it
//! still serves one refresh lead past the renewal's start
//! ([`MemoUse::OutlivingPast`]), so a refresh never reuses the answer it is
//! replacing, and the group moves into a new window of
//! `authz_scope_ttl_secs` from that start (no stagger after the first
//! window). What a renewal finds decides its group:
//!
//! - allowed: the group gets the new scope, candidates and expiry, stored at
//!   once with a compare-and-swap on the generation last read, so the session
//!   serves it while other renewals still run; every other group keeps its
//!   entry as it is, so the session keeps its decisions;
//! - denied, not compilable or nothing enforceable, on I or E: the scope is
//!   cleared at once, which tears the stream down, and the other renewals
//!   are dropped;
//! - a failed evaluation or the deadline, on I or E: the group is tried again
//!   1 s after the failure, doubling up to 5 s, never later than its expiry,
//!   while the current scope serves;
//! - a failed candidate query: the group keeps its candidates, marked stale,
//!   and E is asked with the candidate list the worker last had. With no list
//!   yet (the open reused a held scope and the group has not been refreshed
//!   since), the group is tried again as after a failed evaluation.
//!
//! The refresh keeps up while a renewal takes at most
//! `REFRESH_CONCURRENCY x authz_scope_ttl_secs / groups`: a stream open
//! spaces the groups' expiries `authz_scope_ttl_secs / groups` apart, and
//! renewals that take longer queue past the cap until a group reaches its
//! expiry before its renewal ends, where the pass clears the scope and tears
//! the stream down; the client opens it again. At the default 60 s that is
//! 15 s a renewal for 16 groups and 3.75 s for 64; at the default 10 s
//! deadline, `respect` groups whose E takes the whole deadline keep up to 24
//! groups, and `ignore` groups whose I and E both do keep up to 12. A renewal
//! whose PDP calls each end within the deadline also ends within its group's
//! lead, which is sized to them with 2 s to spare for the candidate query.
//!
//! Groups due together (expiries that bunch, as a reused memo entry can cap
//! several at one instant) start [`REFRESH_CONCURRENCY`] at a time: with `k`
//! of them and renewals of `L`, the last starts after
//! `ceil(k / REFRESH_CONCURRENCY) - 1` renewals in turn, so they keep up
//! while `ceil(k / REFRESH_CONCURRENCY) x L` stays within their lead.
//! Starting the earliest expiry first keeps a group retrying near its expiry
//! from waiting behind groups that expire later.
//!
//! A write that loses to a newer scope ends the pass, dropping the renewals
//! still running, and the next pass reads that scope; a refused group whose
//! clear lost is asked again on that pass. A cancelled worker's pass writes
//! nothing and drops its renewals.
//!
//! [`ScopeFeed`]: crate::domain::streaming::session::ScopeFeed
//! [`MemoUse::OutlivingPast`]: crate::domain::delivery::MemoUse::OutlivingPast

use std::convert::Infallible;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use toolkit::domain_model;
use toolkit_security::SecurityContext;
use toolkit_taskward::{Directive, WorkerAction};
use tracing::Instrument;
use uuid::Uuid;

use crate::config::StreamingConfig;
use crate::domain::consumer_group_coordinator::ConsumerGroupCoordinator;
use crate::domain::consumer_group_coordinator::member_scope::{
    AuthorizedGroup, CachedScope, Expected, ScopeContent, ScopePrincipal, ScopeWrite,
    ScopeWriteOutcome,
};
use crate::domain::delivery::{ConsumeEvaluator, MemoUse, Renewal};
use crate::domain::error::DomainError;
use crate::domain::join_scope::InterestGroup;
use crate::domain::model::Subscription;
use crate::domain::specification::EventTypeCandidates;

/// The wait before the first retry of a group whose evaluation failed.
const FIRST_RETRY: Duration = Duration::from_secs(1);

/// The longest wait between two retries of a group; each retry doubles the
/// previous wait up to it.
const MAX_RETRY: Duration = Duration::from_secs(5);

/// The most group renewals one stream's refresh runs at once.
///
/// It bounds how slow a PDP the refresh keeps up with: a renewal may take up
/// to `REFRESH_CONCURRENCY x authz_scope_ttl_secs / groups` (see the module
/// doc), and a slower one clears the scope at a group's expiry, which tears
/// the stream down.
///
/// Per stream; with [`SINGLE_TYPE_CONCURRENCY`] it stays below the gear
/// database pool's default size (10: sqlx's `max_connections`, which no gear
/// config overrides). Each renewal holds a pool connection only for its one
/// candidate read, released before its PDP calls, so one stream's refresh
/// and read batches hold at most 9 connections at once, and many streams
/// queue on the pool for one short read each, as publish's own reads do. The
/// test harness's SQLite pool holds 4, where those reads queue too.
///
/// [`SINGLE_TYPE_CONCURRENCY`]: crate::domain::streaming::scoped_filter::SINGLE_TYPE_CONCURRENCY
const REFRESH_CONCURRENCY: usize = 4;

// A pass starts a due group only while fewer than `REFRESH_CONCURRENCY`
// renewals run: with none allowed, no group is ever renewed and the worker
// runs empty passes without end.
const _: () = assert!(REFRESH_CONCURRENCY >= 1);

/// One open stream's consume scope refresh: a `toolkit-taskward` action whose
/// pass re-evaluates the groups that are due and says when the next one is.
#[domain_model]
pub(crate) struct ScopeRefresh {
    subscription_id: Uuid,
    /// The opener, as whom every refresh evaluates: the session fails closed
    /// on a scope stored for another caller.
    ctx: Arc<SecurityContext>,
    principal: ScopePrincipal,
    coordinator: Arc<ConsumerGroupCoordinator>,
    evaluator: Arc<ConsumeEvaluator>,
    streaming: StreamingConfig,
    /// One per interest group, in the order the member scope holds them.
    refreshing: Vec<Refreshing>,
    clock: Clock,
}

/// What the worker keeps about one interest group between passes.
struct Refreshing {
    group: InterestGroup,
    /// The candidate list the group was last evaluated with, so a failed
    /// candidate query asks E as before; `None` until the worker has one.
    /// Shared with the group each renewal evaluates, never copied. Bound: one
    /// list per group (at most 64), of at most `EventTypeCandidates::CAP`
    /// (256) types, each with the subject-type patterns it allows.
    listed: Option<Arc<EventTypeCandidates>>,
    /// The pending retry after a failed evaluation, or after a refusal whose
    /// clear lost to a newer scope; `None` once an evaluation succeeds.
    retry: Option<Retry>,
}

/// When a group is tried again, and the wait after a failure that got it
/// there (zero when no failure did).
#[derive(Debug, Clone, Copy)]
struct Retry {
    at: Instant,
    backoff: Duration,
}

/// One group's renewal as its task runs it, holding what it reads.
struct Turn {
    /// The group's position in the member scope.
    k: usize,
    group: InterestGroup,
    /// The candidate list the worker had for the group when the turn began.
    listed: Option<Arc<EventTypeCandidates>>,
    /// The group's entry in the scope read when the turn began.
    current: AuthorizedGroup,
    /// When the turn began, on the action's clock.
    start: Instant,
    lead: Duration,
    clock: Clock,
    evaluator: Arc<ConsumeEvaluator>,
    ctx: Arc<SecurityContext>,
    /// The pass's cancel, which the turn checks itself: a renewal can run on
    /// after the cancel before the pass is polled and drops it.
    cancel: CancellationToken,
}

/// What a turn hands back to its pass.
struct TurnEnd {
    k: usize,
    /// The candidate list the turn's query read; `None` when it failed.
    fresh: Option<Arc<EventTypeCandidates>>,
    step: Step,
}

/// What one group's turn came to.
enum Step {
    Renewed(AuthorizedGroup),
    Revoked,
    /// The evaluation failed, or the candidate query did with no list yet,
    /// at this instant.
    Failed(Instant),
    /// The pass was cancelled before the turn took its last answer; the turn
    /// asks the PDP nothing once it sees the cancel.
    Cancelled,
}

/// Where a pass reads the time.
#[derive(Debug, Clone, Copy)]
enum Clock {
    Tokio,
    /// A fixed instant, so a test can make a group due, or its retry or its
    /// expiry come, without waiting, and assert exact durations.
    #[cfg(test)]
    Pinned(Instant),
}

/// The subscriptions of the refresh actions that exist, one entry per action,
/// so a test can see a worker end.
#[cfg(test)]
static LIVE: std::sync::Mutex<Vec<Uuid>> = std::sync::Mutex::new(Vec::new());

impl ScopeRefresh {
    /// The refresh of `subscription`'s scope for the stream `ctx` opened.
    /// `listed` holds the candidate lists the open evaluated each group with,
    /// in interest-group order, and is empty when it reused the held scope.
    ///
    /// # Errors
    ///
    /// What [`InterestGroup::form`] refuses; JOIN ran the same check on the
    /// same interests, so only a broken internal invariant reaches it.
    pub(crate) fn new(
        subscription: &Subscription,
        ctx: SecurityContext,
        listed: Vec<Arc<EventTypeCandidates>>,
        coordinator: Arc<ConsumerGroupCoordinator>,
        evaluator: Arc<ConsumeEvaluator>,
        streaming: &StreamingConfig,
    ) -> Result<Self, DomainError> {
        let mut listed = listed.into_iter();
        let refreshing = InterestGroup::form(&subscription.interests, ctx.subject_tenant_id())?
            .into_iter()
            .map(|group| Refreshing {
                group,
                listed: listed.next(),
                retry: None,
            })
            .collect();
        #[cfg(test)]
        Self::live().push(subscription.id);
        let principal = ScopePrincipal::from(&ctx);
        Ok(Self {
            subscription_id: subscription.id,
            ctx: Arc::new(ctx),
            principal,
            coordinator,
            evaluator,
            streaming: streaming.clone(),
            refreshing,
            clock: Clock::Tokio,
        })
    }

    /// One pass: reads the member's scope, starts each due group's renewal
    /// as a slot frees, stores each renewed group at once, and returns once
    /// no renewal runs and no group is due, saying when the next one is. A
    /// cancelled pass writes nothing and idles. Every return drops the
    /// renewals still running.
    async fn refresh(&mut self, cancel: &CancellationToken) -> Directive {
        // Cloned out under the coordinator lock; nothing here holds a lock
        // across an await, and each renewal owns what it reads.
        let Some(mut held) = self.coordinator.scope_of(self.subscription_id) else {
            // Gone or cleared: the session ends on its feed, and its drop
            // cancels this worker.
            return Directive::idle();
        };
        let mut running = JoinSet::new();
        // Whether each group's renewal is in `running`.
        let mut in_flight = vec![false; self.refreshing.len()];
        loop {
            // A pass run after the cancel writes nothing, not even a clear
            // that needs no wait.
            if cancel.is_cancelled() {
                return Directive::idle();
            }
            let now = self.clock.now();
            let Some(first) = held.groups.iter().min_by_key(|group| group.expires_at) else {
                return Directive::idle();
            };
            if first.expires_at <= now {
                return self.expire(first, &held);
            }
            let first_expiry = first.expires_at;
            self.start_due(&held, now, &mut running, &mut in_flight, cancel);
            if running.is_empty() {
                return self.next_pass(&held.groups);
            }
            // The wait ends by the scope's earliest expiry, on the action's
            // clock, so a PDP that answers late never keeps an expired group
            // serving.
            let wake = self
                .next_start(&held, now, &in_flight, running.len())
                .map_or(first_expiry, |next| next.min(first_expiry));
            // The pass progresses: `start_due` started every due group it
            // had a slot for, so either every slot is taken (no next start,
            // and `wake` is the first expiry, which the check above found
            // after `now`) or every group not running is next tried after
            // `now`. A wake at or before `now` would loop without waiting.
            debug_assert!(
                wake > now,
                "a pass waits past now: every slot is taken, or every idle group is due later"
            );
            tokio::select! {
                // The cancel first; nothing relies on it, as `finish` checks
                // the cancel again before any write, so a renewal that ends
                // at the same poll writes nothing whichever arm is taken.
                biased;
                () = cancel.cancelled() => return Directive::idle(),
                Some(joined) = running.join_next() => {
                    let end = match joined {
                        Ok(end) => end,
                        // A renewal that panicked fails the worker, as one
                        // run on the pass itself would; the session still
                        // ends at the expiry on its own.
                        Err(error) => match error.try_into_panic() {
                            Ok(panic) => std::panic::resume_unwind(panic),
                            // The pass never aborts one renewal alone; a
                            // runtime shutting down cancels them all, and
                            // the worker with them, so the directive is not
                            // acted on. A fresh pass would start over.
                            Err(_cancelled) => return Directive::proceed(),
                        },
                    };
                    if let Some(running) = in_flight.get_mut(end.k) {
                        *running = false;
                    }
                    if let ControlFlow::Break(directive) = self.finish(end, &mut held, cancel) {
                        return directive;
                    }
                }
                () = tokio::time::sleep(wake.saturating_duration_since(now)) => {}
            }
        }
    }

    /// Starts the renewal of each group of `held` due at `now` and not
    /// running, earliest expiry first (in group order on a tie), while fewer
    /// than [`REFRESH_CONCURRENCY`] run.
    fn start_due(
        &self,
        held: &CachedScope,
        now: Instant,
        running: &mut JoinSet<TurnEnd>,
        in_flight: &mut [bool],
        cancel: &CancellationToken,
    ) {
        let mut due: Vec<_> = held
            .groups
            .iter()
            .zip(&self.refreshing)
            .zip(in_flight.iter())
            .enumerate()
            .filter_map(|(k, ((current, refreshing), runs))| {
                let lead = self.streaming.refresh_lead(current.key.barrier_mode);
                (!*runs && refreshing.next_attempt(current, lead, now) <= now)
                    .then_some((k, current, refreshing, lead))
            })
            .collect();
        // Stable, so groups expiring together keep their group order.
        due.sort_by_key(|(_, current, _, _)| current.expires_at);
        for (k, current, refreshing, lead) in due {
            if running.len() >= REFRESH_CONCURRENCY {
                return;
            }
            if let Some(runs) = in_flight.get_mut(k) {
                *runs = true;
            }
            let turn = Turn {
                k,
                group: refreshing.group.clone(),
                listed: refreshing.listed.as_ref().map(Arc::clone),
                current: current.clone(),
                start: now,
                lead,
                clock: self.clock,
                evaluator: Arc::clone(&self.evaluator),
                ctx: Arc::clone(&self.ctx),
                cancel: cancel.clone(),
            };
            // In the worker's span, so a renewal's warnings name its worker.
            running.spawn(turn.take().in_current_span());
        }
    }

    /// When the next group of `held` that is not running is due, while a
    /// slot is free for it; `None` when `running` renewals take every slot.
    fn next_start(
        &self,
        held: &CachedScope,
        now: Instant,
        in_flight: &[bool],
        running: usize,
    ) -> Option<Instant> {
        if running >= REFRESH_CONCURRENCY {
            return None;
        }
        held.groups
            .iter()
            .zip(&self.refreshing)
            .zip(in_flight)
            .filter(|(_, runs)| !**runs)
            .map(|((group, refreshing), _)| {
                let lead = self.streaming.refresh_lead(group.key.barrier_mode);
                refreshing.next_attempt(group, lead, now)
            })
            .min()
    }

    /// Takes what a turn found into the group's state and the member's
    /// scope: a renewed group is stored at once and `held` read again, a
    /// refusal clears the scope, a failure schedules the retry. Breaks with
    /// the pass's directive when the pass ends here. After `cancel`, a
    /// renewal or a refusal ends the pass without writing.
    fn finish(
        &mut self,
        end: TurnEnd,
        held: &mut Arc<CachedScope>,
        cancel: &CancellationToken,
    ) -> ControlFlow<Directive> {
        let TurnEnd { k, fresh, step } = end;
        if let (Some(fresh), Some(refreshing)) = (fresh, self.refreshing.get_mut(k)) {
            refreshing.listed = Some(fresh);
        }
        match step {
            Step::Renewed(group) => {
                if cancel.is_cancelled() {
                    return ControlFlow::Break(Directive::idle());
                }
                if let Some(refreshing) = self.refreshing.get_mut(k) {
                    refreshing.retry = None;
                }
                let mut groups = held.groups.to_vec();
                let Some(slot) = groups.get_mut(k) else {
                    return ControlFlow::Continue(());
                };
                *slot = group;
                let write = ScopeWrite::Store {
                    expected: Expected::Generation(held.generation),
                    content: ScopeContent {
                        principal: self.principal.clone(),
                        groups: groups.into(),
                    },
                };
                match self.coordinator.write_scope(self.subscription_id, write) {
                    ScopeWriteOutcome::Applied => {}
                    // A newer scope: this group's answer is dropped, and the
                    // next pass reads that scope and decides again.
                    ScopeWriteOutcome::Superseded => {
                        return ControlFlow::Break(Directive::proceed());
                    }
                    ScopeWriteOutcome::MemberGone => return ControlFlow::Break(Directive::idle()),
                }
                let Some(stored) = self.coordinator.scope_of(self.subscription_id) else {
                    return ControlFlow::Break(Directive::idle());
                };
                *held = stored;
                ControlFlow::Continue(())
            }
            Step::Revoked => {
                if cancel.is_cancelled() {
                    return ControlFlow::Break(Directive::idle());
                }
                if let Some(refused) = held.groups.get(k) {
                    tracing::warn!(
                        topic = %refused.key.topic,
                        "a consume scope group is no longer allowed; clearing the scope"
                    );
                }
                let directive = self.clear(held);
                // The clear lost to a newer scope, which may hold the group
                // again: the next pass asks it at once.
                if directive == Directive::proceed()
                    && let Some(refreshing) = self.refreshing.get_mut(k)
                {
                    refreshing.mark_due(self.clock.now());
                }
                ControlFlow::Break(directive)
            }
            Step::Failed(failed_at) => {
                if let (Some(refreshing), Some(current)) =
                    (self.refreshing.get_mut(k), held.groups.get(k))
                {
                    refreshing.fail(current, failed_at);
                }
                ControlFlow::Continue(())
            }
            // Reached through `refresh` only when the cancel lands between the
            // pass's select polling its cancel arm and its join arm (a
            // multi-thread runtime); it idles as a renewal or a refusal does
            // after the cancel.
            Step::Cancelled => ControlFlow::Break(Directive::idle()),
        }
    }

    /// Clears the member's scope over `expired`, a group of `held` at or
    /// past its expiry.
    fn expire(&self, expired: &AuthorizedGroup, held: &CachedScope) -> Directive {
        tracing::warn!(
            topic = %expired.key.topic,
            "a consume scope group expired before it was evaluated again; clearing the scope"
        );
        self.clear(held)
    }

    /// Clears the member's scope, expecting the generation `held` carries.
    fn clear(&self, held: &CachedScope) -> Directive {
        let write = ScopeWrite::Clear {
            expected: Expected::Generation(held.generation),
        };
        match self.coordinator.write_scope(self.subscription_id, write) {
            ScopeWriteOutcome::Applied | ScopeWriteOutcome::MemberGone => Directive::idle(),
            // A newer scope: the next pass reads it and decides again.
            ScopeWriteOutcome::Superseded => Directive::proceed(),
        }
    }

    /// A sleep until the earliest group refresh or retry among `groups`.
    fn next_pass(&self, groups: &[AuthorizedGroup]) -> Directive {
        let now = self.clock.now();
        let next = groups
            .iter()
            .zip(&self.refreshing)
            .map(|(group, refreshing)| {
                let lead = self.streaming.refresh_lead(group.key.barrier_mode);
                refreshing.next_attempt(group, lead, now)
            })
            .min();
        match next {
            Some(next) => Directive::sleep(next.saturating_duration_since(now)),
            None => Directive::idle(),
        }
    }

    /// Reads the time from `at` from now on.
    #[cfg(test)]
    pub(crate) fn pin_clock(&mut self, at: Instant) {
        self.clock = Clock::Pinned(at);
    }

    /// How many refresh actions for `subscription_id` exist.
    #[cfg(test)]
    pub(crate) fn live_for(subscription_id: Uuid) -> usize {
        Self::live()
            .iter()
            .filter(|live| **live == subscription_id)
            .count()
    }

    #[cfg(test)]
    fn live() -> std::sync::MutexGuard<'static, Vec<Uuid>> {
        LIVE.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
impl Drop for ScopeRefresh {
    fn drop(&mut self) {
        let mut live = Self::live();
        if let Some(index) = live.iter().position(|live| *live == self.subscription_id) {
            live.swap_remove(index);
        }
    }
}

impl WorkerAction for ScopeRefresh {
    type Payload = ();
    type Error = Infallible;

    /// Never an `Err`: the worker would go idle after one, and the action
    /// times its own retries.
    async fn execute(&mut self, cancel: &CancellationToken) -> Result<Directive, Infallible> {
        Ok(self.refresh(cancel).await)
    }
}

impl Refreshing {
    /// When the group is next tried: its pending retry, else one `lead`
    /// before `current` expires (`now` when that instant precedes the
    /// clock's origin).
    fn next_attempt(&self, current: &AuthorizedGroup, lead: Duration, now: Instant) -> Instant {
        match self.retry {
            Some(retry) => retry.at,
            None => current.expires_at.checked_sub(lead).unwrap_or(now),
        }
    }

    /// Schedules the next try after a failure at `failed_at`: the wait
    /// doubles from [`FIRST_RETRY`] up to [`MAX_RETRY`], and the try comes no
    /// later than the group's expiry, where the pass clears the scope.
    fn fail(&mut self, current: &AuthorizedGroup, failed_at: Instant) {
        let backoff = self.retry.map_or(FIRST_RETRY, |retry| {
            (retry.backoff * 2).clamp(FIRST_RETRY, MAX_RETRY)
        });
        self.retry = Some(Retry {
            at: (failed_at + backoff).min(current.expires_at),
            backoff,
        });
    }

    /// Makes the group due at `now`, keeping the wait of any failure before:
    /// a refusal whose clear lost is asked again against the newer scope.
    fn mark_due(&mut self, now: Instant) {
        self.retry = Some(Retry {
            at: now,
            backoff: self.retry.map_or(Duration::ZERO, |retry| retry.backoff),
        });
    }
}

impl Turn {
    /// Evaluates the group again from the turn's start: the candidate query,
    /// then the consume answers. The turn ends `Cancelled` once the pass's
    /// cancel has come: when the candidate query returns, or at once during
    /// its PDP calls, dropping the call in flight. A failure is timed from
    /// the turn's clock when it comes.
    async fn take(self) -> TurnEnd {
        let fresh = if let Ok(listed) = self.evaluator.get_candidates(self.group.topic()).await {
            Some(Arc::new(listed))
        } else {
            tracing::warn!(
                topic = %self.current.key.topic,
                "the candidate query failed at a consume scope refresh; \
                 keeping the previous candidates"
            );
            None
        };
        let step = tokio::select! {
            // The cancel first, so a cancel that came before the query
            // returned ends the turn before any PDP call, and a cancel that
            // comes with a PDP answer ends the turn before it takes the
            // answer.
            biased;
            () = self.cancel.cancelled() => Step::Cancelled,
            step = async {
                match fresh.as_ref().or(self.listed.as_ref()) {
                    Some(listed) => self.evaluate(Arc::clone(listed), fresh.is_none()).await,
                    None => Step::Failed(self.clock.now()),
                }
            } => step,
        };
        TurnEnd {
            k: self.k,
            fresh,
            step,
        }
    }

    /// The consume answers for the group with `listed`, its candidates kept
    /// as they were and marked stale when `stale`.
    async fn evaluate(&self, listed: Arc<EventTypeCandidates>, stale: bool) -> Step {
        let group = self.group.clone().with_candidates(listed);
        let memo_use = MemoUse::OutlivingPast(self.start + self.lead);
        match self
            .evaluator
            .renew_group(&self.ctx, &group, memo_use)
            .await
        {
            Renewal::Renewed { scope, remembered } => {
                // The window starts at the turn's start, before any answer
                // it holds was asked, so it never outlives one: an answer's
                // memo entry serves one TTL from when it came.
                let expires_at = remembered.cap(self.start + self.evaluator.ttl());
                let mut renewed = group.authorized(scope, expires_at);
                if stale {
                    renewed.candidates = Arc::clone(&self.current.candidates);
                    renewed.candidates_stale = true;
                }
                Step::Renewed(renewed)
            }
            Renewal::Revoked => Step::Revoked,
            Renewal::Unavailable => {
                tracing::warn!(
                    topic = %self.current.key.topic,
                    "a consume scope group could not be evaluated; trying again"
                );
                Step::Failed(self.clock.now())
            }
        }
    }
}

impl Clock {
    fn now(self) -> Instant {
        match self {
            Self::Tokio => Instant::now(),
            #[cfg(test)]
            Self::Pinned(at) => at,
        }
    }
}

#[cfg(test)]
#[path = "scope_refresh_tests.rs"]
mod scope_refresh_tests;
