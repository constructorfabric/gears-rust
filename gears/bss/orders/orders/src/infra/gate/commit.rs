//! Submit phase C (S3-12; 03 §3.6 step 15–27; D-188 steps 4–6, D-192, D-216): the commit of a
//! reserved commercial attempt through the one engine transaction. The preparation reads only
//! the attempt the engine loaded under the presented owner ([`PreparationView::frozen`]), its
//! frozen requests and evaluation and its recorded receipts; it never re-runs the gate.
//!
//! [`SubmitPreparation::prepare`], in order:
//! 1. the view's frozen execution must be this attempt (identity, candidate and frozen
//!    requests), else `Integration`; the frozen date basis is restored exactly
//!    ([`DateBasis::from_frozen`]);
//! 2. a draft revision other than the attempt's prepared one is left to the engine's precedence
//!    refusal (`version-conflict`, decided before any guard): nothing commercial is built;
//! 3. every line's recorded result is decoded. A recorded Pricing refusal settles the attempt
//!    refused through its registered guard ([`refusal_disposition`]); a line without a result
//!    when nothing refused is an integration failure;
//! 4. every receipt becomes the line's schema-2 pin ([`OrderPin::new`], the only constructor),
//!    bound to the committed row `(order, candidate, line)` and verified against the frozen
//!    query, the frozen request digest and the terms digest recomputed over the receipt's
//!    bindings ([`OrderPin::verify_selection`]) and, when the attempt froze the Rating request,
//!    against that request ([`verify_evaluated_query`], [`verify_evaluated_bindings`], D-197);
//!    any mismatch fails closed (`Integration`, D-192);
//! 5. the admitted line rows ([`admitted_line`] plus the commercial columns, the encoded pin and
//!    the claimed overlap key) and the `resolved_total` rows ([`total_rows`]) are built; an
//!    evaluation that cannot be stored refuses `evaluation-unavailable`, and one not in the
//!    frozen market currency is an integration failure (D-216);
//! 6. the guards: `gate.has-lines`, the shared structural guards, `gate.binding-admission`
//!    (a recorded lapsed hold, a single pin or the whole version over the byte limit, then
//!    every receipt's exclusive `hold_until` against the transition time),
//!    `capture.date-basis` and the composite gate;
//!    the version contribution names the reserved candidate, the claims are the frozen keys,
//!    the event is `OrderSubmitted`; the settlement carries the frozen gate report and the
//!    engine writes the frozen outcome vector as the run's diagnostics, whether this call
//!    assessed or recovered the attempt (D-231).
//!
//! The engine does the rest: claims, version, documents, diagnostics, aggregate, audit, event,
//! settlement and `finish_execution`.
use super::failure::{Ids, submit_integration};
use super::submit::structural_guards;
use super::writer::GateRun;
use crate::domain::contributions::{AggregateContribution, VersionContribution};
use crate::domain::dates::DateBasis;
use crate::domain::gate::commercial::{
    FrozenLine, FrozenRequests, ReceiptResult, RefusalDisposition, decode_receipt_result,
    digest_hex, persisted, refusal_disposition, verify_evaluated_bindings, verify_evaluated_query,
    version_capacity, version_row_bytes,
};
use crate::domain::gate::totals::{TotalRow, total_rows};
use crate::domain::guards::{GATE_COMPOSITE, GUARDS, InputPort};
use crate::domain::overlap::{ClaimParties, OverlapScopeKey, ProposedClaims};
use crate::domain::transition::{GuardBindings, GuardSubject, GuardVerdict, InputState, Inputs};
use crate::infra::dates::admitted_line;
use crate::infra::engine::documents::EventDetail;
use crate::infra::engine::{
    ChildDocuments, DocumentWriter, Preparation, PreparationView, PrepareError, Prepared,
};
use crate::infra::events::payload::{LineRef, OrderSubmitted};
use crate::infra::storage::entity::{draft_content, order_line, resolved_total};
use crate::infra::storage::repo::idempotency::Frozen;
use async_trait::async_trait;
use bss_orders_sdk::catalog::{Reason, Trigger};
use bss_orders_sdk::commercial::{
    AcceptedVersionRef, AssessmentRef, CodecError, OrderPin, SelectionEvidence,
};
use bss_orders_sdk::models::OrderVersion;
use bss_pricing_sdk::digest::terms_digest;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use time::OffsetDateTime;
use toolkit_db::secure::ScopeError;
use uuid::Uuid;

/// The transition-table row submit runs (`draft → submitted`).
const SUBMIT_ROW: u8 = 4;
/// The guard submit's at-least-one-line rule is registered under (03 step 1).
pub(super) const HAS_LINES: &str = "gate.has-lines";
/// The accepted-binding admission guard (03 step 15).
pub(super) const BINDING_ADMISSION: &str = "gate.binding-admission";
/// The date-basis guard (S2-10).
pub(super) const DATE_BASIS: &str = "capture.date-basis";

/// Submit's commit preparation (module doc).
pub struct SubmitPreparation {
    /// The reserved attempt.
    pub(super) attempt_id: Uuid,
    /// Its candidate version.
    pub(super) candidate_version: i32,
    /// Its stored `line_requests`, compared with the attempt the engine loaded.
    pub(super) line_requests: Value,
    /// The decoded frozen requests and order facts.
    pub(super) requests: FrozenRequests,
    /// The configured line cap.
    pub(super) line_cap: usize,
}

/// What one accepted line contributes.
struct AdmittedLine {
    row: order_line::Model,
    /// The persisted bytes of the line row ([`line_row_bytes`]), its encoded pin included.
    bytes: usize,
    hold_until: OffsetDateTime,
}

/// One accepted line's admission: its row, or a pin no version can hold.
enum Admission {
    Admitted(Box<AdmittedLine>),
    /// The line's schema-2 pin alone exceeds the version limit (`CodecError::TooLarge`).
    OverCapacity,
}

/// A recorded refusal or a commercial input the commit cannot admit, with where it settles.
#[derive(Clone, Copy)]
enum Blocked {
    Pricing(RefusalDisposition),
    /// The evaluation cannot be stored (D-216).
    Totals,
    /// One line's schema-2 pin alone exceeds the version byte limit (D-231 item 7).
    Capacity,
}

fn integration(ids: Ids, what: &'static str) -> PrepareError {
    submit_integration(what, ids, None);
    PrepareError::Integration
}

/// [`integration`] keeping the cause's `Display` (never stored content, see [`super::failure`]).
fn integration_with(ids: Ids, what: &'static str, cause: &dyn std::fmt::Display) -> PrepareError {
    submit_integration(what, ids, Some(cause));
    PrepareError::Integration
}

#[async_trait]
impl Preparation for SubmitPreparation {
    async fn prepare(&self, view: &PreparationView) -> Result<Prepared, PrepareError> {
        let ids = Ids::attempt(view.facts.order_id, self.attempt_id);
        let fail = |what: &'static str| integration(ids, what);
        // 1. The attempt the engine loaded under the presented owner, and its frozen basis.
        let Some(Frozen::Commercial(attempt)) = &view.frozen else {
            return Err(fail("commit without the reserved commercial attempt"));
        };
        if attempt.attempt_id != self.attempt_id
            || attempt.candidate_version != self.candidate_version
            || attempt.line_requests != self.line_requests
        {
            return Err(fail("the loaded attempt is not the reserved one"));
        }
        let basis = DateBasis::from_frozen(&attempt.date_policy_basis)
            .map_err(|e| integration_with(ids, "the frozen date basis does not restore", &e))?;
        let facts = &view.facts;
        let market = &self
            .requests
            .lines
            .first()
            .ok_or_else(|| fail("an attempt without lines"))?
            .query
            .market;
        if self
            .requests
            .lines
            .iter()
            .any(|l| l.query.market != *market)
        {
            return Err(fail("frozen lines disagree on the market"));
        }
        let mut prepared = Prepared::new(AggregateContribution::Version(VersionContribution {
            candidate: self.candidate_version,
            payer_tenant_id: facts.payer_tenant_id,
            category: facts.category.clone(),
            contract_id: facts.contract_id,
            market_currency: Some(market.currency.clone()),
            market_region: market.region.clone(),
            amendment_reason: None,
        }));
        // 2. A changed draft: the engine refuses by precedence before any guard.
        if view.prepared_draft_revision != attempt.prepared_draft_revision {
            return Ok(prepared);
        }
        let drafts: BTreeMap<Uuid, &draft_content::Model> =
            view.draft_lines.iter().map(|l| (l.line_id, l)).collect();
        if drafts.len() != self.requests.lines.len()
            || self
                .requests
                .lines
                .iter()
                .any(|l| !drafts.contains_key(&l.line_id))
        {
            return Err(fail("the draft lines are not the frozen lines"));
        }
        // 3. The recorded results.
        let mut blocked = None;
        let mut receipts = Vec::with_capacity(self.requests.lines.len());
        for line in &self.requests.lines {
            match attempt.receipt_results.get(line.line_id.to_string()) {
                Some(value) => match decode_receipt_result(value).map_err(|e| {
                    integration_with(
                        ids.line(line.line_id),
                        "a recorded receipt result does not decode",
                        &e,
                    )
                })? {
                    ReceiptResult::Accepted(receipt) => receipts.push((line, *receipt)),
                    ReceiptResult::Refused(reason) => {
                        blocked.get_or_insert(Blocked::Pricing(refusal_disposition(reason)));
                    }
                },
                None if blocked.is_some() => {}
                None => return Err(fail("a line without a recorded result")),
            }
        }
        // 4–5. Pins, line rows and totals, only when nothing refused.
        let mut lines = Vec::new();
        let mut totals = Vec::new();
        if blocked.is_none() {
            for (line, receipt) in receipts {
                let draft = drafts.get(&line.line_id).ok_or_else(|| {
                    integration(ids.line(line.line_id), "a frozen line without its draft")
                })?;
                match self.admitted(line, draft, receipt, facts.resource_tenant_id, &basis)? {
                    Admission::Admitted(admitted) => lines.push(*admitted),
                    Admission::OverCapacity => {
                        lines.clear();
                        blocked = Some(Blocked::Capacity);
                        break;
                    }
                }
            }
            if blocked.is_none() {
                let line_ids: Vec<Uuid> = self.requests.lines.iter().map(|l| l.line_id).collect();
                match total_rows(&self.requests.order.evaluation, &line_ids, &market.currency) {
                    Ok(rows) => totals = rows,
                    Err(error) if error.is_contract_defect() => {
                        return Err(integration_with(
                            ids,
                            "the frozen evaluation is not in the market currency (D-216)",
                            &error,
                        ));
                    }
                    Err(error) => {
                        // D-216: a fraction without a terminating decimal, or a display finer
                        // than the currency scale.
                        tracing::warn!(
                            target: "orders.gate.submit",
                            error = %error,
                            "bss-orders: the frozen evaluation cannot be stored as submit totals"
                        );
                        blocked = Some(Blocked::Totals);
                    }
                }
            }
        }
        // 6. Guards, inputs, contribution, claims, documents and event. The version limit
        // counts every persisted member: the line rows (pins included), the totals rows and the
        // version row.
        let version_row = match &prepared.aggregate {
            AggregateContribution::Version(version) => {
                version_row_bytes(version, Trigger::Submit.token())
            }
            _ => return Err(fail("submit without a version contribution")),
        };
        let persisted: Vec<usize> = lines
            .iter()
            .map(|l| l.bytes)
            .chain(totals.iter().map(TotalRow::persisted_bytes))
            .chain([version_row])
            .collect();
        let capacity = match super::submit::locked_consumed_item_capacity(view)? {
            GuardVerdict::Pass => version_capacity(persisted),
            failure @ GuardVerdict::Fail(_) => failure,
        };
        let earliest_hold = lines.iter().map(|l| l.hold_until).min();
        prepared.inputs = row_inputs_resolved();
        if let Some(Blocked::Pricing(RefusalDisposition::Unevaluable(reason))) = blocked {
            prepared.inputs = prepared.inputs.with(
                InputPort::CatalogPredicates,
                InputState::Unresolvable(reason),
            );
        }
        if let Some(Blocked::Totals) = blocked {
            prepared.inputs = prepared.inputs.with(
                InputPort::Evaluation,
                InputState::Unresolvable(Reason::EvaluationUnavailable),
            );
        }
        prepared.guards = self.guards(view, blocked, capacity, earliest_hold, basis);
        let keys = frozen_scope_keys(&self.requests.lines, ids)?;
        prepared.claims = Some(ProposedClaims::new(
            ClaimParties {
                payer_tenant_id: facts.payer_tenant_id,
                resource_tenant_id: facts.resource_tenant_id,
            },
            keys,
        ));
        if blocked.is_none() {
            prepared.documents = Some(Arc::new(SubmitDocuments {
                lines: lines.into_iter().map(|l| l.row).collect(),
                totals,
                assessment_id: self.requests.order.assessment_id,
                version: self.candidate_version,
            }));
        }
        prepared.event = Some(Arc::new(EventDetail(OrderSubmitted {
            lines: self
                .requests
                .lines
                .iter()
                .map(|l| LineRef { line_id: l.line_id })
                .collect(),
            acceptance: None,
        })));
        // The frozen run: a fresh and a recovered commit settle the same report and write the
        // same diagnostics (D-231).
        let order = &self.requests.order;
        let axes = &self
            .requests
            .lines
            .first()
            .ok_or_else(|| fail("an attempt without lines"))?
            .query
            .tenant_axes;
        // The engine settles the report as its wire value (`Prepared::assessment`).
        prepared.assessment = Some(
            serde_json::to_value(&order.report).map_err(|_| integration(ids, "frozen report"))?,
        );
        prepared.assessment_id = Some(order.assessment_id);
        let run: Arc<GateRun> = Arc::new(GateRun {
            overlap_keys: self
                .requests
                .lines
                .iter()
                .map(|l| {
                    (
                        l.line_id,
                        l.scopes().into_iter().map(str::to_owned).collect(),
                    )
                })
                .collect(),
            run_id: order.assessment_id,
            resource_tenant_id: axes.resource_tenant_id,
            seller_tenant_id: axes.seller_tenant_id,
            payer_tenant_id: axes.payer_tenant_id,
            correlation_id: order.correlation_id,
            evaluated_at: order.assessed_at,
            vector: order.vector.clone(),
        });
        prepared.diagnostics = Some(run);
        Ok(prepared)
    }
}

fn frozen_scope_keys(lines: &[FrozenLine], ids: Ids) -> Result<Vec<OverlapScopeKey>, PrepareError> {
    lines
        .iter()
        .flat_map(|line| line.scopes().into_iter().map(move |key| (line, key)))
        .map(|(line, key)| {
            OverlapScopeKey::try_from(key.to_owned()).map_err(|e| {
                integration_with(
                    ids.line(line.line_id),
                    "a frozen overlap key is invalid",
                    &e,
                )
            })
        })
        .collect()
}

impl SubmitPreparation {
    /// The commit preparation of `attempt` as phase C builds it (storage tests drive phase C
    /// for a chosen owner).
    #[cfg(test)]
    #[cfg_attr(
        not(feature = "gate-dev-stubs"),
        expect(
            dead_code,
            reason = "only the `gate-dev-stubs` storage and orchestrate tests use it"
        )
    )]
    pub fn for_attempt(
        attempt: &crate::infra::storage::entity::commercial_attempt::Model,
        line_cap: usize,
    ) -> Option<Self> {
        Some(Self {
            attempt_id: attempt.attempt_id,
            candidate_version: attempt.candidate_version,
            line_requests: attempt.line_requests.clone(),
            requests: crate::domain::gate::commercial::decode_line_requests(&attempt.line_requests)
                .ok()?,
            line_cap,
        })
    }

    /// One accepted line: its verified pin and admitted row (module doc, steps 4–5).
    fn admitted(
        &self,
        line: &FrozenLine,
        draft: &draft_content::Model,
        receipt: bss_pricing_sdk::acceptance::AcceptanceReceipt,
        resource_tenant_id: Uuid,
        basis: &DateBasis,
    ) -> Result<Admission, PrepareError> {
        let order = &self.requests.order;
        let ids = Ids {
            order_id: Some(line.query.order_id),
            attempt_id: Some(self.attempt_id),
            line_id: Some(line.line_id),
        };
        let version = OrderVersion::try_from(i64::from(self.candidate_version))
            .map_err(|_| integration(ids, "the candidate is not an order version"))?;
        // A NULL key is an exception only for a receipt without paid recurring bindings.
        // Recovery trusts the verified receipt, never a nullable field by itself.
        if !line.matches_bindings(&receipt.bindings) {
            return Err(integration(
                ids,
                "the frozen overlap evidence does not cover the receipt",
            ));
        }
        let terms = terms_digest(&line.query, &receipt.bindings);
        let (acceptance_id, hold_until) = (receipt.acceptance_id, receipt.hold_until);
        let bindings = receipt.bindings.clone();
        let pin = match OrderPin::new(
            AssessmentRef {
                assessment_id: order.assessment_id,
                assessed_at: order.assessed_at,
                resolve_date: order.resolve_date,
            },
            AcceptedVersionRef {
                order_id: line.query.order_id,
                order_version: version,
                line_id: line.line_id,
            },
            receipt,
        ) {
            Ok(pin) => pin,
            Err(CodecError::TooLarge) => {
                tracing::warn!(
                    target: "orders.gate.submit",
                    order_id = %line.query.order_id,
                    attempt_id = %self.attempt_id,
                    line_id = %line.line_id,
                    "bss-orders: a single pin exceeds the version limit"
                );
                return Ok(Admission::OverCapacity);
            }
            Err(error) => {
                return Err(integration_with(
                    ids,
                    "the receipt is not a valid schema-2 pin (D-192)",
                    &error,
                ));
            }
        };
        pin.verify_selection(
            &line.query,
            SelectionEvidence {
                acceptance_id,
                request_digest: line.request_digest,
                terms_digest: terms,
                evaluated_bindings: &bindings,
            },
        )
        .map_err(|_| integration(ids, "the receipt does not match the frozen request (D-192)"))?;
        // D-197: the receipt equals what Rating evaluated (selection, quantity, market, terms,
        // bindings); a legacy attempt without the frozen request is not compared.
        if let Some(evaluated) = &order.evaluation_request {
            verify_evaluated_query(&line.query, evaluated)
                .and_then(|()| verify_evaluated_bindings(&line.query, &bindings, evaluated))
                .map_err(|what| {
                    integration_with(
                        ids,
                        "the receipt does not match the evaluated request (D-197)",
                        &what,
                    )
                })?;
        }
        let encoded = pin
            .encode()
            .map_err(|_| integration(ids, "the pin does not encode"))?;
        let order_pin: Value = serde_json::from_slice(&encoded)
            .map_err(|_| integration(ids, "the encoded pin is not JSON"))?;
        let mut row = admitted_line(draft, resource_tenant_id, self.candidate_version, basis)
            .map_err(|e| {
                integration_with(ids, "the frozen basis does not admit the draft line", &e)
            })?;
        let items: Vec<bss_orders_sdk::authoring::SelectedItem> =
            serde_json::from_value(row.selected_items.clone())
                .map_err(|_| integration(ids, "the admitted selections do not decode"))?;
        let selected = super::preview::gate_line(
            row.line_id,
            row.plan_id,
            row.plan_revision_id,
            &items,
            &row.currency,
        );
        if !crate::domain::gate::commercial::verify_commit_line(
            &line.query,
            row.order_id,
            u64::try_from(row.version).map_err(|_| integration(ids, "invalid admitted version"))?,
            &selected,
        ) {
            return Err(integration(
                ids,
                "the admitted row differs from the frozen query",
            ));
        }
        row.commercial_attempt_id = Some(self.attempt_id);
        row.pricing_acceptance_id = Some(acceptance_id);
        row.pricing_request_digest = Some(digest_hex(&line.request_digest));
        row.pricing_terms_digest = Some(digest_hex(&terms));
        row.order_pin = Some(order_pin);
        row.overlap_item_keys = line
            .overlap_item_keys
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|_| integration(ids, "item key encoding"))?;
        row.overlap_scope_key.clone_from(&line.overlap_scope_key);
        row.catalog_subscription_product_key
            .clone_from(&line.catalog_subscription_product_key);
        let bytes = line_row_bytes(&row);
        Ok(Admission::Admitted(Box::new(AdmittedLine {
            row,
            bytes,
            hold_until,
        })))
    }

    /// Row 4's guards over the commit's facts (module doc, step 6).
    fn guards(
        &self,
        view: &PreparationView,
        blocked: Option<Blocked>,
        capacity: GuardVerdict,
        earliest_hold: Option<OffsetDateTime>,
        basis: DateBasis,
    ) -> GuardBindings {
        let admission = match blocked {
            Some(Blocked::Pricing(RefusalDisposition::BindingAdmission(reason))) => {
                GuardVerdict::Fail(reason)
            }
            Some(Blocked::Capacity) => GuardVerdict::Fail(Reason::PurchaseCapacityExceeded),
            Some(
                Blocked::Pricing(
                    RefusalDisposition::Composite(_) | RefusalDisposition::Unevaluable(_),
                )
                | Blocked::Totals,
            )
            | None => capacity,
        };
        let composite = match blocked {
            Some(Blocked::Pricing(RefusalDisposition::Composite(reason))) => {
                GuardVerdict::Fail(reason)
            }
            Some(
                Blocked::Pricing(
                    RefusalDisposition::BindingAdmission(_) | RefusalDisposition::Unevaluable(_),
                )
                | Blocked::Totals
                | Blocked::Capacity,
            )
            | None => GuardVerdict::Pass,
        };
        structural_guards(view, self.line_cap)
            // A recorded lapsed hold, the version limit, then each receipt's exclusive
            // activation deadline against the engine's transition time (03 step 15, D-152).
            .bind(BINDING_ADMISSION, move |subject: &GuardSubject<'_>| match (
                admission,
                earliest_hold,
            ) {
                (GuardVerdict::Fail(reason), _) => GuardVerdict::Fail(reason),
                (GuardVerdict::Pass, Some(hold)) if hold <= subject.transition_time => {
                    GuardVerdict::Fail(Reason::OrderBindingExpired)
                }
                (GuardVerdict::Pass, _) => GuardVerdict::Pass,
            })
            .bind(DATE_BASIS, Arc::new(basis).guard())
            .bind(GATE_COMPOSITE, move |_: &GuardSubject<'_>| composite)
    }
}

/// Every input row 4 declares, resolved: the receipts are the evidence (module doc).
pub(super) fn row_inputs_resolved() -> Inputs {
    GUARDS
        .iter()
        .filter(|g| g.rows.contains(&SUBMIT_ROW))
        .flat_map(|g| g.inputs.iter().copied())
        .fold(Inputs::new(), |inputs, port| {
            inputs.with(port, InputState::Resolved)
        })
}

/// The persisted bytes of one admitted `order_line` row, every column counted ([`persisted`]),
/// the encoded schema-2 pin included.
fn line_row_bytes(row: &order_line::Model) -> usize {
    persisted::UUID
        + persisted::INT
        + persisted::UUID
        + persisted::opt(row.commercial_attempt_id.as_ref(), persisted::UUID)
        + persisted::opt(row.pricing_acceptance_id.as_ref(), persisted::UUID)
        + persisted::opt_text(row.pricing_request_digest.as_deref())
        + persisted::opt_text(row.pricing_terms_digest.as_deref())
        + persisted::UUID
        + persisted::UUID
        + persisted::json(&row.selected_items)
        + persisted::text(&row.currency)
        + persisted::DATE
        + persisted::opt(row.service_activation_date.as_ref(), persisted::DATE)
        + persisted::opt(row.acceptance_due_date.as_ref(), persisted::DATE)
        + row
            .term_duration
            .as_ref()
            .map_or(0, |_| persisted::INTERVAL)
        + persisted::text(&row.term_kind)
        + persisted::json(&row.authored_term)
        + persisted::text(&row.billing_cycle)
        + row.order_pin.as_ref().map_or(0, persisted::json)
        + persisted::opt_text(row.overlap_scope_key.as_deref())
        + row.overlap_item_keys.as_ref().map_or(0, persisted::json)
        + persisted::opt_text(row.catalog_subscription_product_key.as_deref())
        + persisted::json(&row.date_policy_switch_state)
}

/// The admitted version's child documents: line rows, then totals rows (step 19).
struct SubmitDocuments {
    lines: Vec<order_line::Model>,
    totals: Vec<TotalRow>,
    assessment_id: Uuid,
    version: i32,
}

#[async_trait]
impl DocumentWriter for SubmitDocuments {
    async fn write(&self, documents: &ChildDocuments<'_, '_>) -> Result<(), ScopeError> {
        documents.insert_order_lines(self.lines.clone()).await?;
        let totals = self
            .totals
            .iter()
            .map(|row| total_model(documents.order_id(), self.version, self.assessment_id, row))
            .collect();
        documents.insert_resolved_totals(totals).await?;
        Ok(())
    }
}

/// One `resolved_total` row from its content (`domain::gate::totals`).
fn total_model(
    order_id: Uuid,
    version: i32,
    assessment_id: Uuid,
    row: &TotalRow,
) -> resolved_total::Model {
    let (discount_exact, discount_display) = row
        .discount
        .clone()
        .map_or((None, None), |(e, d)| (Some(e), Some(d)));
    let (tcv_exact, tcv_display, tcv_basis, tcv_evidence) = row
        .tcv
        .clone()
        .map_or((None, None, None, None), |(e, d, b, ev)| {
            (Some(e), Some(d), Some(b.token().to_owned()), Some(ev))
        });
    resolved_total::Model {
        order_id,
        version,
        scope: row.scope.token().to_owned(),
        line_id: row.line_id,
        currency: row.currency.clone(),
        assessment_id,
        currency_scale: i32::from(row.currency_scale),
        exclusions: row.exclusions.clone(),
        item_breakdown: row.item_breakdown.clone(),
        recurring_by_cycle: row.recurring_by_cycle.clone(),
        amount_status: row.amount_status.token().to_owned(),
        amount_basis: row.amount_basis.clone(),
        period_evidence: row.period_evidence.clone(),
        gross_exact: row.gross.as_ref().map(|g| g.0.clone()),
        gross_display: row.gross.as_ref().map(|g| g.1.clone()),
        net_exact: row.net.as_ref().map(|n| n.0.clone()),
        net_display: row.net.as_ref().map(|n| n.1.clone()),
        discount_status: if row.discount.is_some() {
            "available"
        } else {
            "unavailable"
        }
        .to_owned(),
        discount_exact,
        discount_display,
        promotion_ref: None,
        charge_kind: row.charge_kind.token().to_owned(),
        tcv_exact,
        tcv_display,
        tcv_basis,
        tcv_evidence,
    }
}
