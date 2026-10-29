//! The `CatalogReads` port (D-187, F03 steps 2-3): a basket's Pricing `current_revision` and
//! `resolve` answers and the Products SKU reads of its bound slots, read as the seller-scoped
//! Orders service principal (D-194) under the run's budget (F03 §3.3).
//!
//! `current_revision` may promote a due scheduled revision and so write in Pricing (D-187):
//! callers never invoke [`CatalogReads::read_lines`] or [`CatalogReads::read_line`] inside an
//! Orders database transaction.
use super::authority::{AuthorityError, CommercialAuthority, WarnOnce};
use super::budget::{Budget, CallError, RunBudget, bounded, to_unavailable};
use crate::domain::gate::ports::{GatePort, PortAnswer, Unavailable};
use crate::domain::gate::pricing_map::{
    CatalogAnswer, LineCatalogRequest, ReadFailure, classify_read_error, selected_slot_binding,
};
use async_trait::async_trait;
use bss_pricing_sdk::read::{CatalogRef, PlanQuery, PricingReadV1, ResolveQuery, ResolvedBindings};
use bss_products_sdk::api::ProductsClient;
use bss_products_sdk::models::Sku;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use time::Date;
use tokio_util::sync::CancellationToken;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// The catalog reads of a basket.
///
/// Callers use [`Self::read_lines`] for a basket, so the basket shares one authority exchange
/// and each distinct plan, revision and SKU is read once; [`Self::read_line`] is for single-line
/// callers. Never call either inside an Orders transaction: `current_revision` may write in
/// Pricing.
#[async_trait]
pub trait CatalogReads: Send + Sync {
    /// The line's `current_revision`, `resolve` and SKU answers, read for `seller_tenant_id`.
    async fn read_line(
        &self,
        seller_tenant_id: Uuid,
        req: &LineCatalogRequest,
        resolve_date: Date,
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> CatalogAnswer;

    /// Every line's answer, keyed by `line_id` (a repeated line id keeps its last request's
    /// answer), read for `seller_tenant_id`. The default reads the lines one by one.
    async fn read_lines(
        &self,
        seller_tenant_id: Uuid,
        lines: &[LineCatalogRequest],
        resolve_date: Date,
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> BTreeMap<Uuid, CatalogAnswer> {
        let mut out = BTreeMap::new();
        for req in lines {
            let answer = self
                .read_line(seller_tenant_id, req, resolve_date, run, cancel)
                .await;
            out.insert(req.line_id, answer);
        }
        out
    }
}

/// The production adapter when the Pricing or Products client is absent: every answer is the
/// carried unavailability; nothing is called.
#[derive(Debug, Clone, Copy)]
pub struct CatalogReadsUnavailable(
    /// The unavailability every answer carries.
    pub Unavailable,
);

#[async_trait]
impl CatalogReads for CatalogReadsUnavailable {
    async fn read_line(
        &self,
        _seller: Uuid,
        _req: &LineCatalogRequest,
        _resolve_date: Date,
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> CatalogAnswer {
        unanswered(self.0)
    }
}

/// Every answer of the line unavailable for `why`, with no SKU read.
fn unanswered(why: Unavailable) -> CatalogAnswer {
    CatalogAnswer {
        current: Err(ReadFailure::Unavailable(why)),
        resolved: Err(ReadFailure::Unavailable(why)),
        skus: BTreeMap::new(),
    }
}

/// A failed bounded Pricing read as the typed failure: a permanent callee error is classified
/// (its business answers and `PermissionDenied`, which is `Unavailable(Denied)`, an operator-only
/// diagnostic behind `pricing-revision-unavailable`, never a buyer 403: F03 §4.6, D-122); every
/// other failure (deadline, an unrecovered transient error, backpressure, cancellation, the
/// subrequest cap) is the port outage `to_unavailable` names.
fn read_failure(e: &CallError) -> ReadFailure {
    match e {
        CallError::Permanent(error) => classify_read_error(error),
        other => ReadFailure::Unavailable(to_unavailable(other)),
    }
}

/// The answer of a plan or revision the shared reads did not cover. Every plan and revision of
/// the call is read, so this never happens; it fails closed if it did.
fn not_read<T>() -> Result<T, ReadFailure> {
    Err(ReadFailure::Unavailable(Unavailable::Outage))
}

/// The distinct SKUs bound to the line's selected slots. Only the selected slots are read: the
/// mapping reads no other SKU, and the resolved matrix may be far larger than the request. A
/// resolve of another plan or revision binds nothing here: the mapping treats it as malformed.
fn bound_skus(req: &LineCatalogRequest, rb: &ResolvedBindings) -> BTreeSet<Uuid> {
    if rb.plan_id != req.plan_id || rb.revision_id != req.revision_id {
        return BTreeSet::new();
    }
    req.items
        .iter()
        .filter_map(|i| {
            selected_slot_binding(&rb.cells, (i.item_id, i.selected_dim_value.as_deref()))
        })
        .map(|b| b.sku_id)
        .collect()
}

/// Pricing and Products reads as the seller's Orders service principal.
pub struct PricingCatalogReads {
    pricing: Arc<dyn PricingReadV1>,
    products: Arc<dyn ProductsClient>,
    authority: Arc<dyn CommercialAuthority>,
    budget: Arc<Budget>,
    /// Sellers already warned about as not provisioned (one warning per seller per process).
    warned: WarnOnce,
}

impl std::fmt::Debug for PricingCatalogReads {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PricingCatalogReads")
            .field("budget", &self.budget)
            .finish_non_exhaustive()
    }
}

impl PricingCatalogReads {
    /// The adapter over the Pricing and Products clients, presenting `authority`'s principal.
    #[must_use]
    pub fn new(
        pricing: Arc<dyn PricingReadV1>,
        products: Arc<dyn ProductsClient>,
        authority: Arc<dyn CommercialAuthority>,
        budget: Arc<Budget>,
    ) -> Self {
        Self {
            pricing,
            products,
            authority,
            budget,
            warned: WarnOnce::default(),
        }
    }

    /// The line's answer when no service context exists: `Missing` for an unprovisioned or a
    /// misprovisioned seller (warned once per seller, naming the seller only), an outage for a
    /// failed exchange.
    fn without_authority(&self, seller: Uuid, e: &AuthorityError) -> CatalogAnswer {
        match e {
            AuthorityError::NotProvisioned => {
                if self.warned.first(seller) {
                    tracing::warn!(
                        target: "orders.gate.authority",
                        seller_tenant_id = %seller,
                        "no commercial identity is configured for the seller; catalog reads are \
                         unavailable"
                    );
                }
                unanswered(Unavailable::Missing)
            }
            AuthorityError::Misprovisioned => {
                if self.warned.first(seller) {
                    tracing::warn!(
                        target: "orders.gate.authority",
                        seller_tenant_id = %seller,
                        "the seller's commercial identity is misprovisioned: the exchanged \
                         principal is homed in another tenant or is not a service subject \
                         (D-194, D-242); catalog reads are unavailable"
                    );
                }
                unanswered(Unavailable::Missing)
            }
            AuthorityError::Deadline => unanswered(Unavailable::Deadline),
            AuthorityError::Exchange(_) | AuthorityError::Unavailable => {
                unanswered(Unavailable::Outage)
            }
        }
    }

    /// Each SKU read once, concurrently; the port's in-run limiter bounds what is in flight.
    async fn read_skus(
        &self,
        ctx: &SecurityContext,
        seller: Uuid,
        ids: BTreeSet<Uuid>,
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> BTreeMap<Uuid, PortAnswer<Sku>> {
        let reads = ids.into_iter().map(|id| async move {
            let answer = bounded(&self.budget, run, GatePort::SkuRead, cancel, || {
                self.products.get_sku(ctx, seller, id)
            })
            .await;
            let answer = answer.map_or_else(
                |e| PortAnswer::Unavailable(to_unavailable(&e)),
                PortAnswer::Answered,
            );
            (id, answer)
        });
        futures::future::join_all(reads).await.into_iter().collect()
    }
}

#[async_trait]
impl CatalogReads for PricingCatalogReads {
    async fn read_line(
        &self,
        seller_tenant_id: Uuid,
        req: &LineCatalogRequest,
        resolve_date: Date,
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> CatalogAnswer {
        let mut answers = self
            .read_lines(
                seller_tenant_id,
                std::slice::from_ref(req),
                resolve_date,
                run,
                cancel,
            )
            .await;
        answers
            .remove(&req.line_id)
            .unwrap_or_else(|| unanswered(Unavailable::Outage))
    }

    /// One authority exchange for the call; `current_revision` once per distinct plan, then
    /// `resolve` once per distinct revision, then each distinct bound SKU once; the shared
    /// answers are cloned into each line. Every read runs under the port's in-run limiter.
    async fn read_lines(
        &self,
        seller_tenant_id: Uuid,
        lines: &[LineCatalogRequest],
        resolve_date: Date,
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> BTreeMap<Uuid, CatalogAnswer> {
        if lines.is_empty() {
            return BTreeMap::new();
        }
        // The exchange is bounded by `DeadlineAuthority` (`CatalogBinding::new`) and the run's
        // cancellation. It is not a subrequest: the cap counts catalog reads, and the exchange
        // reads no catalog.
        let exchange = tokio::select! {
            biased;
            () = cancel.cancelled() => None,
            r = self.authority.service_context(seller_tenant_id) => Some(r),
        };
        let ctx = match exchange {
            Some(Ok(ctx)) => ctx,
            Some(Err(e)) => {
                let answer = self.without_authority(seller_tenant_id, &e);
                return lines.iter().map(|l| (l.line_id, answer.clone())).collect();
            }
            None => {
                let answer = unanswered(Unavailable::Outage);
                return lines.iter().map(|l| (l.line_id, answer.clone())).collect();
            }
        };
        let ctx = &ctx;
        // D-187: the catalog is the seller's on every query, never the buyer's tenant.
        let catalog = CatalogRef {
            tenant_id: seller_tenant_id,
        };
        let catalog = &catalog;
        // Sequential phases, not joined: `current_revision` may promote a requested revision,
        // which `resolve` must then see.
        let plans: BTreeSet<Uuid> = lines.iter().map(|l| l.plan_id).collect();
        let currents = plans.into_iter().map(|plan_id| async move {
            let answer = bounded(&self.budget, run, GatePort::CurrentRevision, cancel, || {
                self.pricing.current_revision(
                    ctx,
                    PlanQuery {
                        catalog: catalog.clone(),
                        plan_id,
                    },
                )
            })
            .await
            .map_err(|e| read_failure(&e));
            (plan_id, answer)
        });
        let currents: BTreeMap<Uuid, _> = futures::future::join_all(currents)
            .await
            .into_iter()
            .collect();
        let revisions: BTreeSet<Uuid> = lines.iter().map(|l| l.revision_id).collect();
        let resolves = revisions.into_iter().map(|revision_id| async move {
            let answer = bounded(&self.budget, run, GatePort::Resolve, cancel, || {
                self.pricing.resolve(
                    ctx,
                    ResolveQuery {
                        catalog: catalog.clone(),
                        revision_id,
                        date: resolve_date,
                        item_id: None,
                        pins: Vec::new(),
                    },
                )
            })
            .await
            .map_err(|e| read_failure(&e))
            .map(Arc::new);
            (revision_id, answer)
        });
        let resolves: BTreeMap<Uuid, Result<Arc<ResolvedBindings>, ReadFailure>> =
            futures::future::join_all(resolves)
                .await
                .into_iter()
                .collect();
        let bound: Vec<BTreeSet<Uuid>> = lines
            .iter()
            .map(|l| match resolves.get(&l.revision_id) {
                Some(Ok(rb)) => bound_skus(l, rb),
                _ => BTreeSet::new(),
            })
            .collect();
        let all_skus: BTreeSet<Uuid> = bound.iter().flatten().copied().collect();
        let skus = self
            .read_skus(ctx, seller_tenant_id, all_skus, run, cancel)
            .await;
        lines
            .iter()
            .zip(bound)
            .map(|(l, ids)| {
                let answer = CatalogAnswer {
                    current: currents.get(&l.plan_id).cloned().unwrap_or_else(not_read),
                    resolved: resolves
                        .get(&l.revision_id)
                        .cloned()
                        .unwrap_or_else(not_read),
                    skus: ids
                        .into_iter()
                        .filter_map(|id| skus.get(&id).map(|a| (id, a.clone())))
                        .collect(),
                };
                (l.line_id, answer)
            })
            .collect()
    }
}
