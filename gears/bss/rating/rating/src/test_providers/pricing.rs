//! `FakePricingCatalog` (DESIGN 11 §4.12): `PricingReadV1` answering from a script. A test
//! scripts the answer to each query — bindings, refusals, uncovered cells, `ends_on` — and reads
//! back the calls made, so a feature can prove it resolved, or did not call Pricing at all. The
//! adapter tests of the pricing binding (feature 06) load pricing's golden contracts
//! (`pricing/tests/contract/*.json`) into it.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use bss_pricing_sdk::read::{
    ImmutablePrice, PlanQuery, PriceQuery, PricingReadV1, ResolveQuery, ResolvedBindings,
    RevisionRef,
};
use bss_rating_sdk::errors::UsageResource;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

/// One call the fake answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PricingCall {
    /// `resolve`.
    Resolve(ResolveQuery),
    /// `price`.
    Price(PriceQuery),
    /// `current_revision`.
    CurrentRevision(PlanQuery),
}

type Script<Q, A> = Vec<(Q, Result<A, CanonicalError>)>;

/// A scripted `PricingReadV1`. An unscripted query is `NotFound`.
#[derive(Default)]
pub struct FakePricingCatalog {
    resolves: Mutex<Script<ResolveQuery, ResolvedBindings>>,
    prices: Mutex<Script<PriceQuery, ImmutablePrice>>,
    revisions: Mutex<Script<PlanQuery, RevisionRef>>,
    calls: Mutex<Vec<PricingCall>>,
    enforce_tenant: AtomicBool,
}

impl FakePricingCatalog {
    /// A catalog with nothing scripted.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Answers `query` with `answer` (the latest script for a query wins).
    ///
    /// # Panics
    ///
    /// If the lock is poisoned.
    #[expect(
        clippy::expect_used,
        reason = "a test double: a poisoned lock means a test already panicked"
    )]
    pub fn on_resolve(
        &self,
        query: ResolveQuery,
        answer: Result<ResolvedBindings, CanonicalError>,
    ) {
        self.resolves
            .lock()
            .expect("fake pricing lock")
            .insert(0, (query, answer));
    }

    /// Answers `query` with `answer`.
    ///
    /// # Panics
    ///
    /// If the lock is poisoned.
    #[expect(
        clippy::expect_used,
        reason = "a test double: a poisoned lock means a test already panicked"
    )]
    pub fn on_price(&self, query: PriceQuery, answer: Result<ImmutablePrice, CanonicalError>) {
        self.prices
            .lock()
            .expect("fake pricing lock")
            .insert(0, (query, answer));
    }

    /// Answers `query` with `answer`.
    ///
    /// # Panics
    ///
    /// If the lock is poisoned.
    #[expect(
        clippy::expect_used,
        reason = "a test double: a poisoned lock means a test already panicked"
    )]
    pub fn on_current_revision(
        &self,
        query: PlanQuery,
        answer: Result<RevisionRef, CanonicalError>,
    ) {
        self.revisions
            .lock()
            .expect("fake pricing lock")
            .insert(0, (query, answer));
    }

    /// Refuses, as Pricing does (D-523: `TenantMode::RootOnly`), every query whose
    /// `CatalogRef.tenant_id` is not the caller's subject tenant, with `403 PermissionDenied`.
    pub fn enforce_catalog_tenant(&self) {
        self.enforce_tenant.store(true, Ordering::SeqCst);
    }

    fn refuse_foreign(
        &self,
        ctx: &SecurityContext,
        catalog_tenant: uuid::Uuid,
    ) -> Result<(), CanonicalError> {
        if self.enforce_tenant.load(Ordering::SeqCst) && ctx.subject_tenant_id() != catalog_tenant {
            return Err(UsageResource::permission_denied()
                .with_reason("PERMISSION_DENIED")
                .create());
        }
        Ok(())
    }

    /// Every call answered so far, in order.
    ///
    /// # Panics
    ///
    /// If the lock is poisoned.
    #[expect(
        clippy::expect_used,
        reason = "a test double: a poisoned lock means a test already panicked"
    )]
    #[must_use]
    pub fn calls(&self) -> Vec<PricingCall> {
        self.calls.lock().expect("fake pricing lock").clone()
    }

    #[expect(
        clippy::expect_used,
        reason = "a test double: a poisoned lock means a test already panicked"
    )]
    fn record(&self, call: PricingCall) {
        self.calls.lock().expect("fake pricing lock").push(call);
    }
}

#[expect(
    clippy::expect_used,
    reason = "a test double: a poisoned lock means a test already panicked"
)]
fn answer<Q: PartialEq, A: Clone>(
    script: &Mutex<Script<Q, A>>,
    query: &Q,
) -> Result<A, CanonicalError> {
    script
        .lock()
        .expect("fake pricing lock")
        .iter()
        .find(|(q, _)| q == query)
        .map_or_else(
            || {
                Err(UsageResource::not_found("no scripted pricing answer")
                    .with_resource("fake_pricing")
                    .create())
            },
            |(_, a)| a.clone(),
        )
}

#[async_trait]
impl PricingReadV1 for FakePricingCatalog {
    async fn resolve(
        &self,
        ctx: &SecurityContext,
        query: ResolveQuery,
    ) -> Result<ResolvedBindings, CanonicalError> {
        self.record(PricingCall::Resolve(query.clone()));
        self.refuse_foreign(ctx, query.catalog.tenant_id)?;
        answer(&self.resolves, &query)
    }

    async fn price(
        &self,
        ctx: &SecurityContext,
        query: PriceQuery,
    ) -> Result<ImmutablePrice, CanonicalError> {
        self.record(PricingCall::Price(query.clone()));
        self.refuse_foreign(ctx, query.catalog.tenant_id)?;
        answer(&self.prices, &query)
    }

    async fn current_revision(
        &self,
        ctx: &SecurityContext,
        query: PlanQuery,
    ) -> Result<RevisionRef, CanonicalError> {
        self.record(PricingCall::CurrentRevision(query.clone()));
        self.refuse_foreign(ctx, query.catalog.tenant_id)?;
        answer(&self.revisions, &query)
    }
}

#[cfg(test)]
#[path = "pricing_tests.rs"]
mod tests;
