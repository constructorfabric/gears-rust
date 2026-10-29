//! The assessment's outbound reads (S3-02) and their call discipline (S3-09, F03 §3.3):
//! deadlines, bulkheads, run caps, and one gear-wide circuit breaker per port (D-233; see
//! [`budget`]). Every call, retry, breaker state and run outcome is reported to the
//! [`signals::GateSignals`] sink the [`budget::Budget`] carries. The assessed vector is persisted
//! by [`writer`] (S3-10).
pub mod authority;
pub mod budget;
pub mod commercial;
pub mod commit;
mod failure;
pub mod orchestrate;
pub mod ports;
pub mod preview;
pub mod pricing_read;
pub mod readiness;
pub mod signals;
#[cfg(feature = "gate-dev-stubs")]
pub mod stubs;
pub mod submit;
pub mod unavailable;
pub mod writer;

use crate::config::GateConfig;
use crate::domain::gate::ports::{GatePort, Unavailable};
use crate::domain::gate::pricing_map::{CatalogAnswer, LineCatalogRequest};
use arc_swap::ArcSwap;
use authn_resolver_sdk::AuthNResolverClient;
use authority::commercial_authority;
use authority::{CommercialAuthority, DeadlineAuthority};
use bss_pricing_sdk::read::PricingReadV1;
use bss_products_sdk::api::ProductsClient;
use budget::Budget;
use budget::RunBudget;
use ports::{
    AcceptancePolicyReads, AssessmentReads, BillingTermsReads, ContractReads, EvaluationReads,
    IdentityReads, IndicativeTaxReads, OccupancyReads, OverlapKeyReads,
};
use pricing_read::{CatalogReads, CatalogReadsUnavailable, PricingCatalogReads};
use readiness::{AuthorityState, GateReadiness};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use time::Date;
use tokio_util::sync::CancellationToken;
use unavailable::{
    AcceptancePolicyUnavailable, AssessmentUnavailable, BillingTermsUnavailable,
    ContractsUnavailable, EvaluationUnavailable, IdentityUnavailable, IndicativeTaxUnavailable,
    OccupancyUnavailable, OverlapKeyUnavailable,
};
use uuid::Uuid;

/// The catalog port's live binding (S3-14): fail-closed `Missing` until the Pricing and Products
/// clients are both found in the hub, then [`PricingCatalogReads`] for the rest of the process.
///
/// Pricing and Products have no dependency edge to Orders, so their clients are not looked up at
/// init (whether they were published yet would depend on boot order). The gear binds them from
/// `serve`, as it binds Event Broker and the tenant resolver: every gear's `init` has completed
/// before any `serve` starts. Binding never calls the port.
pub struct CatalogBinding {
    /// The adapter every read goes through.
    reads: ArcSwap<CatalogSlot>,
    /// The principal a bound adapter presents (D-194), validated at init.
    authority: Arc<dyn CommercialAuthority>,
    /// The commercial authority's manifest state.
    authority_state: AuthorityState,
    /// The call discipline a bound adapter runs under.
    budget: Arc<Budget>,
    /// Whether the missing-client warning was logged (once per process).
    warned: AtomicBool,
}

/// One binding of the catalog port.
struct CatalogSlot {
    reads: Arc<dyn CatalogReads>,
    bound: bool,
}

impl CatalogSlot {
    fn missing() -> Self {
        Self {
            reads: Arc::new(CatalogReadsUnavailable(Unavailable::Missing)),
            bound: false,
        }
    }
}

impl std::fmt::Debug for CatalogBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogBinding")
            .field("bound", &self.is_bound())
            .field("authority", &self.authority_state)
            .finish_non_exhaustive()
    }
}

impl CatalogBinding {
    /// An unbound catalog port presenting `authority` once bound.
    fn new(
        authority: Arc<dyn CommercialAuthority>,
        authority_state: AuthorityState,
        budget: Arc<Budget>,
    ) -> Self {
        // The one place the exchange is bounded: the catalog reads and both submit call sites
        // take this authority (`CatalogBinding::authority`).
        let authority: Arc<dyn CommercialAuthority> = Arc::new(DeadlineAuthority::new(
            authority,
            budget.authority_exchange_deadline(),
        ));
        Self {
            reads: ArcSwap::from_pointee(CatalogSlot::missing()),
            authority,
            authority_state,
            budget,
            warned: AtomicBool::new(false),
        }
    }

    /// Whether a real Pricing/Products adapter answers the port.
    #[must_use]
    pub fn is_bound(&self) -> bool {
        self.reads.load().bound
    }

    /// The commercial authority's manifest state.
    #[must_use]
    pub fn authority_state(&self) -> AuthorityState {
        self.authority_state
    }
    /// The seller-scoped service principal the catalog reads present (D-194); submit freezes
    /// the same principal on its commercial attempt.
    #[must_use]
    pub fn authority(&self) -> Arc<dyn CommercialAuthority> {
        Arc::clone(&self.authority)
    }

    /// Bind the port to [`PricingCatalogReads`] when both clients are published in `hub`; a no-op
    /// once bound. Returns whether the port is bound. Without both clients the port stays
    /// `Missing` and one warning (per process) names the absent client. No port call is made.
    pub fn bind_from_hub(&self, hub: &toolkit::ClientHub) -> bool {
        if self.is_bound() {
            return true;
        }
        let pricing = hub.get::<dyn PricingReadV1>().ok();
        let products = hub.get::<dyn ProductsClient>().ok();
        let (Some(pricing), Some(products)) = (pricing.as_ref(), products.as_ref()) else {
            if !self.warned.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    target: "orders.gate",
                    pricing = pricing.is_some(),
                    products = products.is_some(),
                    "bss-orders: the Pricing or Products client is not published; the catalog \
                     ports answer unavailable"
                );
            }
            return false;
        };
        self.reads.store(Arc::new(CatalogSlot {
            reads: Arc::new(PricingCatalogReads::new(
                Arc::clone(pricing),
                Arc::clone(products),
                Arc::clone(&self.authority),
                Arc::clone(&self.budget),
            )),
            bound: true,
        }));
        true
    }
}

#[async_trait::async_trait]
impl CatalogReads for CatalogBinding {
    async fn read_line(
        &self,
        seller_tenant_id: Uuid,
        req: &LineCatalogRequest,
        resolve_date: Date,
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> CatalogAnswer {
        let reads = Arc::clone(&self.reads.load().reads);
        reads
            .read_line(seller_tenant_id, req, resolve_date, run, cancel)
            .await
    }

    async fn read_lines(
        &self,
        seller_tenant_id: Uuid,
        lines: &[LineCatalogRequest],
        resolve_date: Date,
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> std::collections::BTreeMap<Uuid, CatalogAnswer> {
        let reads = Arc::clone(&self.reads.load().reads);
        reads
            .read_lines(seller_tenant_id, lines, resolve_date, run, cancel)
            .await
    }
}

/// Every port the assessment reads, one adapter each.
#[derive(Clone)]
pub struct GatePorts {
    /// Pricing `current_revision`/`resolve` and Products SKU reads, bound late (see
    /// [`CatalogBinding`]).
    pub catalog: Arc<CatalogBinding>,
    /// Payer commercial profile.
    pub identity: Arc<dyn IdentityReads>,
    /// Contract party eligibility.
    pub contracts: Arc<dyn ContractReads>,
    /// Catalog subscription product keys.
    pub overlap_keys: Arc<dyn OverlapKeyReads>,
    /// Overlap presence.
    pub occupancy: Arc<dyn OccupancyReads>,
    /// Pre-subscription billing terms.
    pub billing_terms: Arc<dyn BillingTermsReads>,
    /// Acceptance policy.
    pub acceptance_policy: Arc<dyn AcceptancePolicyReads>,
    /// Purchase assessment.
    pub assessment: Arc<dyn AssessmentReads>,
    /// Rating evaluation.
    pub evaluation: Arc<dyn EvaluationReads>,
    /// Indicative tax.
    pub indicative_tax: Arc<dyn IndicativeTaxReads>,
    /// The ports a development stub answers (`gate-dev-stubs`); empty in every other build.
    stubbed: Vec<GatePort>,
    /// The development fixture the stubs answer from, when bound (`gate-dev-stubs`); submit's
    /// development acceptance double reads its acceptance policy (M2-6).
    #[cfg(feature = "gate-dev-stubs")]
    dev_fixture: Option<Arc<stubs::StubFixture>>,
}

impl std::fmt::Debug for GatePorts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatePorts").finish_non_exhaustive()
    }
}

impl GatePorts {
    /// Every port bound to its fail-closed adapter: each answers `Missing`, and the catalog
    /// presents [`authority::NotProvisioned`] if it is ever bound (tests only; the gear binds
    /// through [`Self::from_hub`]).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn unavailable() -> Self {
        Self::with_catalog(CatalogBinding::new(
            Arc::new(authority::NotProvisioned),
            AuthorityState::NotProvisioned,
            Arc::new(Budget::baseline()),
        ))
    }

    fn with_catalog(catalog: CatalogBinding) -> Self {
        Self {
            catalog: Arc::new(catalog),
            identity: Arc::new(IdentityUnavailable),
            contracts: Arc::new(ContractsUnavailable),
            overlap_keys: Arc::new(OverlapKeyUnavailable),
            occupancy: Arc::new(OccupancyUnavailable),
            billing_terms: Arc::new(BillingTermsUnavailable),
            acceptance_policy: Arc::new(AcceptancePolicyUnavailable),
            assessment: Arc::new(AssessmentUnavailable),
            evaluation: Arc::new(EvaluationUnavailable),
            indicative_tax: Arc::new(IndicativeTaxUnavailable),
            stubbed: Vec::new(),
            #[cfg(feature = "gate-dev-stubs")]
            dev_fixture: None,
        }
    }

    /// The development fixture the stubs answer from, when `gate.dev_stubs` bound them.
    #[cfg(feature = "gate-dev-stubs")]
    #[must_use]
    pub fn dev_fixture(&self) -> Option<&Arc<stubs::StubFixture>> {
        self.dev_fixture.as_ref()
    }

    /// The ports a development stub answers; empty unless `gate-dev-stubs` bound them.
    #[must_use]
    pub fn stubbed(&self) -> &[GatePort] {
        &self.stubbed
    }

    /// Task 8b: bind a development stub over `fixture` for every port without a delivered
    /// provider (its real client is absent). A delivered port (the catalog's `current-revision`,
    /// `resolve` and `sku-read`) is never stubbed, so a real client is never replaced.
    #[cfg(feature = "gate-dev-stubs")]
    #[must_use]
    pub fn with_dev_stubs(mut self, fixture: &Arc<stubs::StubFixture>) -> Self {
        use crate::domain::gate::ports::Delivery;
        self.dev_fixture = Some(Arc::clone(fixture));
        let f = || Arc::clone(fixture);
        for port in GatePort::ALL {
            if port.delivery() == Delivery::Available {
                continue;
            }
            match port {
                GatePort::CurrentRevision | GatePort::Resolve | GatePort::SkuRead => continue,
                GatePort::Assessment => self.assessment = Arc::new(stubs::AssessmentStub::new(f())),
                GatePort::BillingTerms => {
                    self.billing_terms = Arc::new(stubs::BillingTermsStub::new(f()));
                }
                GatePort::AcceptancePolicy => {
                    self.acceptance_policy = Arc::new(stubs::AcceptancePolicyStub::new(f()));
                }
                GatePort::IdentityProfile => {
                    self.identity = Arc::new(stubs::IdentityStub::new(f()));
                }
                GatePort::ContractResolution => {
                    self.contracts = Arc::new(stubs::ContractsStub::new(f()));
                }
                GatePort::OverlapKey => {
                    self.overlap_keys = Arc::new(stubs::OverlapKeyStub::new(f()));
                }
                GatePort::Occupancy => self.occupancy = Arc::new(stubs::OccupancyStub::new(f())),
                GatePort::Evaluation => {
                    self.evaluation = Arc::new(stubs::EvaluationStub::new(f()));
                }
                GatePort::IndicativeTax => {
                    self.indicative_tax = Arc::new(stubs::IndicativeTaxStub::new(f()));
                }
            }
            self.stubbed.push(port);
        }
        self
    }

    /// The composition root at init (S3-14): every port bound to its fail-closed adapter, never
    /// a double, and the commercial authority validated.
    ///
    /// The commercial authority is [`authority::NotProvisioned`] without configured identities
    /// and [`authority::AuthnAuthority`] over the published `AuthN` resolver otherwise (the gear
    /// depends on `authn_resolver`, so the resolver is published before Orders' init). The
    /// catalog port starts `Missing`: its Pricing and Products clients are optional and bound
    /// later from `serve` ([`CatalogBinding::bind_from_hub`]). Every other port has no
    /// delivered provider and is its `*Unavailable` adapter. Nothing is called.
    ///
    /// With the `gate-dev-stubs` feature and `gate.dev_stubs = true`, every undelivered port is
    /// then bound to its development stub over `gate.dev_stub_fixture` (Task 8b) and a warning
    /// naming the fixture is logged on `orders.gate.dev_stubs`.
    ///
    /// `config` is already validated (`OrdersConfig::validate` runs `GateConfig::validate` once
    /// at init): `gate.dev_stubs` without the feature or without a fixture path never reaches
    /// here.
    ///
    /// # Errors
    /// `gate.commercial_identities` is configured but no `AuthNResolverClient` is published;
    /// with the feature, a missing, unreadable or invalid fixture.
    pub fn from_hub(
        hub: &toolkit::ClientHub,
        config: &GateConfig,
        budget: Arc<Budget>,
    ) -> anyhow::Result<Self> {
        let identities = &config.commercial_identities;
        let authority = commercial_authority(
            if identities.is_empty() {
                None
            } else {
                hub.get::<dyn AuthNResolverClient>().ok()
            },
            identities,
        )?;
        let authority_state = if identities.is_empty() {
            AuthorityState::NotProvisioned
        } else {
            AuthorityState::Bound
        };
        let ports = Self::with_catalog(CatalogBinding::new(authority, authority_state, budget));
        // Task 8b: feature-gated stubs of the undelivered providers, never in production builds.
        Self::bind_dev_stubs(ports, config)
    }

    /// Without the feature nothing can be stubbed; validation already refused `gate.dev_stubs`.
    #[cfg(not(feature = "gate-dev-stubs"))]
    #[expect(
        clippy::unnecessary_wraps,
        reason = "one signature for both feature variants of the composition root"
    )]
    fn bind_dev_stubs(ports: Self, _config: &GateConfig) -> anyhow::Result<Self> {
        Ok(ports)
    }

    #[cfg(feature = "gate-dev-stubs")]
    fn bind_dev_stubs(ports: Self, config: &GateConfig) -> anyhow::Result<Self> {
        let Some(path) = config.dev_delivery_fixture() else {
            // `validate` refuses the flag without a fixture; an unvalidated config binds nothing.
            debug_assert!(
                !config.dev_stubs,
                "gate.dev_stubs without a fixture is refused by validate()"
            );
            return Ok(ports);
        };
        let fixture = Arc::new(stubs::StubFixture::load(path)?);
        let ports = ports.with_dev_stubs(&fixture);
        tracing::warn!(
            target: "orders.gate.dev_stubs",
            fixture = %path.display(),
            stubbed = ports.stubbed.len(),
            "bss-orders: development stubs answer the undelivered gate ports from the fixture; \
             never a production binding, never readiness evidence"
        );
        Ok(ports)
    }

    /// The manifest of the live bindings: the catalog state and each port's breaker state are
    /// read at each call, so it is accurate after `serve` binds the catalog clients.
    #[must_use]
    pub fn readiness(&self) -> GateReadiness {
        let now = tokio::time::Instant::now();
        let breakers: Vec<_> = GatePort::ALL
            .into_iter()
            .filter_map(|p| self.catalog.budget.breaker_state(p, now).map(|s| (p, s)))
            .collect();
        GateReadiness::new(self.catalog.is_bound(), self.catalog.authority_state())
            .with_stubbed(&self.stubbed)
            .with_breakers(&breakers)
    }
}

/// In-crate gate tests; `pub(crate)` so the gear-boot tests reuse the upstream doubles.
#[cfg(test)]
pub mod tests;
