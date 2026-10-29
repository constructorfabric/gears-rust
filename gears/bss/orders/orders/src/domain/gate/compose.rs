//! D-195 composition: one Orders outcome per `(predicate, line, item, selection)` from ordered
//! producer subresults. Required conjunction: any unevaluable gives unevaluable; else any failed
//! gives failed; else passed. The reason is the first subresult's of the winning verdict, in
//! registry (given) order. Results of different applicability are never merged: the caller
//! composes each applicability separately.
use super::evidence::UpstreamDetail;
use super::vector::{Applicability, Outcome, OutcomeKey, ProducerResult, Verdict};
use bss_orders_sdk::catalog::Reason;
use std::collections::BTreeSet;
use toolkit_macros::domain_model;

/// Why subresults could not be composed into one outcome.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ComposeError {
    /// Two subresults share one `producer/profile/predicate` identity.
    #[error("duplicate producer subresult `{0}`")]
    DuplicateSubresult(String),
    /// An owner reason the mapping registry does not know: the row is invalid coverage
    /// (D-195 "unknown results ... map to catalog-predicates-unavailable" at the caller).
    #[error("unmapped producer reason `{0}`")]
    ReasonUnmapped(String),
    /// No subresult at all: an uncovered row is invalid coverage (D-195), never a pass.
    #[error("no producer subresults to compose")]
    NoSubresults,
}

/// Compose ordered producer subresults into one outcome (D-195).
///
/// # Errors
/// No subresults, duplicate subresult identity, or a winning non-passed subresult whose reason `reason_of`
/// cannot map.
#[must_use = "the composed outcome or the error must be handled"]
pub fn compose(
    key: OutcomeKey,
    applicability: Applicability,
    reason_of: impl Fn(&ProducerResult) -> Option<Reason>,
    subresults: Vec<ProducerResult>,
) -> Result<Outcome, ComposeError> {
    if subresults.is_empty() {
        return Err(ComposeError::NoSubresults);
    }
    let mut seen = BTreeSet::new();
    for s in &subresults {
        let id = (
            s.producer.as_str(),
            s.profile.as_str(),
            s.predicate.as_str(),
        );
        if !seen.insert(id) {
            let (producer, profile, predicate) = id;
            return Err(ComposeError::DuplicateSubresult(format!(
                "{producer}/{profile}/{predicate}"
            )));
        }
    }
    let winner = |v: Verdict| subresults.iter().find(|s| s.verdict == v);
    let (verdict, chosen) = match (winner(Verdict::Unevaluable), winner(Verdict::Failed)) {
        (Some(u), _) => (Verdict::Unevaluable, Some(u)),
        (None, Some(f)) => (Verdict::Failed, Some(f)),
        (None, None) => (Verdict::Passed, None),
    };
    let reason =
        match chosen {
            Some(s) => Some(reason_of(s).ok_or_else(|| {
                ComposeError::ReasonUnmapped(s.reason.clone().unwrap_or_default())
            })?),
            None => None,
        };
    Ok(Outcome {
        key,
        verdict,
        reason,
        applicability,
        producer_results: subresults,
        upstream_detail: None,
    })
}

/// A wholly local result (empty `producer_results`, D-195 storage rule).
#[must_use]
pub fn local(
    key: OutcomeKey,
    applicability: Applicability,
    verdict: Verdict,
    reason: Option<Reason>,
    upstream_detail: Option<UpstreamDetail>,
) -> Outcome {
    Outcome {
        key,
        verdict,
        reason,
        applicability,
        producer_results: Vec::new(),
        upstream_detail,
    }
}
