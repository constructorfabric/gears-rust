//! Submit phase A's frozen commercial inputs (S3-12, D-188 step 2): the owned answers of one
//! passing gate run that the per-line Pricing requests are built from, held until the engine
//! allocates the attempt's candidate inside its reservation transaction. The pure builder is
//! [`crate::domain::gate::commercial`]; this adapter only owns its inputs, because the
//! reservation runs the builder after the run's borrowed answers are gone.
use super::orchestrate::Resolved;
use crate::domain::dates::DateBasis;
use crate::domain::gate::answers::{AcceptancePolicy, Market, ResolvedTerms};
use crate::domain::gate::assess::BasketLine;
use crate::domain::gate::commercial::{
    CommercialError, CommercialFacts, FrozenOrder, build_line_requests, line_requests_value,
};
use crate::domain::gate::orchestrate::TenantAxes;
use bss_pricing_sdk::read::ResolvedBindings;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use uuid::Uuid;

/// The owned inputs of one submit's line requests, captured from a passing run.
#[derive(Debug, Clone)]
pub struct FrozenLineInputs {
    order_id: Uuid,
    axes: TenantAxes,
    lines: Vec<BasketLine>,
    resolved_bindings: BTreeMap<Uuid, Arc<ResolvedBindings>>,
    terms: BTreeMap<Uuid, ResolvedTerms>,
    market: Option<Market>,
    basis: DateBasis,
    policy: AcceptancePolicy,
    keys: BTreeMap<Uuid, Vec<crate::domain::gate::answers::OverlapKey>>,
    order: FrozenOrder,
}

impl FrozenLineInputs {
    /// Capture what the requests read from `resolved`, for `lines` of `order_id`, and the
    /// order facts the attempt freezes once (`order`: the assessment and its evaluation).
    #[must_use]
    pub fn capture(
        order_id: Uuid,
        axes: TenantAxes,
        lines: &[BasketLine],
        resolved: &Resolved,
        basis: &DateBasis,
        policy: &AcceptancePolicy,
        order: FrozenOrder,
    ) -> Self {
        Self {
            order_id,
            axes,
            lines: lines.to_vec(),
            resolved_bindings: resolved.resolve_answers.clone(),
            terms: resolved.terms.clone(),
            market: resolved.market.clone(),
            basis: basis.clone(),
            policy: policy.clone(),
            keys: resolved.keys.clone(),
            order,
        }
    }

    fn facts(&self) -> CommercialFacts<'_> {
        CommercialFacts {
            order_id: self.order_id,
            axes: self.axes,
            lines: &self.lines,
            resolved_bindings: &self.resolved_bindings,
            terms: &self.terms,
            keys: &self.keys,
            market: self.market.as_ref(),
            basis: &self.basis,
            policy: &self.policy,
        }
    }

    /// The canonical `line_requests` value for `candidate` (`{line_id: {key, overlap_scope_key,
    /// query, request_digest}, "order": …}`).
    ///
    /// # Errors
    /// A line whose request cannot be frozen or encoded.
    pub fn line_requests(&self, candidate: i32) -> Result<Value, CommercialError> {
        line_requests_value(&build_line_requests(&self.facts(), candidate)?, &self.order)
    }
}
