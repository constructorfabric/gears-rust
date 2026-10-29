//! Development stubs of the nine undelivered gate providers (Task 8b), compiled only with the
//! `gate-dev-stubs` cargo feature and bound only when `gate.dev_stubs = true`.
//!
//! A stub implements the same Orders-owned port trait as the future real adapter, answers only
//! what the JSON fixture ([`StubFixture`]) states, and answers `Unavailable(Missing)` for anything
//! the fixture does not state. A batched port answers `Missing` when any key of the batch is
//! absent: a stub never answers part of a batch. No stub computes: every value is copied from the
//! fixture (no money arithmetic, no sum, no derived total). Readiness reports each stubbed port as
//! `stubbed (development only)`; stubbed answers are never readiness evidence (D-199, UPR R11),
//! and no production composition root binds them (`apps/example-server` never enables the
//! feature).
use super::budget::RunBudget;
use super::ports::{
    AcceptancePolicyReads, AssessmentReads, BillingTermsReads, ContractReads, EvaluationReads,
    IdentityReads, IndicativeTaxReads, OccupancyReads, OverlapKeyReads,
};
use crate::domain::gate::answers::{
    AcceptancePolicy, AcceptancePolicyQuery, AssessmentAnswer, AssessmentQuery, Availability,
    BillingTermsQuery, CommittedComponent, ContractFacts, ContractQuery, IndicativeTax,
    LineEvaluation, OccupancyObservation, OccupancyQuery, OrderEvaluation, OrderEvaluationRequest,
    OverlapKey, OverlapKeyLine, PayerProfile, PayerProfileQuery, ProducerResultWithKey,
    ResolvedTerms, TaxQuery, Tcv, Totals, TotalsDeclaration,
};
use crate::domain::gate::commercial::deterministic_receipt;
use crate::domain::gate::ports::{PortAnswer, Unavailable};
use crate::domain::gate::predicates::PredicateId as P;
use crate::domain::gate::vector::{ProducerResult, Verdict};
use async_trait::async_trait;
use bss_orders_sdk::gate_report::CatalogSelection;
use bss_pricing_sdk::acceptance::{
    AcceptanceReceipt, CommandMeta, CommercialReason, FulfilmentEligibility, FulfilmentQuery,
    NewSaleQuery, SellabilityV1,
};
use bss_pricing_sdk::read::{CatalogRef, PricingReadV1, ResolveQuery, ResolvedBindings};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// The producer name the assessment stub's results carry, so a stubbed subresult is never
/// mistaken for Pricing's.
pub const STUB_PRODUCER: &str = "pricing-stub";

/// The `request_digest` the evaluation stub answers: a marker, never a digest of the request.
pub const STUB_REQUEST_DIGEST: &str = "gate-dev-stub";

/// The order-level facts of a stubbed evaluation, stated by the fixture because the stub never
/// sums its lines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StubEvaluationTotals {
    /// The evaluation currency.
    pub currency: String,
    /// The currency's minor-unit scale.
    pub currency_scale: u8,
    /// The order totals, as given.
    pub order_totals: Totals,
    /// Producer-declared components, as given; absent retains legacy fixture semantics.
    #[serde(default)]
    pub order_committed_components: Option<Vec<CommittedComponent>>,
    /// Explicit recurring TCV carrier, as given.
    #[serde(default)]
    pub order_tcv_carrier: Option<CommittedComponent>,
    /// Whether promotions were evaluated, as given.
    pub promotion_status: Availability,
    /// The order roll-up's storage declaration, as given (submit needs it; absent for a
    /// Preview-only fixture).
    #[serde(default)]
    pub order_declaration: Option<TotalsDeclaration>,
    /// The order roll-up's usage declaration, as given (when a line carries usage charges).
    #[serde(default)]
    pub order_usage_declaration: Option<TotalsDeclaration>,
}

/// Explicit development classification; never inferred from a legacy scalar map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StubItemClassification {
    /// Independently supplied product identity.
    pub catalog_subscription_product_key: String,
    /// Supplied scope identity.
    pub overlap_scope_key: String,
    /// Explicit development provenance.
    pub classification: crate::domain::gate::item_keys::ClassificationProvenance,
}

/// The development fixture every stub answers from (serde JSON). Every field is optional: an
/// absent entry makes the port it feeds answer `Unavailable(Missing)`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StubFixture {
    /// Payer profiles by payer tenant id (identity port).
    #[serde(default)]
    pub payer_profiles: BTreeMap<Uuid, PayerProfile>,
    /// Contract facts by contract id (contracts port).
    #[serde(default)]
    pub contracts: BTreeMap<Uuid, ContractFacts>,
    /// The overlap scope key by paid recurring SKU id (overlap-key port).
    #[serde(default)]
    pub overlap_keys: BTreeMap<Uuid, String>,
    /// Proposed complete classifications by SKU, explicitly supplied by development fixtures.
    /// An empty map selects only the historical scalar adapter.
    #[serde(default)]
    pub item_classifications: BTreeMap<Uuid, StubItemClassification>,
    /// Occupancy by overlap scope key (occupancy port).
    #[serde(default)]
    pub occupancy: BTreeMap<String, OccupancyObservation>,
    /// Resolved billing terms by plan revision id (billing-terms port); the answer carries the
    /// query's `line_id`.
    #[serde(default)]
    pub terms: BTreeMap<Uuid, ResolvedTerms>,
    /// The seller's acceptance policy (acceptance-policy port), for every seller.
    #[serde(default)]
    pub acceptance_policy: Option<AcceptancePolicy>,
    /// The assessment profile: when set, the assessment stub answers `passed` for predicates 2,
    /// 7 and 8 and 4-Orders on every line and selected slot, as producer [`STUB_PRODUCER`].
    #[serde(default)]
    pub assessment_profile: Option<String>,
    /// Line evaluations by plan revision id (evaluation port); each applies to every request
    /// line priced on that revision, and the answer carries the request line's `line_id` (the
    /// fixture's own `line_id` is a placeholder, as for `terms`).
    #[serde(default)]
    pub evaluation: BTreeMap<Uuid, LineEvaluation>,
    /// The evaluation's order-level facts (evaluation port); without them the port is `Missing`.
    #[serde(default)]
    pub evaluation_totals: Option<StubEvaluationTotals>,
    /// The total contract value, as given (evaluation port).
    #[serde(default)]
    pub tcv: Option<Tcv>,
    /// The indicative tax (tax port), for every query.
    #[serde(default)]
    pub tax: Option<IndicativeTax>,
}

impl StubFixture {
    /// Load the fixture from a JSON file.
    ///
    /// # Errors
    /// The file is missing, unreadable or not a valid fixture; the message names the path.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            anyhow::anyhow!(
                "bss-orders: gate.dev_stub_fixture `{}` is unreadable: {e}",
                path.display()
            )
        })?;
        serde_json::from_str(&text).map_err(|e| {
            anyhow::anyhow!(
                "bss-orders: gate.dev_stub_fixture `{}` is not a valid fixture: {e}",
                path.display()
            )
        })
    }
}

/// `Answered(value)` when the fixture states it, `Unavailable(Missing)` otherwise.
fn stated<T>(value: Option<T>) -> PortAnswer<T> {
    value.map_or(PortAnswer::Unavailable(Unavailable::Missing), |v| {
        PortAnswer::Answered(v)
    })
}

macro_rules! stub_struct {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone)]
        pub struct $name(Arc<StubFixture>);

        impl $name {
            /// A stub answering from `fixture`.
            #[must_use]
            pub fn new(fixture: Arc<StubFixture>) -> Self {
                Self(fixture)
            }
        }
    };
}

stub_struct!(
    /// [`IdentityReads`] from the fixture, keyed on `payer_tenant_id`.
    IdentityStub
);
stub_struct!(
    /// [`ContractReads`] from the fixture, keyed on `contract_id`.
    ContractsStub
);
stub_struct!(
    /// [`OverlapKeyReads`] from the fixture, keyed on `paid_recurring_sku`.
    OverlapKeyStub
);
stub_struct!(
    /// [`OccupancyReads`] from the fixture, keyed on `overlap_scope_key`.
    OccupancyStub
);
stub_struct!(
    /// [`BillingTermsReads`] from the fixture, keyed on `plan_revision_id`.
    BillingTermsStub
);
stub_struct!(
    /// [`AcceptancePolicyReads`] from the fixture's single policy.
    AcceptancePolicyStub
);
stub_struct!(
    /// [`AssessmentReads`] from the fixture's assessment profile.
    AssessmentStub
);
stub_struct!(
    /// [`EvaluationReads`] from the fixture's line evaluations (by plan revision), order totals and TCV.
    EvaluationStub
);
stub_struct!(
    /// [`IndicativeTaxReads`] from the fixture's single tax answer.
    IndicativeTaxStub
);

#[async_trait]
impl IdentityReads for IdentityStub {
    async fn payer_profile(
        &self,
        _ctx: &SecurityContext,
        query: &PayerProfileQuery,
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> PortAnswer<PayerProfile> {
        stated(self.0.payer_profiles.get(&query.payer_tenant_id).cloned())
    }
}

#[async_trait]
impl ContractReads for ContractsStub {
    async fn resolve(
        &self,
        _ctx: &SecurityContext,
        queries: &[ContractQuery],
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> PortAnswer<BTreeMap<Uuid, ContractFacts>> {
        stated(
            queries
                .iter()
                .map(|q| {
                    let facts = self.0.contracts.get(&q.contract_id)?;
                    Some((q.contract_id, facts.clone()))
                })
                .collect(),
        )
    }
}

#[async_trait]
impl OverlapKeyReads for OverlapKeyStub {
    async fn keys(
        &self,
        _ctx: &SecurityContext,
        _payer_tenant_id: Uuid,
        _resource_tenant_id: Option<Uuid>,
        lines: &[OverlapKeyLine],
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> PortAnswer<Vec<OverlapKey>> {
        // Complete fixture classifications and legacy scalar fixtures remain separate profiles.
        stated(
            lines
                .iter()
                .map(|l| {
                    if !self.0.item_classifications.is_empty() {
                        let classification =
                            self.0.item_classifications.get(&l.paid_recurring_sku)?;
                        return Some(OverlapKey {
                            line_id: l.line_id,
                            catalog_subscription_product_key: classification
                                .catalog_subscription_product_key
                                .clone(),
                            overlap_scope_key: classification.overlap_scope_key.clone(),
                            item_key: Some(crate::domain::gate::item_keys::ItemOverlapKey {
                                profile: "item_keys_v1".into(),
                                selection: l.clone(),
                                catalog_subscription_product_key: classification
                                    .catalog_subscription_product_key
                                    .clone(),
                                overlap_scope_key: classification.overlap_scope_key.clone(),
                                classification: classification.classification.clone(),
                            }),
                        });
                    }
                    let key = self.0.overlap_keys.get(&l.paid_recurring_sku)?;
                    Some(OverlapKey {
                        line_id: l.line_id,
                        catalog_subscription_product_key: key.clone(),
                        overlap_scope_key: key.clone(),
                        item_key: None,
                    })
                })
                .collect(),
        )
    }
}

#[async_trait]
impl OccupancyReads for OccupancyStub {
    async fn get_overlap_occupancy(
        &self,
        _ctx: &SecurityContext,
        queries: &[OccupancyQuery],
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> PortAnswer<Vec<(OccupancyQuery, OccupancyObservation)>> {
        stated(
            queries
                .iter()
                .map(|q| {
                    let occupancy = self.0.occupancy.get(&q.overlap_scope_key)?;
                    Some((q.clone(), occupancy.clone()))
                })
                .collect(),
        )
    }
}

#[async_trait]
impl BillingTermsReads for BillingTermsStub {
    async fn resolve(
        &self,
        queries: &[BillingTermsQuery],
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> PortAnswer<Vec<ResolvedTerms>> {
        stated(
            queries
                .iter()
                .map(|q| {
                    let terms = self.0.terms.get(&q.plan_revision_id)?;
                    Some(ResolvedTerms {
                        line_id: q.line_id,
                        ..terms.clone()
                    })
                })
                .collect(),
        )
    }
}

#[async_trait]
impl AcceptancePolicyReads for AcceptancePolicyStub {
    async fn policy(
        &self,
        _query: &AcceptancePolicyQuery,
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> PortAnswer<Option<AcceptancePolicy>> {
        stated(self.0.acceptance_policy.clone().map(Some))
    }
}

/// The assessment results of `query` under `profile`: one passed result per line for
/// predicates 2, 8 and 4-Orders, and one passed predicate 7 result per distinct selected slot,
/// keyed exactly as the gate's expected coverage (`map_line`) keys them.
#[must_use]
pub fn passing_results(query: &AssessmentQuery, profile: &str) -> Vec<ProducerResultWithKey> {
    let passed = |p: P| ProducerResult {
        producer: STUB_PRODUCER.to_owned(),
        profile: profile.to_owned(),
        predicate: p.token().to_owned(),
        verdict: Verdict::Passed,
        reason: None,
        detail: None,
    };
    let mut results = Vec::new();
    for line in &query.lines {
        for p in [
            P::CatalogRevisionAvailable,
            P::CatalogEligibilityRules,
            P::MarketConsistency,
        ] {
            results.push(ProducerResultWithKey {
                line_id: line.line_id,
                item_id: None,
                selection: CatalogSelection::NotSelected,
                result: passed(p),
            });
        }
        let mut slots = BTreeSet::new();
        for s in &line.selections {
            if slots.insert((s.item_id, s.selected_dim_value.clone())) {
                results.push(ProducerResultWithKey {
                    line_id: line.line_id,
                    item_id: Some(s.item_id),
                    selection: s
                        .selected_dim_value
                        .clone()
                        .map_or(CatalogSelection::SelectedNull, CatalogSelection::Selected),
                    result: passed(P::CatalogDimensionMarket),
                });
            }
        }
    }
    results
}

#[async_trait]
impl AssessmentReads for AssessmentStub {
    async fn assess(
        &self,
        query: &AssessmentQuery,
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> PortAnswer<AssessmentAnswer> {
        stated(
            self.0
                .assessment_profile
                .as_ref()
                .map(|profile| AssessmentAnswer {
                    profile: profile.clone(),
                    observed_at: OffsetDateTime::now_utc(),
                    // The envelope names the gate contract; subresults retain their producer profile.
                    results: passing_results(query, "dev-stub"),
                }),
        )
    }
}

#[async_trait]
impl EvaluationReads for EvaluationStub {
    async fn evaluate(
        &self,
        _ctx: &SecurityContext,
        request: &OrderEvaluationRequest,
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> PortAnswer<OrderEvaluation> {
        let fixture = &self.0;
        let Some(order) = fixture.evaluation_totals.as_ref() else {
            return PortAnswer::Unavailable(Unavailable::Missing);
        };
        // Every requested line must be stated: a partial evaluation is never summed or filled.
        let lines: Option<Vec<LineEvaluation>> = request
            .lines
            .iter()
            .map(|l| {
                let stated = fixture.evaluation.get(&l.plan_revision_id)?;
                Some(LineEvaluation {
                    line_id: l.line_id,
                    ..stated.clone()
                })
            })
            .collect();
        stated(lines.map(|lines| OrderEvaluation {
            assessment_id: request.assessment_id,
            estimate: true,
            currency: order.currency.clone(),
            currency_scale: order.currency_scale,
            lines,
            order_totals: order.order_totals.clone(),
            order_committed_components: order.order_committed_components.clone(),
            order_tcv_carrier: order.order_tcv_carrier.clone(),
            order_declaration: order.order_declaration.clone(),
            order_usage_declaration: order.order_usage_declaration.clone(),
            tcv: fixture.tcv.clone(),
            promotion_status: order.promotion_status.clone(),
            request_digest: STUB_REQUEST_DIGEST.to_owned(),
        }))
    }
}

#[async_trait]
impl IndicativeTaxReads for IndicativeTaxStub {
    async fn estimate(
        &self,
        _ctx: &SecurityContext,
        _query: &TaxQuery,
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> PortAnswer<IndicativeTax> {
        stated(self.0.tax.clone())
    }
}

// ---- Pricing acceptance (submit phase B, M2-6) ----

/// The development stub's acceptance identity namespace: a receipt's `acceptance_id` is the
/// version-5 UUID of its command key under it, so a replayed command gets the same identity.
const STUB_ACCEPTANCE_NAMESPACE: Uuid = Uuid::from_u128(0x6f72_6465_7273_2d64_6576_2d61_6363_6570);

/// What a [`SellabilityStub`] accepts a query on: Pricing's own `resolve` answer for the query's
/// plan revision. The development binding reads the published `PricingReadV1`
/// ([`HubResolveBindings`]); tests answer from fixed bindings.
#[async_trait]
pub trait StubBindings: Send + Sync {
    /// The revision's resolved bindings for `query`, as Pricing answers them now.
    ///
    /// # Errors
    /// Pricing's own error, or unavailability.
    async fn resolve(
        &self,
        ctx: &SecurityContext,
        query: &NewSaleQuery,
    ) -> Result<ResolvedBindings, CanonicalError>;
}

/// [`StubBindings`] over the `PricingReadV1` published in the hub, resolved on today's UTC date
/// (the stub's stand-in for Pricing's command-time resolution).
pub struct HubResolveBindings(pub Arc<toolkit::ClientHub>);

#[async_trait]
impl StubBindings for HubResolveBindings {
    async fn resolve(
        &self,
        ctx: &SecurityContext,
        query: &NewSaleQuery,
    ) -> Result<ResolvedBindings, CanonicalError> {
        let pricing = self
            .0
            .get::<dyn PricingReadV1>()
            .map_err(|_| CanonicalError::service_unavailable().create())?;
        pricing
            .resolve(
                ctx,
                ResolveQuery {
                    catalog: CatalogRef {
                        tenant_id: query.tenant_axes.seller_tenant_id,
                    },
                    revision_id: query.plan_revision_id,
                    date: OffsetDateTime::now_utc().date(),
                    item_id: None,
                    pins: Vec::new(),
                },
            )
            .await
    }
}

/// A development double of Pricing's `SellabilityV1` that issues deterministic receipts (Task
/// M2-6): the bindings are Pricing's resolved cells of the query's selections and must digest to
/// the query's `resolved_bindings_digest` (else `RESOLUTION_CHANGED`); the digests are recomputed
/// with `bss_pricing_sdk::digest`; `hold_until = accepted_at + acceptance_policy.hold_seconds`;
/// the acceptance id is derived from the command key. A command key replays its first receipt;
/// the same key with another query is `IDEMPOTENCY_CONFLICT`. Without an acceptance policy in the
/// fixture every command answers unavailable. Never a production binding.
pub struct SellabilityStub {
    hold_seconds: Option<u32>,
    bindings: Arc<dyn StubBindings>,
    issued: parking_lot::Mutex<BTreeMap<String, AcceptanceReceipt>>,
}

impl std::fmt::Debug for SellabilityStub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SellabilityStub")
            .field("hold_seconds", &self.hold_seconds)
            .finish_non_exhaustive()
    }
}

impl SellabilityStub {
    /// A stub holding receipts for the fixture's acceptance policy, accepting on `bindings`.
    #[must_use]
    pub fn new(fixture: &StubFixture, bindings: Arc<dyn StubBindings>) -> Self {
        Self {
            hold_seconds: fixture.acceptance_policy.as_ref().map(|p| p.hold_seconds),
            bindings,
            issued: parking_lot::Mutex::new(BTreeMap::new()),
        }
    }
}

#[async_trait]
impl SellabilityV1 for SellabilityStub {
    async fn check(
        &self,
        ctx: &SecurityContext,
        query: NewSaleQuery,
        meta: CommandMeta,
    ) -> Result<AcceptanceReceipt, CanonicalError> {
        let Some(hold_seconds) = self.hold_seconds else {
            return Err(CanonicalError::service_unavailable().create());
        };
        if let Some(first) = self.issued.lock().get(&meta.idempotency_key) {
            return if first.query == query {
                Ok(first.clone())
            } else {
                Err(CommercialReason::IdempotencyConflict.into())
            };
        }
        let resolved = self.bindings.resolve(ctx, &query).await?;
        let acceptance_id =
            Uuid::new_v5(&STUB_ACCEPTANCE_NAMESPACE, meta.idempotency_key.as_bytes());
        let receipt = deterministic_receipt(
            query,
            &resolved,
            acceptance_id,
            OffsetDateTime::now_utc(),
            hold_seconds,
        )
        .map_err(CanonicalError::from)?;
        // A concurrent first command under the same key keeps the first receipt, and answers
        // the first-check arm's verdict: the same query replays it, another query conflicts.
        let query = receipt.query.clone();
        let mut issued = self.issued.lock();
        let stored = issued.entry(meta.idempotency_key).or_insert(receipt);
        if stored.query == query {
            Ok(stored.clone())
        } else {
            Err(CommercialReason::IdempotencyConflict.into())
        }
    }

    async fn check_fulfilment(
        &self,
        _ctx: &SecurityContext,
        _query: FulfilmentQuery,
    ) -> Result<FulfilmentEligibility, CanonicalError> {
        Err(CanonicalError::service_unavailable().create())
    }
}
