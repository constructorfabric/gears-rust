//! Rule (i): the rows the D-189 assessment port decides (predicates 2, 7, 8 and 4-Orders),
//! composed from the answered envelope under the D-195 coverage rules
//! (DESIGN `#contract-03-diagnostic-mapping`).
use super::{ASSESSED, AssessmentInput, unevaluable};
use crate::domain::gate::compose::{ComposeError, compose, local};
use crate::domain::gate::evidence::UpstreamDetail;
use crate::domain::gate::predicates::PredicateId as P;
use crate::domain::gate::vector::{Applicability, Outcome, OutcomeKey, ProducerResult, Verdict};
use bss_orders_sdk::catalog::Reason;
use std::collections::{BTreeMap, BTreeSet};
use toolkit_macros::domain_model;

/// The `upstream_code` of a would-be passed row withheld because the envelope is malformed.
const ENVELOPE_MALFORMED: &str = "assessment-envelope-malformed";

/// The answered assessment, grouped by expected row identity.
#[domain_model]
pub(super) struct Assessed {
    /// Each expected row's subresults in `(producer, profile, predicate)` order.
    by_key: BTreeMap<OutcomeKey, Vec<ProducerResult>>,
    /// The envelope carried an entry that is not independently verifiable: an unknown or
    /// non-assessed predicate token, a line outside the basket, an identity outside the expected
    /// coverage (out of slot), or a duplicate subresult identity (D-195).
    malformed: bool,
}

/// The answered assessment's subresults by expected row identity. Only entries keyed inside
/// `expected` are kept; any other entry, and any duplicate subresult identity, marks the
/// envelope malformed (D-195: keep the verifiable entries, never treat them as a complete owner
/// vector). `None` when the port did not answer.
pub(super) fn assessed_results(
    input: &AssessmentInput,
    expected: &BTreeSet<OutcomeKey>,
) -> Option<Assessed> {
    let answer = input.assessment.answered()?;
    let mut by_key: BTreeMap<OutcomeKey, Vec<ProducerResult>> = BTreeMap::new();
    let mut malformed = answer.profile != crate::domain::gate::orchestrate::ASSESSMENT_PROFILE;
    for r in &answer.results {
        let Some(predicate) = P::from_token(&r.result.predicate).filter(|p| ASSESSED.contains(p))
        else {
            malformed = true;
            continue;
        };
        let k = OutcomeKey {
            predicate,
            line_id: Some(r.line_id),
            item_id: r.item_id,
            selection: r.selection.clone(),
        };
        // Out of basket or out of slot: never part of any row.
        if !expected.contains(&k) {
            malformed = true;
            continue;
        }
        by_key.entry(k).or_default().push(r.result.clone());
    }
    let identity =
        |s: &ProducerResult| (s.producer.clone(), s.profile.clone(), s.predicate.clone());
    for subs in by_key.values_mut() {
        subs.sort_by_key(identity);
        if subs.windows(2).any(|w| identity(&w[0]) == identity(&w[1])) {
            malformed = true;
        }
    }
    Some(Assessed { by_key, malformed })
}

/// The Orders reason one producer subresult of a `predicate` row maps to (D-195 registry; F03
/// §3.1): a failed subresult only to one of the predicate's own `failure_reasons`, an
/// unevaluable one only to `catalog-predicate-unevaluable` or `catalog-predicates-unavailable`.
/// Any other token, registered elsewhere in Orders or not at all, is unmapped, so the row is
/// invalid coverage: unmapped upstream detail never becomes a public reason.
fn mapped_reason(predicate: P, s: &ProducerResult) -> Option<Reason> {
    let reason = s.reason.as_deref().and_then(Reason::from_token)?;
    let owned = match s.verdict {
        Verdict::Failed => predicate.spec().failure_reasons.contains(&reason),
        Verdict::Unevaluable => matches!(
            reason,
            Reason::CatalogPredicateUnevaluable | Reason::CatalogPredicatesUnavailable
        ),
        Verdict::Passed => false,
    };
    owned.then_some(reason)
}

/// Rule (i): one assessed row. Unanswered port: the port reason, except `MarketConsistency`,
/// whose owner row is missing (F03 §4.2 predicate 4). Uncovered row or a composition error
/// (duplicate subresult, unmapped reason): invalid coverage (D-195). A malformed envelope never
/// admits a row: a would-be passed row becomes invalid coverage with `upstream_code`
/// `assessment-envelope-malformed`, keeping its producer subresults as evidence (D-195).
pub(super) fn assessed_row(k: OutcomeKey, assessed: Option<&Assessed>) -> Outcome {
    let invalid = |k: OutcomeKey| {
        let (v, r, d) = unevaluable(Reason::CatalogPredicatesUnavailable);
        local(k, Applicability::Required, v, r, d)
    };
    let Some(assessed) = assessed else {
        if k.predicate == P::MarketConsistency {
            let (v, r, d) = unevaluable(Reason::CatalogPredicateUnevaluable);
            return local(k, Applicability::Required, v, r, d);
        }
        return invalid(k);
    };
    let Some(subs) = assessed.by_key.get(&k).filter(|s| !s.is_empty()) else {
        return invalid(k);
    };
    let reason_of = |s: &ProducerResult| mapped_reason(k.predicate, s);
    let mut o = match compose(k.clone(), Applicability::Required, reason_of, subs.clone()) {
        Ok(o) => o,
        // Every composition refusal is invalid coverage (D-195); listed so a new variant must
        // be decided here.
        Err(
            ComposeError::NoSubresults
            | ComposeError::DuplicateSubresult(_)
            | ComposeError::ReasonUnmapped(_),
        ) => return invalid(k),
    };
    if assessed.malformed && o.verdict == Verdict::Passed {
        o.verdict = Verdict::Unevaluable;
        o.reason = Some(Reason::CatalogPredicatesUnavailable);
        o.upstream_detail = Some(UpstreamDetail {
            upstream_code: Some(ENVELOPE_MALFORMED.to_owned()),
            ..UpstreamDetail::default()
        });
    }
    o
}
