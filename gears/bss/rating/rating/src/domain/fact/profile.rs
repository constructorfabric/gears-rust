//! Rating's reading of Subscriptions' `subscriptions-rating/1` profile (Subscriptions PR #5516
//! `DESIGN.md` §3.3.3; DESIGN 11 §4.3; DECISIONS T-D-87) and the checks of feature 06 §3.1.
//!
//! #5516 fixes the profile's semantic groups and leaves the field names and codec open (S-U13), so
//! the names below are Rating's, and `FakeSubscriptionsBilling` writes them. A binding is the
//! core's serde mirror of Pricing's `AcceptedBinding` (`CoreBinding`), so what Rating stores is
//! what it evaluates.
//!
//! [`read_fact`] reads the key first, so even a payload it refuses can be quarantined under its
//! key; a payload whose key cannot be read is not stored at all.

use std::collections::BTreeSet;

use bss_rating_core::Interval;
use bss_rating_core::engine::current::input::{BillingTiming, ChargeKind, CoreBinding, TenantAxes};
use serde::Deserialize;
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::fact::model::{
    AcceptedTerm, Coverage, FactKind, FactRef, FactVersion, Lot, Obligation, Slice, Suspensions,
    Timing,
};
use crate::domain::inbox::PayloadRefusal;
use crate::domain::pricing::cell_digest;

/// The profile this reader reads.
pub const PROFILE: &str = "subscriptions-rating/1";

/// The most integer digits of a lot quantity (#5516 `DESIGN.md` §3.3.3).
pub const MAX_INTEGER_DIGITS: usize = 60;

/// The most fractional digits of a lot quantity.
pub const MAX_FRACTION_DIGITS: usize = 28;

/// What reading a fact payload found.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum ReadFact {
    /// The fact version, every check passed.
    Accepted(Box<FactVersion>),
    /// The payload names another profile: quarantined `unsupported_schema_version`.
    Unsupported {
        /// Its key.
        fact: FactRef,
    },
    /// The payload breaks the profile: quarantined with the refusal as its reason.
    Refused {
        /// Its key.
        fact: FactRef,
        /// Why.
        refusal: PayloadRefusal,
        /// What failed.
        detail: String,
    },
}

/// A payload whose key — seller tenant, fact id, version — cannot be read: nothing can be stored.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the fact's key cannot be read: {0}")]
pub struct Unkeyable(pub String);

/// A check that failed: its refusal and what failed.
#[derive(Debug)]
struct Refusal {
    refusal: PayloadRefusal,
    detail: String,
}

impl Refusal {
    fn invalid(detail: impl Into<String>) -> Self {
        Self {
            refusal: PayloadRefusal::Invalid,
            detail: detail.into(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireFact {
    profile: String,
    fact_id: Uuid,
    fact_version: i64,
    #[serde(default)]
    predecessor_version: Option<i64>,
    payload_digest: String,
    #[serde(default)]
    source_commercial_revision: Option<String>,
    billing_group: WireGroup,
    resource_tenant_id: Uuid,
    sub_line_key: String,
    item_id: Uuid,
    #[serde(default)]
    order_refs: Vec<Value>,
    kind: String,
    obligation: String,
    timing: String,
    period: WireInterval,
    served: WireInterval,
    billing_terms: Value,
    #[serde(default)]
    suspensions: Option<WireSuspensions>,
    #[serde(default)]
    occurrence: Option<WireOccurrence>,
    #[serde(default)]
    slices: Vec<WireSlice>,
    #[serde(default)]
    terms: Vec<WireTerm>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireGroup {
    billing_group_id: Uuid,
    subscription_id: Uuid,
    seller_tenant_id: Uuid,
    payer_tenant_id: Uuid,
    currency: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireInterval {
    start: String,
    end: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSuspensions {
    ranges: Vec<WireInterval>,
    posture: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireOccurrence {
    occurrence_id: String,
    basis: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSlice {
    from: String,
    to: String,
    coverage: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    terms: Vec<String>,
    #[serde(default)]
    lots: Vec<WireLot>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireLot {
    lot_id: String,
    quantity: String,
    unit: String,
    #[serde(default)]
    terms_ref: Option<String>,
    purchase_origin: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireTerm {
    terms_ref: String,
    catalog_tenant_id: Uuid,
    plan_id: Uuid,
    revision_id: Uuid,
    binding: CoreBinding,
}

/// Reads a fact payload: its key, then the profile and every check of feature 06 §3.1.
///
/// # Errors
///
/// [`Unkeyable`] when the seller tenant, fact id or version cannot be read.
pub fn read_fact(payload: &Value) -> Result<ReadFact, Unkeyable> {
    let fact = read_key(payload)?;
    if payload.get("profile").and_then(Value::as_str) != Some(PROFILE) {
        return Ok(ReadFact::Unsupported { fact });
    }
    let wire: WireFact = match serde_json::from_value(payload.clone()) {
        Ok(wire) => wire,
        Err(error) => {
            return Ok(ReadFact::Refused {
                fact,
                refusal: PayloadRefusal::Invalid,
                detail: format!("the payload does not read as {PROFILE}: {error}"),
            });
        }
    };
    // @cpt-begin:cpt-cf-bss-rating-algo-schedule-validate-fact:p1:inst-vf-invalid
    Ok(match check(fact, wire) {
        Ok(version) => ReadFact::Accepted(Box::new(version)),
        Err(Refusal { refusal, detail }) => ReadFact::Refused {
            fact,
            refusal,
            detail,
        },
    })
    // @cpt-end:cpt-cf-bss-rating-algo-schedule-validate-fact:p1:inst-vf-invalid
}

/// The key of a payload, read without the rest of it.
///
/// # Errors
///
/// [`Unkeyable`] naming the field that cannot be read.
pub fn read_key(payload: &Value) -> Result<FactRef, Unkeyable> {
    let uuid = |path: &[&str]| -> Result<Uuid, Unkeyable> {
        let mut value = payload;
        for part in path {
            value = value
                .get(part)
                .ok_or_else(|| Unkeyable(format!("`{}` is missing", path.join("."))))?;
        }
        value
            .as_str()
            .and_then(|text| Uuid::parse_str(text).ok())
            .ok_or_else(|| Unkeyable(format!("`{}` is not a UUID", path.join("."))))
    };
    let tenant_id = uuid(&["billing_group", "seller_tenant_id"])?;
    let fact_id = uuid(&["fact_id"])?;
    let fact_version = payload
        .get("fact_version")
        .and_then(Value::as_i64)
        .filter(|version| *version >= 1)
        .ok_or_else(|| Unkeyable("`fact_version` is not a positive integer".to_owned()))?;
    Ok(FactRef {
        tenant_id,
        fact_id,
        fact_version,
    })
}

fn check(fact: FactRef, wire: WireFact) -> Result<FactVersion, Refusal> {
    // @cpt-begin:cpt-cf-bss-rating-algo-schedule-validate-fact:p1:inst-vf-whole-seconds
    let period = interval("period", &wire.period)?;
    let served = interval("served", &wire.served)?;
    let mut spans = Vec::with_capacity(wire.slices.len());
    for (index, slice) in wire.slices.iter().enumerate() {
        let field = format!("slices[{index}]");
        spans.push(Interval {
            start: instant(&format!("{field}.from"), &slice.from)?,
            end: instant(&format!("{field}.to"), &slice.to)?,
        });
    }
    let suspensions = suspensions(wire.suspensions.as_ref())?;
    check_billing_terms(&wire.billing_terms)?;
    // @cpt-end:cpt-cf-bss-rating-algo-schedule-validate-fact:p1:inst-vf-whole-seconds
    let kind = FactKind::parse(&wire.kind)
        .ok_or_else(|| Refusal::invalid(format!("unknown kind `{}`", wire.kind)))?;
    let obligation = Obligation::parse(&wire.obligation)
        .ok_or_else(|| Refusal::invalid(format!("unknown obligation `{}`", wire.obligation)))?;
    let timing = Timing::parse(&wire.timing)
        .ok_or_else(|| Refusal::invalid(format!("unknown timing `{}`", wire.timing)))?;
    check_identity(fact, &wire)?;
    // @cpt-begin:cpt-cf-bss-rating-algo-schedule-validate-fact:p1:inst-vf-timeline
    check_timeline(period, served, &spans, obligation)?;
    // @cpt-end:cpt-cf-bss-rating-algo-schedule-validate-fact:p1:inst-vf-timeline
    // @cpt-begin:cpt-cf-bss-rating-algo-schedule-validate-fact:p1:inst-vf-terms
    let terms = terms(fact, &wire, kind, timing)?;
    // @cpt-end:cpt-cf-bss-rating-algo-schedule-validate-fact:p1:inst-vf-terms
    // @cpt-begin:cpt-cf-bss-rating-algo-schedule-validate-fact:p1:inst-vf-slices
    let slices = slices(&wire.slices, spans, &terms)?;
    // @cpt-end:cpt-cf-bss-rating-algo-schedule-validate-fact:p1:inst-vf-slices
    let group = wire.billing_group;
    Ok(FactVersion {
        fact,
        predecessor_version: wire.predecessor_version,
        producer_digest: wire.payload_digest,
        kind,
        obligation,
        timing,
        billing_group_id: group.billing_group_id,
        subscription_id: group.subscription_id,
        axes: TenantAxes {
            seller_tenant_id: group.seller_tenant_id,
            payer_tenant_id: group.payer_tenant_id,
            resource_tenant_id: wire.resource_tenant_id,
        },
        currency: group.currency,
        sub_line_key: wire.sub_line_key,
        item_id: wire.item_id,
        period,
        served,
        billing_terms: wire.billing_terms,
        suspensions,
        occurrence_id: wire.occurrence.map(|occurrence| occurrence.occurrence_id),
        slices,
        terms,
    })
}

fn instant(field: &str, text: &str) -> Result<OffsetDateTime, Refusal> {
    let at = OffsetDateTime::parse(text, &Rfc3339)
        .map_err(|error| Refusal::invalid(format!("{field}: `{text}` is not RFC 3339: {error}")))?;
    if at.nanosecond() != 0 {
        return Err(Refusal {
            refusal: PayloadRefusal::InstantNotWholeSecond,
            detail: format!("{field}: `{text}` has a sub-second part"),
        });
    }
    Ok(at.to_offset(UtcOffset::UTC))
}

fn interval(field: &str, wire: &WireInterval) -> Result<Interval, Refusal> {
    Ok(Interval {
        start: instant(&format!("{field}.start"), &wire.start)?,
        end: instant(&format!("{field}.end"), &wire.end)?,
    })
}

fn suspensions(wire: Option<&WireSuspensions>) -> Result<Suspensions, Refusal> {
    let Some(wire) = wire else {
        return Ok(Suspensions {
            ranges: Vec::new(),
            posture: "continue".to_owned(),
        });
    };
    if !matches!(wire.posture.as_str(), "pause_recurring" | "continue") {
        return Err(Refusal::invalid(format!(
            "suspensions.posture: unknown `{}`",
            wire.posture
        )));
    }
    let mut ranges = Vec::with_capacity(wire.ranges.len());
    for (index, range) in wire.ranges.iter().enumerate() {
        let range = interval(&format!("suspensions.ranges[{index}]"), range)?;
        if range.start >= range.end
            || ranges
                .last()
                .is_some_and(|last: &Interval| last.end > range.start)
        {
            return Err(Refusal::invalid(format!(
                "suspensions.ranges[{index}] is empty or out of order"
            )));
        }
        ranges.push(range);
    }
    Ok(Suspensions {
        ranges,
        posture: wire.posture.clone(),
    })
}

fn check_billing_terms(terms: &Value) -> Result<(), Refusal> {
    let anchor_at = terms
        .get("anchor_at")
        .and_then(Value::as_str)
        .ok_or_else(|| Refusal::invalid("billing_terms.anchor_at is missing"))?;
    instant("billing_terms.anchor_at", anchor_at).map(drop)
}

fn check_identity(fact: FactRef, wire: &WireFact) -> Result<(), Refusal> {
    if wire.profile != PROFILE
        || wire.fact_id != fact.fact_id
        || wire.fact_version != fact.fact_version
    {
        return Err(Refusal::invalid(
            "the payload's identity does not read twice the same",
        ));
    }
    if let Some(occurrence) = &wire.occurrence {
        check_occurrence(occurrence)?;
    }
    if wire.payload_digest.len() != 64
        || !wire
            .payload_digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Refusal::invalid(
            "payload_digest is not 64 lowercase hex characters",
        ));
    }
    if wire
        .predecessor_version
        .is_some_and(|predecessor| predecessor < 1 || predecessor >= fact.fact_version)
    {
        return Err(Refusal::invalid(
            "predecessor_version must name an earlier version",
        ));
    }
    if wire.sub_line_key.trim().is_empty() || wire.billing_group.currency.trim().is_empty() {
        return Err(Refusal::invalid("sub_line_key and currency are required"));
    }
    if wire
        .source_commercial_revision
        .as_deref()
        .is_some_and(|revision| revision.trim().is_empty())
        || wire.order_refs.iter().any(Value::is_null)
    {
        return Err(Refusal::invalid(
            "source_commercial_revision and order_refs must not be blank",
        ));
    }
    Ok(())
}

/// A one-time occurrence: its id and basis, `once` or `units{quantity, unit}` (SUB-D-43).
fn check_occurrence(occurrence: &WireOccurrence) -> Result<(), Refusal> {
    let kind = occurrence.basis.get("kind").and_then(Value::as_str);
    let units_ok = || {
        occurrence
            .basis
            .get("quantity")
            .and_then(Value::as_str)
            .is_some_and(is_positive_decimal)
            && occurrence
                .basis
                .get("unit")
                .and_then(Value::as_str)
                .is_some_and(|unit| !unit.trim().is_empty())
    };
    let basis_ok = match kind {
        Some("once") => true,
        Some("units") => units_ok(),
        _ => false,
    };
    if occurrence.occurrence_id.trim().is_empty() || !basis_ok {
        return Err(Refusal::invalid(
            "occurrence: an id and a basis `once` or `units{quantity, unit}`",
        ));
    }
    Ok(())
}

fn check_timeline(
    period: Interval,
    served: Interval,
    spans: &[Interval],
    obligation: Obligation,
) -> Result<(), Refusal> {
    if period.start >= period.end {
        return Err(Refusal::invalid("the period is empty"));
    }
    if served.start > served.end || served.start < period.start || served.end > period.end {
        return Err(Refusal::invalid("the served extent leaves the period"));
    }
    match obligation {
        Obligation::Zeroed if spans.is_empty() => return Ok(()),
        Obligation::Zeroed => {
            return Err(Refusal::invalid("a zeroed obligation carries no slice"));
        }
        Obligation::Active if served.start == served.end || spans.is_empty() => {
            return Err(Refusal::invalid(
                "an active obligation needs a served extent and slices",
            ));
        }
        Obligation::Active => {}
    }
    let mut at = served.start;
    for (index, span) in spans.iter().enumerate() {
        if span.start != at || span.start >= span.end {
            return Err(Refusal::invalid(format!(
                "slices[{index}] leaves a gap, overlaps or is empty"
            )));
        }
        at = span.end;
    }
    if at != served.end {
        return Err(Refusal::invalid(
            "the slices do not reach the end of the served extent",
        ));
    }
    Ok(())
}

fn terms(
    fact: FactRef,
    wire: &WireFact,
    kind: FactKind,
    timing: Timing,
) -> Result<Vec<AcceptedTerm>, Refusal> {
    let mut seen = BTreeSet::new();
    let mut terms = Vec::with_capacity(wire.terms.len());
    for term in &wire.terms {
        let field = format!("terms[{}]", term.terms_ref);
        if term.terms_ref.trim().is_empty() || !seen.insert(term.terms_ref.clone()) {
            return Err(Refusal::invalid(format!(
                "{field}: blank or repeated terms_ref"
            )));
        }
        if term.catalog_tenant_id != fact.tenant_id {
            // Pricing reads only the caller's own catalog (D-523): a term of another seller's
            // catalog can never be proven fresh.
            return Err(Refusal::invalid(format!(
                "{field}: catalog of another tenant"
            )));
        }
        let binding = &term.binding;
        if binding.item_id != wire.item_id {
            return Err(Refusal::invalid(format!(
                "{field}: binding of another item"
            )));
        }
        let expected_kind = match kind {
            FactKind::Recurring => ChargeKind::Recurring,
            FactKind::Usage => ChargeKind::Usage,
            FactKind::OneTime => ChargeKind::OneTime,
        };
        if binding.kind != expected_kind {
            return Err(Refusal::invalid(format!(
                "{field}: binding of another charge kind"
            )));
        }
        let expected_timing = match timing {
            Timing::Advance => BillingTiming::Advance,
            Timing::Arrears => BillingTiming::Arrears,
        };
        if binding.invoice.timing != expected_timing {
            return Err(Refusal::invalid(format!(
                "{field}: the binding's invoice timing differs from the fact's"
            )));
        }
        let binding_digest = cell_digest(term.plan_id, term.revision_id, binding)
            .map_err(|detail| Refusal::invalid(format!("{field}: {detail}")))?;
        terms.push(AcceptedTerm {
            terms_ref: term.terms_ref.clone(),
            catalog_tenant_id: term.catalog_tenant_id,
            plan_id: term.plan_id,
            revision_id: term.revision_id,
            binding: binding.clone(),
            binding_digest,
        });
    }
    Ok(terms)
}

fn slices(
    wire: &[WireSlice],
    spans: Vec<Interval>,
    terms: &[AcceptedTerm],
) -> Result<Vec<Slice>, Refusal> {
    let mut out = Vec::with_capacity(wire.len());
    for (index, (slice, span)) in wire.iter().zip(spans).enumerate() {
        let field = format!("slices[{index}]");
        let reason = slice
            .reason
            .clone()
            .filter(|reason| !reason.trim().is_empty());
        let coverage = match (slice.coverage.as_str(), reason) {
            ("chargeable", None) => Coverage::Chargeable,
            ("nonchargeable", Some(reason)) => Coverage::Nonchargeable { reason },
            ("unresolved", Some(reason)) => Coverage::Unresolved { reason },
            (coverage, _) => {
                return Err(Refusal::invalid(format!(
                    "{field}: coverage `{coverage}` with a missing or extra reason"
                )));
            }
        };
        check_slice_terms(&field, &coverage, &slice.terms, terms)?;
        let lots = lots(&field, &coverage, slice)?;
        out.push(Slice {
            span,
            coverage,
            terms: slice.terms.clone(),
            lots,
        });
    }
    Ok(out)
}

fn check_slice_terms(
    field: &str,
    coverage: &Coverage,
    named: &[String],
    terms: &[AcceptedTerm],
) -> Result<(), Refusal> {
    let chargeable = *coverage == Coverage::Chargeable;
    if chargeable == named.is_empty() {
        return Err(Refusal::invalid(format!(
            "{field}: a chargeable slice names its terms, and only a chargeable slice does"
        )));
    }
    let mut cells = BTreeSet::new();
    for terms_ref in named {
        let term = terms
            .iter()
            .find(|term| &term.terms_ref == terms_ref)
            .ok_or_else(|| Refusal::invalid(format!("{field}: names absent term `{terms_ref}`")))?;
        if !cells.insert(term.binding.dimension_value.clone()) {
            return Err(Refusal::invalid(format!(
                "{field}: two terms for one dimension value"
            )));
        }
    }
    Ok(())
}

fn lots(field: &str, coverage: &Coverage, slice: &WireSlice) -> Result<Vec<Lot>, Refusal> {
    let mut ids = BTreeSet::new();
    let mut lots = Vec::with_capacity(slice.lots.len());
    for lot in &slice.lots {
        let lot_field = format!("{field}.lots[{}]", lot.lot_id);
        if lot.lot_id.trim().is_empty() || !ids.insert(lot.lot_id.clone()) {
            return Err(Refusal::invalid(format!(
                "{lot_field}: blank or repeated lot_id"
            )));
        }
        if !is_positive_decimal(&lot.quantity) {
            return Err(Refusal::invalid(format!(
                "{lot_field}: quantity `{}` is not a positive canonical decimal",
                lot.quantity
            )));
        }
        let terms_ok = match (coverage, &lot.terms_ref) {
            (Coverage::Chargeable, Some(terms_ref)) => slice.terms.contains(terms_ref),
            (Coverage::Unresolved { .. }, None) => true,
            _ => false,
        };
        if !terms_ok || lot.unit.trim().is_empty() || lot.purchase_origin.trim().is_empty() {
            return Err(Refusal::invalid(format!(
                "{lot_field}: a lot names a term of its chargeable slice, a unit and its purchase"
            )));
        }
        lots.push(Lot {
            lot_id: lot.lot_id.clone(),
            quantity: lot.quantity.clone(),
            unit: lot.unit.clone(),
            terms_ref: lot.terms_ref.clone(),
            purchase_origin: lot.purchase_origin.clone(),
        });
    }
    Ok(lots)
}

/// Canonical decimal text of a positive quantity: no sign, no leading zero, at most
/// [`MAX_INTEGER_DIGITS`] integer and [`MAX_FRACTION_DIGITS`] fractional digits.
#[must_use]
pub fn is_positive_decimal(text: &str) -> bool {
    let (integer, fraction) = match text.split_once('.') {
        Some((integer, fraction)) => (integer, Some(fraction)),
        None => (text, None),
    };
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    if !digits(integer)
        || integer.len() > MAX_INTEGER_DIGITS
        || (integer.len() > 1 && integer.starts_with('0'))
    {
        return false;
    }
    if let Some(fraction) = fraction
        && (!digits(fraction) || fraction.len() > MAX_FRACTION_DIGITS)
    {
        return false;
    }
    text.bytes().any(|b| (b'1'..=b'9').contains(&b))
}

#[cfg(test)]
#[path = "profile_tests.rs"]
mod tests;
