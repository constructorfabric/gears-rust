//! The orchestrator's async shell (S3-11/S3-12; F03 §3.6 steps 2–5; D-215, D-180, D-227, M2-6):
//! one gate run calls each port at most once per basket, with batched queries, in F03 order,
//! under one fresh [`RunBudget`], and assesses the collected answers through the pure half
//! ([`crate::domain::gate::orchestrate`]).
//!
//! Dependency waves (one checkpoint before starting and after each wave):
//! A. identity, the basket's catalog `read_lines`, and seller acceptance policy run together;
//! B. only after [`IdentityGate::Confirmed`], referenced contracts, billing terms and the
//!    overlap chain run together. The chain resolves keys, then occupancy, then both in-flight
//!    reads (D-180 cross-resource counts and same-tuple claims), checking cancellation between
//!    dependent calls. No payer-related commercial fact is fetched before confirmation;
//! C. the D-189 assessment and the evaluation run together; evaluation is asked only when
//!    [`evaluation_request`] can be built from the completed catalog/terms/market;
//! D. indicative tax runs only for Preview over an answered evaluation.
//!
//! Answers are collected before unavailable diagnostics are recorded, in the stable historical
//! order: identity, contracts, catalog (line-id order; current revision, resolve, SKU reads),
//! overlap keys, occupancy, billing terms, acceptance policy, assessment, evaluation, tax.
//! Completion order cannot change [`Resolved::unavailable_kinds`] or the once-per-run catalog
//! denial. A port the run does not call is "not asked" (the pure half's module doc). Each
//! unavailable called port whose answer reaches diagnostic collection is logged once at info
//! on `orders.gate.unavailable_kind` with `port` and `kind` (D-227). In-flight storage failures
//! retain completed Wave A/B port diagnostics before propagating the local error; they produce
//! no assessment or durable outcome vector. Cancellation returns
//! [`OrchestrateError::Cancelled`]; siblings
//! within the current wave may already have been called, but no following wave starts.
//!
//! Signals (03 §3.8), to the budget's [`GateSignals`] sink: each collected unavailable answer
//! once per port per run ([`GateSignals::port_unavailable`]), collected catalog denials as one
//! denial of the seller's catalog ([`GateSignals::catalog_denial`], once per run however many
//! catalog ports were denied), and an assessed run's [`RunSignals`] with
//! its elapsed time ([`GateSignals::run`]; the flow is Preview when the plan asks for tax). A
//! cancelled or failed run reports no run signal.
use super::GatePorts;
use super::budget::{Budget, RunBudget};
use super::pricing_read::CatalogReads;
use super::signals::{Flow, GateSignals};
use crate::domain::gate::answers::{
    AcceptancePolicy, IndicativeTax, Market, OrderEvaluation, OrderEvaluationRequest, OverlapKey,
    ResolvedTerms,
};
use crate::domain::gate::assess::{Assessment, assess};
use crate::domain::gate::commercial::CommercialError;
use crate::domain::gate::orchestrate::{
    Answers, Basket, IdentityGate, OccupancyAnswer, assemble, assessment_query, evaluation_request,
    gate_after_identity, line_bindings, occupancy_queries, overlap_key_lines, payer_market, plan,
    tax_query,
};
use crate::domain::gate::ports::{GatePort, PortAnswer, Unavailable};
use crate::domain::gate::pricing_map::{CatalogAnswer, ReadFailure};
use crate::domain::gate::signals::RunSignals;
use crate::domain::gate::vector::VectorError;
use crate::domain::overlap::ClaimParties;
use crate::infra::storage::repo::inflight;
use crate::infra::storage::scoped::PrivateScope;
use async_trait::async_trait;
use bss_pricing_sdk::read::{AcceptedBinding, ResolvedBindings};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use toolkit_db::Db;
use toolkit_db::secure::ScopeError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Why an in-flight read failed, typed as the date-policy preparation does
/// (`PrepareDatesError::{Connection, Store}`): the connection and the scoped read stay distinct.
#[derive(Debug, thiserror::Error)]
pub enum InflightReadError {
    /// No database connection.
    #[error("in-flight store connection")]
    Connection(#[source] toolkit_db::DbError),
    /// The scoped read failed.
    #[error("in-flight store read")]
    Store(#[source] ScopeError),
}

/// The D-180 interim addend reader (F03 §4.2 predicate 7): the cross-customer in-flight count.
///
/// A D-180 privileged count: the caller cannot be granted another resource tenant's orders, so
/// the reader runs under the gear's private scope ([`PrivateScope::for_payer_count`], the 08 §3.5
/// exception D-225 item 6 records), not a caller scope, and returns only a count per key. No
/// other order is ever named, returned or disclosed. The predicate-9 reader uses the same
/// bounded private authority and returns only the requested keys held by another order.
#[async_trait]
pub trait InflightClaims: Send + Sync {
    /// Per key of `keys`, the lines carrying it on the current version of every other in-flight
    /// order of `payer` that holds a live claim on `(payer, key)` under a resource tenant other
    /// than `resource` (same-resource siblings are read by `held_same_tuple`), excluding
    /// `exclude_order` (`None` for a Preview, which has no order). Every key asked has an entry.
    ///
    /// # Errors
    /// Scope/store failure.
    async fn count_other_orders(
        &self,
        tuple: ClaimParties,
        keys: &[String],
        exclude_order: Option<Uuid>,
    ) -> Result<BTreeMap<String, u32>, InflightReadError>;
    /// Same payer/resource keys held live by another order, never disclosing its identity.
    /// # Errors
    /// Scope/store failure.
    async fn held_same_tuple(
        &self,
        tuple: ClaimParties,
        keys: &[String],
        exclude_order: Option<Uuid>,
    ) -> Result<BTreeSet<String>, InflightReadError>;
}

/// The storage reader over `inflight_overlap_claim` (read-only, never in a transaction).
#[derive(Clone)]
pub struct StorageInflightClaims {
    db: Db,
}

impl StorageInflightClaims {
    /// A reader over `db`.
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[async_trait]
impl InflightClaims for StorageInflightClaims {
    async fn count_other_orders(
        &self,
        tuple: ClaimParties,
        keys: &[String],
        exclude_order: Option<Uuid>,
    ) -> Result<BTreeMap<String, u32>, InflightReadError> {
        let conn = self.db.conn().map_err(InflightReadError::Connection)?;
        let scope = PrivateScope::for_payer_count(tuple.payer_tenant_id);
        inflight::count_other_orders(&conn, &scope, tuple, keys, exclude_order)
            .await
            .map_err(InflightReadError::Store)
    }
    async fn held_same_tuple(
        &self,
        tuple: ClaimParties,
        keys: &[String],
        exclude_order: Option<Uuid>,
    ) -> Result<BTreeSet<String>, InflightReadError> {
        let conn = self.db.conn().map_err(InflightReadError::Connection)?;
        let scope = PrivateScope::for_payer_count(tuple.payer_tenant_id);
        inflight::held_same_tuple(&conn, &scope, tuple, keys, exclude_order)
            .await
            .map_err(InflightReadError::Store)
    }
}

/// Why a run produced no assessment.
#[derive(Debug, thiserror::Error)]
pub enum OrchestrateError {
    /// The caller cancelled the run.
    #[error("the gate run was cancelled")]
    Cancelled,
    /// A local integration failed: an in-flight read, or the assessment's own vector rules.
    #[error("the gate run failed: {what}")]
    Integration {
        /// What failed.
        what: &'static str,
        /// The cause.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

impl From<VectorError> for OrchestrateError {
    fn from(e: VectorError) -> Self {
        Self::Integration {
            what: "assessment vector",
            source: Box::new(e),
        }
    }
}

/// One run's assessment and the answers later steps reuse.
#[derive(Debug)]
pub struct Resolved {
    /// The assessment.
    pub assessment: Assessment,
    /// Each line's accepted bindings, from its composed pin; a line without one has no entry.
    pub bindings: BTreeMap<Uuid, Vec<AcceptedBinding>>,
    /// Each line's Pricing `resolve` answer, when answered: submit digests the line's
    /// selections over it (`selected_bindings_digest`) for the frozen request (S3-12).
    pub resolve_answers: BTreeMap<Uuid, Arc<ResolvedBindings>>,
    /// The payer's market, when the relationship is confirmed (D-215).
    pub market: Option<Market>,
    /// Resolved billing terms per line, when asked (relationship confirmed) and answered.
    pub terms: BTreeMap<Uuid, ResolvedTerms>,
    /// The seller's acceptance policy, when answered and present.
    pub policy: Option<AcceptancePolicy>,
    /// The exact request sent to Rating, absent when evaluation was not asked.
    pub evaluation_request: Option<OrderEvaluationRequest>,
    /// The Rating evaluation, when asked and answered.
    pub evaluation: Option<OrderEvaluation>,
    /// The indicative tax, when asked and answered (Preview only).
    pub tax: Option<IndicativeTax>,
    /// Complete validated item keys per line, or an explicitly compatible legacy singleton.
    pub keys: BTreeMap<Uuid, Vec<OverlapKey>>,
    /// The run's budget, its per-port subrequest counts included.
    pub run: RunBudget,
    /// Every called port that did not answer, once per port, in the module's fixed diagnostic order (D-227).
    pub unavailable_kinds: Vec<(GatePort, Unavailable)>,
}

/// [`evaluation_request`], with a binding the codec cannot encode logged as a local defect and
/// then taken down the prerequisite-missing path (`None`, so `Skipped`): outcome, wire and
/// D-230 precedence are unchanged.
fn build_evaluation_request(
    basket: &Basket,
    bindings: &BTreeMap<Uuid, Vec<AcceptedBinding>>,
    terms: Option<&[ResolvedTerms]>,
    market: Option<&Market>,
    policy: Option<&AcceptancePolicy>,
) -> Option<OrderEvaluationRequest> {
    match evaluation_request(basket, bindings, terms, market, policy) {
        Ok(request) => request,
        Err(
            CommercialError::Unsupported { line_id, what }
            | CommercialError::Incomplete { line_id, what }
            | CommercialError::Mismatch { line_id, what },
        ) => {
            tracing::error!(
                target: "orders.gate",
                %line_id,
                what,
                "evaluation request not built: binding cannot be encoded"
            );
            None
        }
    }
}

/// The gate orchestrator: the ports, their call discipline and the D-180 reader.
#[derive(Clone)]
pub struct Orchestrator {
    ports: Arc<GatePorts>,
    budget: Arc<Budget>,
    inflight: Arc<dyn InflightClaims>,
    /// The catalog port: `ports.catalog` unless a test substitutes a double.
    catalog: Arc<dyn CatalogReads>,
}

impl std::fmt::Debug for Orchestrator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Orchestrator").finish_non_exhaustive()
    }
}

/// The unavailable answers of one run, each port logged and signalled once (D-227, 03 §3.8).
struct Unanswered<'a> {
    kinds: Vec<(GatePort, Unavailable)>,
    signals: &'a dyn GateSignals,
    /// The basket's seller: the catalog the catalog ports read.
    seller: Uuid,
    /// Whether the run already reported a denial of the seller's catalog (once per run).
    denied: bool,
}

impl<'a> Unanswered<'a> {
    fn new(signals: &'a dyn GateSignals, seller: Uuid) -> Self {
        Self {
            kinds: Vec::new(),
            signals,
            seller,
            denied: false,
        }
    }

    /// Keep `port`'s first unavailable answer of the run; `false` when one is already kept.
    fn record(&mut self, port: GatePort, kind: Unavailable) -> bool {
        if self.kinds.iter().any(|(p, _)| *p == port) {
            return false;
        }
        tracing::info!(
            target: "orders.gate.unavailable_kind",
            port = port.token(),
            kind = ?kind,
            "bss-orders: a gate port did not answer"
        );
        self.signals.port_unavailable(port, kind);
        self.kinds.push((port, kind));
        true
    }

    /// [`Self::record`] for a catalog port; the run's first denial is also a denial of the
    /// seller's catalog, reported once per run however many catalog ports were denied.
    fn record_catalog(&mut self, port: GatePort, kind: Unavailable) {
        if self.record(port, kind) && kind == Unavailable::Denied && !self.denied {
            self.denied = true;
            self.signals.catalog_denial(self.seller);
        }
    }

    fn note<T>(&mut self, port: GatePort, answer: &PortAnswer<T>) {
        if let PortAnswer::Unavailable(kind) = answer {
            self.record(port, *kind);
        }
    }

    /// The catalog's three ports, from every line's answer in line-id order.
    fn note_catalog(&mut self, catalog: &BTreeMap<Uuid, CatalogAnswer>) {
        for answer in catalog.values() {
            if let Err(ReadFailure::Unavailable(kind)) = &answer.current {
                self.record_catalog(GatePort::CurrentRevision, *kind);
            }
            if let Err(ReadFailure::Unavailable(kind)) = &answer.resolved {
                self.record_catalog(GatePort::Resolve, *kind);
            }
            for sku in answer.skus.values() {
                if let PortAnswer::Unavailable(kind) = sku {
                    self.record_catalog(GatePort::SkuRead, *kind);
                }
            }
        }
    }
}

/// Both successful local read results; errors remain separate from these facts.
type InflightAnswers = (BTreeMap<String, u32>, BTreeSet<String>);

/// Completed provider answers survive a failed dependent local read. No failed read is
/// represented by a successful empty count/claim set, and only a successful chain can assess.
struct OverlapAnswers {
    keys: Option<PortAnswer<Vec<OverlapKey>>>,
    occupancy: Option<OccupancyAnswer>,
    inflight: Result<InflightAnswers, OrchestrateError>,
}

/// `Cancelled` once the caller cancelled.
fn checkpoint(cancel: &CancellationToken) -> Result<(), OrchestrateError> {
    if cancel.is_cancelled() {
        Err(OrchestrateError::Cancelled)
    } else {
        Ok(())
    }
}

impl Orchestrator {
    /// An orchestrator over `ports`, `budget` and the D-180 reader.
    #[must_use]
    pub fn new(
        ports: Arc<GatePorts>,
        budget: Arc<Budget>,
        inflight: Arc<dyn InflightClaims>,
    ) -> Self {
        let catalog: Arc<dyn CatalogReads> = ports.catalog.clone();
        Self {
            ports,
            budget,
            inflight,
            catalog,
        }
    }

    /// Substitute the catalog port (in-crate tests count its calls).
    #[cfg(test)]
    #[cfg_attr(
        not(feature = "gate-dev-stubs"),
        expect(
            dead_code,
            reason = "only the `gate-dev-stubs` storage and orchestrate tests use it"
        )
    )]
    #[must_use]
    pub(crate) fn with_catalog(mut self, catalog: Arc<dyn CatalogReads>) -> Self {
        self.catalog = catalog;
        self
    }

    /// The gear-wide port call discipline the run's adapters share.
    #[must_use]
    pub fn budget(&self) -> &Budget {
        &self.budget
    }

    /// Run the gate over `basket` as `ctx` (module doc for the order). The D-180 addend takes
    /// no caller scope: it is a privileged count ([`InflightClaims`]).
    ///
    /// # Errors
    /// [`OrchestrateError::Cancelled`] when `cancel` fires; [`OrchestrateError::Integration`]
    /// when an in-flight read fails or the basket breaks a vector rule (a duplicated line id).
    pub async fn run(
        &self,
        ctx: &SecurityContext,
        basket: &Basket,
        cancel: &CancellationToken,
    ) -> Result<Resolved, OrchestrateError> {
        let run = RunBudget::new();
        let plan = plan(basket);
        let ports = &self.ports;
        let axes = basket.axes;
        let signals = self.budget.signals().as_ref();
        let flow = if plan.tax {
            Flow::Preview
        } else {
            Flow::Submit
        };
        let mut down = Unanswered::new(signals, axes.seller);
        checkpoint(cancel)?;

        // Wave A: these reads carry no payer-dependent commercial facts.
        let (identity, catalog, acceptance_policy) = tokio::join!(
            ports
                .identity
                .payer_profile(ctx, &plan.identity, &run, cancel),
            self.catalog.read_lines(
                axes.seller,
                &plan.catalog,
                basket.resolve_date,
                &run,
                cancel,
            ),
            ports.acceptance_policy.policy(&plan.policy, &run, cancel),
        );
        down.note(GatePort::IdentityProfile, &identity);
        checkpoint(cancel)?;
        let confirmed = gate_after_identity(&identity) == IdentityGate::Confirmed;
        let bindings = line_bindings(basket, &catalog);
        let resolve_answers = catalog
            .iter()
            .filter_map(|(line, answer)| Some((*line, Arc::clone(answer.resolved.as_ref().ok()?))))
            .collect();

        // Wave B: the relationship gates every payer-dependent branch (D-215/D-228).
        let (contracts, billing_terms, overlap) = tokio::join!(
            async {
                if !confirmed {
                    None
                } else if plan.contracts.is_empty() {
                    Some(PortAnswer::Answered(BTreeMap::new()))
                } else {
                    Some(
                        ports
                            .contracts
                            .resolve(ctx, &plan.contracts, &run, cancel)
                            .await,
                    )
                }
            },
            async {
                if !confirmed {
                    None
                } else if plan.terms.is_empty() {
                    Some(PortAnswer::Answered(Vec::new()))
                } else {
                    Some(
                        resolve_terms(ports.billing_terms.as_ref(), &plan.terms, &run, cancel)
                            .await,
                    )
                }
            },
            async {
                if confirmed {
                    self.overlap(ctx, basket, &bindings, &run, cancel).await
                } else {
                    OverlapAnswers {
                        keys: None,
                        occupancy: None,
                        inflight: Ok((BTreeMap::new(), BTreeSet::new())),
                    }
                }
            },
        );
        // Preserve historical diagnostics rather than completion order across both waves.
        if let Some(answer) = &contracts {
            down.note(GatePort::ContractResolution, answer);
        }
        down.note_catalog(&catalog);
        let OverlapAnswers {
            keys: overlap_keys,
            occupancy,
            inflight,
        } = overlap;
        if let Some(answer) = &overlap_keys {
            down.note(GatePort::OverlapKey, answer);
        }
        if let Some(answer) = &occupancy {
            down.note(GatePort::Occupancy, answer);
        }
        if let Some(answer) = &billing_terms {
            down.note(GatePort::BillingTerms, answer);
        }
        down.note(GatePort::AcceptancePolicy, &acceptance_policy);
        // Completed siblings are diagnosed even when the local read failed. Cancellation
        // still wins over that failure, and neither path starts the next dependency wave.
        checkpoint(cancel)?;
        let (inflight_other_orders, inflight_same_tuple) = inflight?;

        // Wave C: neither assessment nor evaluation suppresses an independent result.
        let market = payer_market(&identity);
        let terms = billing_terms
            .as_ref()
            .and_then(PortAnswer::answered)
            .map(Vec::as_slice);
        let query = assessment_query(basket, market.as_ref(), terms);
        let request = build_evaluation_request(
            basket,
            &bindings,
            terms,
            market.as_ref(),
            acceptance_policy.answered().and_then(Option::as_ref),
        );
        let (assessment, evaluation) =
            tokio::join!(ports.assessment.assess(&query, &run, cancel), async {
                match request.as_ref() {
                    Some(request) => {
                        let answer = ports.evaluation.evaluate(ctx, request, &run, cancel).await;
                        Some(match answer {
                            PortAnswer::Answered(value) => {
                                match crate::domain::gate::orchestrate::verify_evaluation(
                                    request, &value,
                                ) {
                                    Ok(()) => PortAnswer::Answered(value),
                                    Err(kind) => PortAnswer::Unavailable(kind),
                                }
                            }
                            other => other,
                        })
                    }
                    None => None,
                }
            },);
        down.note(GatePort::Assessment, &assessment);
        if let Some(answer) = &evaluation {
            down.note(GatePort::Evaluation, answer);
        }
        checkpoint(cancel)?;

        // Wave D: indicative tax: Preview only, over an answered evaluation.
        let tax_request = match (&evaluation, plan.tax) {
            (Some(PortAnswer::Answered(e)), true) => tax_query(basket, e),
            _ => None,
        };
        let tax = match tax_request {
            Some(query) => {
                let answer = ports
                    .indicative_tax
                    .estimate(ctx, &query, &run, cancel)
                    .await;
                down.note(GatePort::IndicativeTax, &answer);
                checkpoint(cancel)?;
                answer.answered().cloned()
            }
            None => None,
        };

        let resolved_terms = terms
            .into_iter()
            .flatten()
            .map(|t| (t.line_id, t.clone()))
            .collect();
        let policy = acceptance_policy.answered().cloned().flatten();
        let keys = overlap_keys
            .as_ref()
            .and_then(PortAnswer::answered)
            .map(|answered| {
                crate::domain::gate::item_keys::validated_line_keys(
                    &overlap_key_lines(basket, &bindings),
                    answered.as_slice(),
                )
            })
            .unwrap_or_default();
        let evaluated = evaluation.as_ref().and_then(|e| e.answered().cloned());
        let answers = Answers {
            catalog,
            identity,
            contracts,
            overlap_keys,
            occupancy,
            inflight_same_tuple,
            inflight_other_orders,
            billing_terms,
            acceptance_policy,
            assessment,
            evaluation,
        };
        let assessment = assess(&assemble(basket, answers))?;
        signals.run(flow, &RunSignals::of(&assessment), run.elapsed());
        Ok(Resolved {
            assessment,
            bindings,
            resolve_answers,
            market,
            terms: resolved_terms,
            policy,
            evaluation_request: request,
            evaluation: evaluated,
            tax,
            keys,
            run,
            unavailable_kinds: down.kinds,
        })
    }

    /// Step 4 with the relationship confirmed: overlap keys, then occupancy per distinct key,
    /// then the D-180 addend for those keys. Both are asked only for the keys of lines
    /// Validated complete item collections only; malformed batches generate no occupancy reads, so
    /// `assess` reads neither its occupancy nor its addend.
    async fn overlap(
        &self,
        ctx: &SecurityContext,
        basket: &Basket,
        bindings: &BTreeMap<Uuid, Vec<AcceptedBinding>>,
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> OverlapAnswers {
        let axes = basket.axes;
        let lines = overlap_key_lines(basket, bindings);
        let keys = if lines.is_empty() {
            PortAnswer::Answered(Vec::new())
        } else {
            self.ports
                .overlap_keys
                .keys(ctx, axes.payer, Some(axes.resource), &lines, run, cancel)
                .await
        };
        let mut occupancy = None;
        let inflight = async {
            checkpoint(cancel)?;
            let PortAnswer::Answered(answered) = &keys else {
                // No key to ask occupancy about: assessment reads the key port's reason.
                return Ok((BTreeMap::new(), BTreeSet::new()));
            };
            let kept: Vec<OverlapKey> =
                crate::domain::gate::item_keys::read_keys(&lines, answered).unwrap_or_default();
            let queries = occupancy_queries(&kept, &axes);
            occupancy = Some(if queries.is_empty() {
                PortAnswer::Answered(Vec::new())
            } else {
                self.ports
                    .occupancy
                    .get_overlap_occupancy(ctx, &queries, run, cancel)
                    .await
            });
            checkpoint(cancel)?;
            let distinct: BTreeSet<&str> =
                kept.iter().map(|k| k.overlap_scope_key.as_str()).collect();
            let parties = ClaimParties {
                payer_tenant_id: axes.payer,
                resource_tenant_id: axes.resource,
            };
            let scope_keys: Vec<String> = distinct.into_iter().map(str::to_owned).collect();
            let counts = self
                .inflight
                .count_other_orders(parties, &scope_keys, basket.target.order_id())
                .await
                .map_err(|e| OrchestrateError::Integration {
                    what: "in-flight line count",
                    source: Box::new(e),
                })?;
            checkpoint(cancel)?;
            let same_tuple = self
                .inflight
                .held_same_tuple(parties, &scope_keys, basket.target.order_id())
                .await
                .map_err(|e| OrchestrateError::Integration {
                    what: "in-flight tuple lookup",
                    source: Box::new(e),
                })?;
            checkpoint(cancel)?;
            Ok((counts, same_tuple))
        }
        .await;
        OverlapAnswers {
            keys: Some(keys),
            occupancy,
            inflight,
        }
    }
}

/// One in-memory in-flight claim: `(payer, resource, key, order)`.
#[cfg(test)]
#[cfg_attr(
    not(feature = "gate-dev-stubs"),
    expect(
        dead_code,
        reason = "only the `gate-dev-stubs` storage and orchestrate tests use it"
    )
)]
type InflightEntry = (Uuid, Uuid, String, Uuid);

/// An in-memory D-180 reader for the in-crate tests: one entry per current-version line that
/// carries a live claim's key, as `(payer, resource, key, order)` tuples (a two-line order holds
/// two entries); the private scope is not modelled.
#[cfg(test)]
#[cfg_attr(
    not(feature = "gate-dev-stubs"),
    expect(
        dead_code,
        reason = "only the `gate-dev-stubs` storage and orchestrate tests use it"
    )
)]
#[derive(Debug, Default)]
pub struct InMemoryInflightClaims {
    pub claims: parking_lot::Mutex<Vec<InflightEntry>>,
    pub reads: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
#[async_trait]
impl InflightClaims for InMemoryInflightClaims {
    async fn count_other_orders(
        &self,
        tuple: ClaimParties,
        keys: &[String],
        exclude_order: Option<Uuid>,
    ) -> Result<BTreeMap<String, u32>, InflightReadError> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut counts: BTreeMap<String, u32> = keys.iter().map(|k| (k.clone(), 0)).collect();
        for (p, r, k, order) in self.claims.lock().iter() {
            if *p == tuple.payer_tenant_id
                && *r != tuple.resource_tenant_id
                && Some(*order) != exclude_order
                && let Some(n) = counts.get_mut(k)
            {
                *n += 1;
            }
        }
        Ok(counts)
    }
    async fn held_same_tuple(
        &self,
        tuple: ClaimParties,
        keys: &[String],
        exclude_order: Option<Uuid>,
    ) -> Result<BTreeSet<String>, InflightReadError> {
        Ok(self
            .claims
            .lock()
            .iter()
            .filter(|(p, r, k, o)| {
                *p == tuple.payer_tenant_id
                    && *r == tuple.resource_tenant_id
                    && keys.contains(k)
                    && Some(*o) != exclude_order
            })
            .map(|(_, _, k, _)| k.clone())
            .collect())
    }
}

/// Ask the billing-terms port and refuse an answer that names a line twice or an unasked line
/// (R3, `verify_terms`) as `Unavailable(Malformed)` before any consumer reads it.
async fn resolve_terms(
    port: &dyn crate::infra::gate::ports::BillingTermsReads,
    queries: &[crate::domain::gate::answers::BillingTermsQuery],
    run: &RunBudget,
    cancel: &CancellationToken,
) -> PortAnswer<Vec<ResolvedTerms>> {
    match port.resolve(queries, run, cancel).await {
        PortAnswer::Answered(terms) => {
            match crate::domain::gate::orchestrate::verify_terms(queries, &terms) {
                Ok(()) => PortAnswer::Answered(terms),
                Err(kind) => PortAnswer::Unavailable(kind),
            }
        }
        other => other,
    }
}
