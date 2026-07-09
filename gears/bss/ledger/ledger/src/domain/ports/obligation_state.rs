//! Obligation-satisfaction run-gating port (ASC 606, design §4.3 — Slice 4
//! Group E4).
//!
//! A recognition run keys on whether the schedule's **performance obligation**
//! is satisfied — **not** on collections/dunning (a collections-suspended payer
//! keeps recognizing; revenue is earned regardless of collection; only an
//! upstream *cancellation* stops the schedule, PRD L680–681). The
//! [`ObligationStateResolver`] is the outbound port the
//! [`RecognitionRunner`](crate::infra::recognition::runner) consults before it
//! releases a segment: a satisfied obligation may proceed; a NOT-satisfied one
//! is skipped (delayed — never released early).
//!
//! **v1 default = proceed.** There is no Subscriptions obligation-satisfaction
//! feed in v1 (the consumer is deferred to Slice 7 / VHP-1859), and the design
//! is explicit that **revenue is earned regardless of collections** — so the v1
//! default [`AlwaysSatisfiedObligationState`] returns
//! [`ObligationState::Satisfied`] for every schedule and recognition actually
//! runs. This is deliberately the opposite of a no-op that would *stall*
//! recognition: with no feed, the safe-for-revenue behaviour is to proceed.
//!
//! Concretely, **v1 recognition is calendar/time-based** (a segment releases on
//! its planned period). **Obligation-gated** recognition — where a release WAITS
//! on the Subscriptions performance-satisfaction signal — is itself the Slice 7
//! capability (it needs the feed), so v1 has no obligation-gated segment that
//! `proceed` could release early; applying the fail-safe `NotSatisfied` here
//! instead would simply stall ALL (time-based) v1 recognition, which is wrong.
//!
//! **Fail-safe contract for the real feed (Slice 7).** Once the Subscriptions
//! feed lands, the resolver becomes the eventually-consistent reader of that
//! state, and the contract flips to fail-safe: an **unknown or stale** state is
//! treated as [`ObligationState::NotSatisfied`] — recognition is delayed (and
//! surfaced at the Slice 7 close gate via undone-due-segment blocking), never
//! released early. A stale "satisfied" can only ever release up to
//! `total_deferred_minor` (the per-schedule cap CHECK caps it). The runner only
//! ever asks "may I release this segment now?" via [`Self::is_satisfied`], so
//! swapping in the real reader needs no runner change. Mirrors the
//! [`LedgerMetricsPort`](super::metrics::LedgerMetricsPort) /
//! [`NoopLedgerMetrics`](super::metrics::NoopLedgerMetrics) port + safe-default
//! shape.

use toolkit_macros::domain_model;
use uuid::Uuid;

/// The performance-obligation context a recognition release draws against — the
/// minimum the resolver needs to look up the obligation's satisfaction state.
/// Carries the schedule identity, the seller tenant, and the schedule's
/// `subscription_ref` when it is subscription-scoped (`None` for a non-
/// subscription schedule — e.g. a one-time deferred line). PII-free by
/// construction (ledger identities + an opaque subscription ref only).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObligationContext {
    /// The seller tenant whose ledger owns the schedule.
    pub tenant_id: Uuid,
    /// The schedule whose segment is up for release.
    pub schedule_id: String,
    /// The schedule's `subscription_ref` when subscription-scoped; `None`
    /// otherwise (the obligation is not tracked by a Subscriptions entitlement).
    pub subscription_ref: Option<String>,
}

/// The resolved obligation-satisfaction state for one [`ObligationContext`].
/// `Satisfied` ⇒ recognition may proceed; `NotSatisfied` ⇒ delay (do not
/// release). The real Slice 7 reader folds *unknown / stale* into
/// `NotSatisfied` (the fail-safe contract); v1's default never reports
/// `NotSatisfied` (no feed ⇒ proceed).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObligationState {
    /// The obligation is satisfied (or, in v1, presumed satisfied — no feed):
    /// recognition may release the segment.
    Satisfied,
    /// The obligation is not (yet) satisfied — or unknown/stale under the real
    /// feed's fail-safe rule: the release is delayed, never made early.
    NotSatisfied,
}

impl ObligationState {
    /// Whether a release may proceed for this state (`Satisfied` ⇒ `true`).
    #[must_use]
    pub const fn may_recognize(self) -> bool {
        matches!(self, Self::Satisfied)
    }
}

/// Outbound port: resolve whether a schedule's performance obligation is
/// satisfied, so the [`RecognitionRunner`](crate::infra::recognition::runner)
/// can gate a release on obligation-satisfaction (not collections). The real
/// adapter (Slice 7) reads the eventually-consistent Subscriptions feed and
/// applies the fail-safe rule; [`AlwaysSatisfiedObligationState`] is the v1
/// default (no feed ⇒ proceed).
#[async_trait::async_trait]
pub trait ObligationStateResolver: Send + Sync + 'static {
    /// Resolve the obligation-satisfaction state for `ctx`. v1 always returns
    /// [`ObligationState::Satisfied`]; the real reader returns the latest known
    /// state and folds unknown/stale into [`ObligationState::NotSatisfied`].
    async fn resolve(&self, ctx: &ObligationContext) -> ObligationState;

    /// Convenience: `true` iff the obligation may be recognized now. Default
    /// impl over [`Self::resolve`] — adapters need only implement `resolve`.
    async fn is_satisfied(&self, ctx: &ObligationContext) -> bool {
        self.resolve(ctx).await.may_recognize()
    }
}

/// The v1 default resolver: every obligation is treated as satisfied, so
/// recognition runs (design §4.3 — revenue is earned regardless of collections,
/// and there is no Subscriptions feed in v1). Replaced by the real fail-safe
/// reader when the Subscriptions obligation-satisfaction feed lands (Slice 7 /
/// VHP-1859). Mirrors [`NoopLedgerMetrics`](super::metrics::NoopLedgerMetrics):
/// a zero-state safe default wired in until the real adapter exists.
#[domain_model]
#[derive(Debug, Default, Clone, Copy)]
pub struct AlwaysSatisfiedObligationState;

#[async_trait::async_trait]
impl ObligationStateResolver for AlwaysSatisfiedObligationState {
    async fn resolve(&self, _ctx: &ObligationContext) -> ObligationState {
        // No Subscriptions feed in v1 → proceed (revenue is earned regardless of
        // collections). The fail-safe unknown/stale → NotSatisfied rule applies
        // only once the real feed lands (Slice 7).
        ObligationState::Satisfied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ObligationContext {
        ObligationContext {
            tenant_id: Uuid::from_u128(1),
            schedule_id: "sched-1".to_owned(),
            subscription_ref: Some("sub-1".to_owned()),
        }
    }

    #[test]
    fn may_recognize_is_true_only_when_satisfied() {
        assert!(ObligationState::Satisfied.may_recognize());
        assert!(!ObligationState::NotSatisfied.may_recognize());
    }

    #[tokio::test]
    async fn v1_default_always_proceeds() {
        let resolver = AlwaysSatisfiedObligationState;
        assert_eq!(resolver.resolve(&ctx()).await, ObligationState::Satisfied);
        assert!(resolver.is_satisfied(&ctx()).await);
    }
}
