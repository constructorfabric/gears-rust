//! Submit's frozen per-line Pricing requests (S3-12; D-188 step 2, D-192, D-193, D-197): one
//! `NewSaleQuery` per basket line, built from the basket, the run's answers and the frozen date
//! basis, and the canonical `line_requests` value a D-188 attempt stores before the first Pricing
//! command. Pure: no call, no clock, no storage; the candidate is the attempt's reserved version.
//!
//! Sourcing, member by member (nothing is defaulted; an input the run does not hold refuses the
//! line with [`CommercialError::Incomplete`]):
//! - `tenant_axes`, `order_id`: the order; `order_version`: the reserved candidate;
//! - `line_id`, `plan_id`, `plan_revision_id`, `selections`: the basket line and its selected
//!   items, in request order (`dimension_value` is the selected value, `None` the default slot);
//! - `quantity`: the line's one exact quantity (D-192). A bundle line states it on every selected
//!   item; items whose quantities differ are [`CommercialError::Unsupported`], never reconciled;
//! - `market`: the confirmed payer's currency and region (D-215);
//! - `start_at`: the line's resolved service-activation date at 00:00 UTC, from the frozen basis;
//! - `term`: the resolved term of the line's billing terms;
//! - `billing_terms`: the resolved snapshot the billing-terms port stated, its digest recomputed
//!   with `billing_terms_digest` and required to equal the stated one;
//! - `resolved_bindings_digest`: `selected_bindings_digest` over the line's resolved bindings
//!   (Pricing's own `resolve` answer) and the selections;
//! - `hold_policy_version`: the seller's acceptance policy version, a canonical positive integer.
//!
//! Beside each request the attempt freezes the line's overlap scope key (the run's one key for
//! a recurring line; null for non-recurring lines; the commit claims present keys, D-179), and once per attempt the [`FrozenOrder`]: the
//! assessment identity the pins name and the Rating evaluation the totals are written from
//! (D-188: "freezes the request and returned evaluation before acceptance dispatch"), plus the
//! run's gate report and outcome vector (D-231), so a recovered attempt commits from exactly
//! what it reserved, with the same settled report and diagnostics, never from a fresh run.
//!
//! The command key of a line is `{order_id}/{candidate}/{line_id}/submit` ([`command_key`]): the
//! attempt identity is allocated with the candidate in one transaction, so the key is built from
//! the candidate, which is unique per order and immutable for the attempt (the stored
//! `(order_id, candidate_version)` is unique). It is therefore stable across retries and recovery
//! of one attempt, and distinct for every other attempt.
use super::answers::{
    AcceptancePolicy, BillingTermsSnapshot, EvaluationCycle, EvaluationTerm, Market,
    OrderEvaluation, OrderEvaluationRequest, ResolvedTerms, TermsAnchor, TermsSnapshotSource,
};
use super::assess::BasketLine;
use super::orchestrate::TenantAxes;
use super::vector::Vector;
use crate::domain::contributions::VersionContribution;
use crate::domain::dates::{DateBasis, LineResolution};
use crate::domain::transition::GuardVerdict;
use bss_orders_sdk::catalog::Reason;
use bss_orders_sdk::commercial::{
    CodecError, decode_acceptance_receipt, decode_new_sale_query, encode_acceptance_receipt,
    encode_new_sale_query,
};
use bss_orders_sdk::gate_report::GateReport;
use bss_pricing_sdk::Digest;
use bss_pricing_sdk::acceptance::{self, AcceptanceReceipt, CommercialReason, NewSaleQuery, Term};
use bss_pricing_sdk::digest::{billing_terms_digest, request_digest, selected_bindings_digest};
use bss_pricing_sdk::read::{BindingSelection, ResolvedBindings};
use bss_pricing_sdk::terms::{BillingAnchor, BillingCycle, BillingTerms, TermsSource, Timezone};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;
use time::{Date, OffsetDateTime, Time};
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// The command segment of a submit line key.
pub const SUBMIT_COMMAND: &str = "submit";
/// The `line_requests` member holding the [`FrozenOrder`]; every other member is a line id.
pub const ORDER_MEMBER: &str = "order";

/// Why a line's Pricing request cannot be frozen. Each names the line and a static member.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CommercialError {
    /// An input the request needs is absent from the run.
    #[error("line {line_id}: {what} is absent")]
    Incomplete {
        /// The line.
        line_id: Uuid,
        /// The absent member.
        what: &'static str,
    },
    /// A value the Pricing request cannot express.
    #[error("line {line_id}: {what} is not supported")]
    Unsupported {
        /// The line.
        line_id: Uuid,
        /// The unsupported member.
        what: &'static str,
    },
    /// Two answers disagree (a stated digest, or bindings that do not cover the selections).
    #[error("line {line_id}: {what} disagrees")]
    Mismatch {
        /// The line.
        line_id: Uuid,
        /// The disagreeing member.
        what: &'static str,
    },
}

/// Everything the line requests read: the order, the basket lines and the run's answers.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct CommercialFacts<'a> {
    /// The order.
    pub order_id: Uuid,
    /// The order's tenant axes.
    pub axes: TenantAxes,
    /// The basket lines, in request order.
    pub lines: &'a [BasketLine],
    /// Each line's Pricing `resolve` answer.
    pub resolved_bindings: &'a BTreeMap<Uuid, Arc<ResolvedBindings>>,
    /// Each line's resolved billing terms.
    pub terms: &'a BTreeMap<Uuid, ResolvedTerms>,
    /// Each line's validated item collection, or an explicit legacy singleton.
    pub keys: &'a BTreeMap<Uuid, Vec<super::answers::OverlapKey>>,
    /// The confirmed payer's market.
    pub market: Option<&'a Market>,
    /// The run's frozen date basis.
    pub basis: &'a DateBasis,
    /// The seller's acceptance policy.
    pub policy: &'a AcceptancePolicy,
}

/// One line's frozen Pricing request.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineRequest {
    /// The line.
    pub line_id: Uuid,
    /// The stable command key ([`command_key`]).
    pub key: String,
    /// The complete query.
    pub query: NewSaleQuery,
    /// `request_digest(query)`; stored as 64 lowercase hexadecimal digits ([`digest_hex`]).
    pub request_digest: Digest,
    /// The line's overlap scope key, absent without a selected paid recurring SKU.
    pub overlap_scope_key: Option<String>,
    /// Exact returned product identity; absent for keyless lines and legacy attempts.
    pub catalog_subscription_product_key: Option<String>,
    /// Complete new profile; `None` preserves the distinct legacy scalar representation.
    pub overlap_item_keys: Option<Vec<super::item_keys::ItemOverlapKey>>,
}

/// What an attempt freezes once: the assessment the pins name, the evaluation the totals are
/// written from, and the run's gate report and outcome vector, so a recovered commit settles the
/// same report and writes the same diagnostics as the fresh one (module doc; D-231).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenOrder {
    /// The assessment (gate run) identity.
    pub assessment_id: Uuid,
    /// When the assessment was evaluated (also the diagnostics' `evaluated_at`).
    #[serde(with = "time::serde::rfc3339")]
    pub assessed_at: OffsetDateTime,
    /// The UTC date the catalog resolved on (`YYYY-MM-DD`).
    #[serde(with = "super::answers::iso_date")]
    pub resolve_date: Date,
    /// The exact input sent to Rating in the passing run, frozen before acceptance dispatch.
    /// `None` explicitly represents a legacy attempt written before request capture; recovery
    /// retains that absence and never fabricates a request from current basket facts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluation_request: Option<OrderEvaluationRequest>,
    /// The Rating evaluation of the passing run.
    pub evaluation: OrderEvaluation,
    /// The authorized gate report the settlement carries, stored as its wire value.
    pub report: GateReport,
    /// The run's request correlation, recorded with its diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<Uuid>,
    /// The run's complete outcome vector, written as the commit's diagnostics (M2-2).
    pub vector: Vector,
}

/// One line of a stored attempt, decoded ([`decode_line_requests`]).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenLine {
    /// The line.
    pub line_id: Uuid,
    /// The line's command key.
    pub key: String,
    /// The frozen query.
    pub query: NewSaleQuery,
    /// The frozen request digest.
    pub request_digest: Digest,
    /// The frozen overlap scope key; null for non-recurring lines.
    pub overlap_scope_key: Option<String>,
    /// Exact returned product identity; absent for keyless lines and legacy attempts.
    pub catalog_subscription_product_key: Option<String>,
    /// Complete new profile; `None` preserves the distinct legacy scalar representation.
    pub overlap_item_keys: Option<Vec<super::item_keys::ItemOverlapKey>>,
}

impl FrozenLine {
    /// All frozen scopes; no arbitrary scalar projection of a collection.
    #[must_use]
    pub fn scopes(&self) -> std::collections::BTreeSet<&str> {
        self.overlap_item_keys.as_ref().map_or_else(
            || self.overlap_scope_key.as_deref().into_iter().collect(),
            |items| super::item_keys::distinct_scopes(items),
        )
    }

    /// Validate complete receipt coverage, or the explicitly limited legacy branch.
    #[must_use]
    pub fn matches_bindings(&self, bindings: &[bss_pricing_sdk::read::AcceptedBinding]) -> bool {
        if let Some(items) = &self.overlap_item_keys {
            self.overlap_scope_key.is_none()
                && self.catalog_subscription_product_key.is_none()
                && super::item_keys::matches_receipt(items, &self.query, bindings)
        } else {
            let recurring = bindings
                .iter()
                .filter(|b| b.kind == bss_pricing_sdk::read::ChargeKind::Recurring)
                .count();
            match recurring {
                0 => {
                    self.overlap_scope_key.is_none()
                        && self.catalog_subscription_product_key.is_none()
                }
                1 => {
                    self.overlap_scope_key
                        .as_ref()
                        .is_some_and(|s| super::item_keys::valid_text(s))
                        && self
                            .catalog_subscription_product_key
                            .as_ref()
                            .is_none_or(|s| super::item_keys::valid_text(s))
                }
                _ => false,
            }
        }
    }
}

/// A stored attempt's `line_requests`, decoded: its lines in line-id order and its order facts.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenRequests {
    /// The lines.
    pub lines: Vec<FrozenLine>,
    /// The order facts.
    pub order: FrozenOrder,
}

/// A stored `line_requests` value that is not the [`line_requests_value`] spelling.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the frozen line requests are malformed: {member}{}", codec.map(|c| format!(" ({c})")).unwrap_or_default())]
pub struct MalformedRequests {
    /// The member that is not the stored spelling.
    pub member: &'static str,
    /// The codec category when the member failed to decode (D-220 item 1: the category, never
    /// the decoder message, D-44).
    pub codec: Option<CodecError>,
}

impl MalformedRequests {
    /// A malformed `member` with no codec cause.
    #[must_use]
    pub const fn member(member: &'static str) -> Self {
        Self {
            member,
            codec: None,
        }
    }

    /// A `member` whose codec failed with `codec`.
    #[must_use]
    pub const fn codec(member: &'static str, codec: CodecError) -> Self {
        Self {
            member,
            codec: Some(codec),
        }
    }
}

/// The submit command key of `line_id` in the attempt that reserved `candidate` (module doc).
#[must_use]
pub fn command_key(order_id: Uuid, candidate: i32, line_id: Uuid) -> String {
    format!("{order_id}/{candidate}/{line_id}/{SUBMIT_COMMAND}")
}

/// A digest as 64 lowercase hexadecimal digits (the schema-2 codec's spelling).
#[must_use]
pub fn digest_hex(digest: &Digest) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    digest
        .iter()
        .flat_map(|byte| [HEX[usize::from(byte >> 4)], HEX[usize::from(byte & 0xf)]])
        .map(char::from)
        .collect()
}

/// Every line's request, in basket order, for the attempt that reserved `candidate`.
///
/// # Errors
/// The first line whose request cannot be frozen (module doc); a non-positive candidate is
/// `Unsupported` on the first line.
pub fn build_line_requests(
    facts: &CommercialFacts<'_>,
    candidate: i32,
) -> Result<Vec<LineRequest>, CommercialError> {
    facts
        .lines
        .iter()
        .map(|line| line_request(facts, candidate, line))
        .collect()
}

/// The canonical `line_requests` value: `{line_id: {key, overlap_scope_key, query,
/// request_digest}, "order": FrozenOrder}`, the query in the schema-2 receipt codec's spelling.
/// Object members are emitted in key order, so equal requests always encode to identical bytes.
///
/// # Errors
/// `Unsupported` for a query the codec refuses to encode, or an order that does not serialize
/// (reported on the first line).
pub fn line_requests_value(
    requests: &[LineRequest],
    order: &FrozenOrder,
) -> Result<Value, CommercialError> {
    let mut lines = BTreeMap::new();
    for r in requests {
        let query = encode_new_sale_query(&r.query).map_err(|_| CommercialError::Unsupported {
            line_id: r.line_id,
            what: "query encoding",
        })?;
        let mut entry: Map<String, Value> = [
            ("key".to_owned(), Value::String(r.key.clone())),
            (
                "overlap_scope_key".to_owned(),
                r.overlap_scope_key
                    .clone()
                    .map_or(Value::Null, Value::String),
            ),
            (
                "catalog_subscription_product_key".to_owned(),
                r.catalog_subscription_product_key
                    .clone()
                    .map_or(Value::Null, Value::String),
            ),
            ("query".to_owned(), canonical(query)),
            (
                "request_digest".to_owned(),
                Value::String(digest_hex(&r.request_digest)),
            ),
        ]
        .into_iter()
        .collect();
        if let Some(items) = &r.overlap_item_keys {
            if r.overlap_scope_key.is_some() || r.catalog_subscription_product_key.is_some() {
                return Err(CommercialError::Mismatch {
                    line_id: r.line_id,
                    what: "mixed overlap evidence",
                });
            }
            entry.remove("overlap_scope_key");
            entry.remove("catalog_subscription_product_key");
            entry.insert(
                "item_keys_v1".into(),
                serde_json::to_value(items).map_err(|_| CommercialError::Unsupported {
                    line_id: r.line_id,
                    what: "item keys",
                })?,
            );
        }
        lines.insert(r.line_id.to_string(), Value::Object(entry));
    }
    let order = serde_json::to_value(order).map_err(|_| CommercialError::Unsupported {
        line_id: requests.first().map_or(Uuid::nil(), |r| r.line_id),
        what: "frozen order",
    })?;
    lines.insert(ORDER_MEMBER.to_owned(), canonical(order));
    Ok(Value::Object(lines.into_iter().collect()))
}

/// D-197 exact acceptance-input equality: one line's frozen Pricing `query` against the frozen
/// Rating `request` the attempt's totals were evaluated from (selection, quantity, market,
/// revision, start, term, cycle, billing terms, axes and hold policy). Members a legacy request
/// snapshot omits (plan, start, resolved terms, axes, policy) are not compared and never
/// fabricated ([`FrozenOrder::evaluation_request`]).
///
/// # Errors
/// The static name of the first member that differs.
pub fn verify_evaluated_query(
    query: &NewSaleQuery,
    request: &OrderEvaluationRequest,
) -> Result<(), &'static str> {
    let mut lines = request.lines.iter().filter(|l| l.line_id == query.line_id);
    let (Some(line), None) = (lines.next(), lines.next()) else {
        return Err("evaluated line");
    };
    if request.market.currency != query.market.currency
        || query.market.region.as_deref() != Some(request.market.region.as_str())
    {
        return Err("evaluated market");
    }
    if request.tenant_axes.as_ref().is_some_and(|a| {
        (a.seller, a.payer, a.resource)
            != (
                query.tenant_axes.seller_tenant_id,
                query.tenant_axes.payer_tenant_id,
                query.tenant_axes.resource_tenant_id,
            )
    }) {
        return Err("evaluated tenant axes");
    }
    if request
        .acceptance_policy
        .as_ref()
        .is_some_and(|p| positive(&p.version) != Some(query.hold_policy_version))
    {
        return Err("evaluated hold policy version");
    }
    if line.plan_revision_id != query.plan_revision_id
        || line.plan_id.is_some_and(|p| p != query.plan_id)
    {
        return Err("evaluated revision");
    }
    if term(line.term) != query.term {
        return Err("evaluated term");
    }
    if line.start_at.is_some_and(|s| s != query.start_at) {
        return Err("evaluated start");
    }
    let cycle = match query.billing_terms.cycle {
        BillingCycle::Month => EvaluationCycle::Month,
        BillingCycle::Year => EvaluationCycle::Year,
    };
    if line.cycle != cycle {
        return Err("evaluated cycle");
    }
    if line.resolved_terms.as_ref().is_some_and(|t| {
        t.term != line.term || t.billing_terms.digest != digest_hex(&query.billing_terms.digest)
    }) {
        return Err("evaluated billing terms");
    }
    if line.items.len() != query.selections.len()
        || !line
            .items
            .iter()
            .zip(&query.selections)
            .all(|(item, selection)| {
                item.item_id == selection.item_id
                    && matches!(
                        item.chains.as_slice(),
                        [chain] if chain.dim_value == selection.dimension_value
                    )
                    && bss_money::parse_decimal(&item.quantity).is_ok_and(|q| q == query.quantity)
            })
    {
        return Err("evaluated selections");
    }
    Ok(())
}

/// D-197: the issued receipt's `bindings` are the ones the frozen Rating `request` evaluated for
/// `query`'s line, one per selected item. A legacy request binding without its native member is
/// compared on the projected identity only.
///
/// # Errors
/// `"evaluated line"` when the request does not price the line; `"evaluated bindings"` for any
/// other difference.
pub fn verify_evaluated_bindings(
    query: &NewSaleQuery,
    bindings: &[bss_pricing_sdk::read::AcceptedBinding],
    request: &OrderEvaluationRequest,
) -> Result<(), &'static str> {
    let line = request
        .lines
        .iter()
        .find(|l| l.line_id == query.line_id)
        .ok_or("evaluated line")?;
    if bindings.len() != line.items.len() {
        return Err("evaluated bindings");
    }
    for item in &line.items {
        let [chain] = item.chains.as_slice() else {
            return Err("evaluated bindings");
        };
        let issued = bindings
            .iter()
            .find(|b| b.item_id == item.item_id)
            .ok_or("evaluated bindings")?;
        let mut expected =
            super::orchestrate::evaluation_binding(issued).map_err(|_| "evaluated bindings")?;
        if chain.binding.native_binding.is_none() {
            expected.native_binding = None;
        }
        if expected != chain.binding {
            return Err("evaluated bindings");
        }
    }
    Ok(())
}

/// The attempt's `line_requests` for `candidate`: [`build_line_requests`], each request checked
/// against the frozen Rating request when the order carries one ([`verify_evaluated_query`]),
/// then [`line_requests_value`]. A legacy order without a request is built as before.
///
/// # Errors
/// The first line whose request cannot be frozen, differs from the evaluated request
/// (`Mismatch`, naming the member), or cannot be encoded.
pub fn frozen_line_requests(
    facts: &CommercialFacts<'_>,
    candidate: i32,
    order: &FrozenOrder,
) -> Result<Value, CommercialError> {
    let requests = build_line_requests(facts, candidate)?;
    if let Some(evaluated) = &order.evaluation_request {
        if evaluated.lines.len() != requests.len() {
            return Err(CommercialError::Mismatch {
                line_id: requests.first().map_or(Uuid::nil(), |r| r.line_id),
                what: "evaluated lines",
            });
        }
        for r in &requests {
            verify_evaluated_query(&r.query, evaluated).map_err(|what| {
                CommercialError::Mismatch {
                    line_id: r.line_id,
                    what,
                }
            })?;
        }
    }
    line_requests_value(&requests, order)
}

/// Decode a stored attempt's `line_requests` ([`line_requests_value`]).
///
/// # Errors
/// [`MalformedRequests`] naming the first member that is not that spelling: a recovered
/// attempt never commits from evidence it cannot read.
pub fn decode_line_requests(value: &Value) -> Result<FrozenRequests, MalformedRequests> {
    let members = value
        .as_object()
        .ok_or(MalformedRequests::member("not an object"))?;
    let order: FrozenOrder = serde_json::from_value(
        members
            .get(ORDER_MEMBER)
            .cloned()
            .ok_or(MalformedRequests::member("order"))?,
    )
    .map_err(|_| MalformedRequests::member("order"))?;
    let mut lines = Vec::with_capacity(members.len().saturating_sub(1));
    for (member, entry) in members {
        if member == ORDER_MEMBER {
            continue;
        }
        let line_id = Uuid::parse_str(member).map_err(|_| MalformedRequests::member("line id"))?;
        let entry = entry
            .as_object()
            .ok_or(MalformedRequests::member("line entry"))?;
        let new_profile = entry.contains_key("item_keys_v1");
        if (new_profile
            && (entry.len() != 4
                || entry.contains_key("overlap_scope_key")
                || entry.contains_key("catalog_subscription_product_key")))
            || (!new_profile
                && entry.len()
                    != 4 + usize::from(entry.contains_key("catalog_subscription_product_key")))
        {
            return Err(MalformedRequests::member("line entry members"));
        }
        let text = |name: &'static str| {
            entry
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or(MalformedRequests::member(name))
        };
        let query = decode_new_sale_query(
            entry
                .get("query")
                .ok_or(MalformedRequests::member("query"))?,
        )
        .map_err(|e| MalformedRequests::codec("query", e))?;
        let frozen_digest = parse_digest(&text("request_digest")?)
            .ok_or(MalformedRequests::member("request digest"))?;
        if query.line_id != line_id || frozen_digest != request_digest(&query) {
            return Err(MalformedRequests::member("request identity"));
        }
        let overlap_item_keys = if new_profile {
            let items: Vec<super::item_keys::ItemOverlapKey> =
                serde_json::from_value(entry["item_keys_v1"].clone())
                    .map_err(|_| MalformedRequests::member("item_keys_v1"))?;
            if items.len() > query.selections.len()
                || items.iter().enumerate().any(|(index, item)| {
                    !item.valid()
                        || item.selection.line_id != line_id
                        || item.selection.plan_revision_id != query.plan_revision_id
                        || !query.selections.iter().any(|s| {
                            s.item_id == item.selection.item_id
                                && s.dimension_value == item.selection.dimension_value
                        })
                        || items[..index].iter().any(|other| {
                            other.selection.item_id == item.selection.item_id
                                && other.selection.dimension_value == item.selection.dimension_value
                        })
                })
            {
                return Err(MalformedRequests::member("item key identity"));
            }
            Some(items)
        } else {
            None
        };
        lines.push(FrozenLine {
            line_id,
            key: text("key")?,
            query,
            request_digest: frozen_digest,
            overlap_scope_key: match entry.get("overlap_scope_key") {
                None if new_profile => None,
                Some(Value::Null) => None,
                Some(Value::String(key)) => Some(key.clone()),
                _ => return Err(MalformedRequests::member("overlap_scope_key")),
            },
            catalog_subscription_product_key: match entry.get("catalog_subscription_product_key") {
                None | Some(Value::Null) => None,
                Some(Value::String(key)) => Some(key.clone()),
                _ => {
                    return Err(MalformedRequests::member(
                        "catalog_subscription_product_key",
                    ));
                }
            },
            overlap_item_keys,
        });
    }
    if lines.is_empty() {
        return Err(MalformedRequests::member("no lines"));
    }
    Ok(FrozenRequests { lines, order })
}

/// 64 lowercase hexadecimal digits as a digest ([`digest_hex`]'s inverse).
#[must_use]
pub fn parse_digest(text: &str) -> Option<Digest> {
    if text.len() != 64 || !text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut digest = [0u8; 32];
    for (i, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(digest)
}

/// `value` with every object's members in key order, whatever the map's own ordering.
fn canonical(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<String, Value> =
                map.into_iter().map(|(k, v)| (k, canonical(v))).collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
        other => other,
    }
}

fn line_request(
    facts: &CommercialFacts<'_>,
    candidate: i32,
    line: &BasketLine,
) -> Result<LineRequest, CommercialError> {
    let line_id = line.line_id;
    let incomplete = |what| CommercialError::Incomplete { line_id, what };
    let unsupported = |what| CommercialError::Unsupported { line_id, what };
    let mismatch = |what| CommercialError::Mismatch { line_id, what };

    let order_version = u64::try_from(candidate)
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(|| unsupported("order version"))?;
    let catalog = &line.catalog;
    let selections: Vec<BindingSelection> = catalog
        .items
        .iter()
        .map(|i| BindingSelection {
            item_id: i.item_id,
            dimension_value: i.selected_dim_value.clone(),
        })
        .collect();
    // D-192: the line's one exact quantity, stated on every selected item of a bundle.
    let mut quantities = catalog
        .items
        .iter()
        .map(|i| bss_money::parse_decimal(&i.quantity));
    let quantity = match quantities.next() {
        None => return Err(incomplete("selections")),
        Some(Err(_)) => return Err(unsupported("quantity")),
        Some(Ok(q)) => q,
    };
    for next in quantities {
        match next {
            Ok(q) if q == quantity => {}
            Ok(_) => return Err(unsupported("mixed item quantities")),
            Err(_) => return Err(unsupported("quantity")),
        }
    }
    if quantity.is_sign_negative() || quantity.is_zero() {
        return Err(unsupported("quantity"));
    }
    let market = facts.market.ok_or_else(|| incomplete("market"))?;
    let start_at = match facts.basis.lines.get(&line_id) {
        Some(LineResolution::Resolved { dates }) => dates
            .service_activation
            .date()
            .with_time(Time::MIDNIGHT)
            .assume_utc(),
        Some(LineResolution::Invalid { .. }) | None => {
            return Err(incomplete("service activation date"));
        }
    };
    let terms = facts
        .terms
        .get(&line_id)
        .ok_or_else(|| incomplete("billing terms"))?;
    let snapshot = terms
        .snapshot
        .as_ref()
        .ok_or_else(|| incomplete("billing terms snapshot"))?;
    let billing_terms = billing_terms(snapshot).map_err(unsupported)?;
    if digest_hex(&billing_terms.digest) != terms.billing_terms.digest {
        return Err(mismatch("billing terms digest"));
    }
    let resolved = facts
        .resolved_bindings
        .get(&line_id)
        .ok_or_else(|| incomplete("resolved bindings"))?;
    if resolved.plan_id != catalog.plan_id || resolved.revision_id != catalog.revision_id {
        return Err(mismatch("resolved revision"));
    }
    let resolved_bindings_digest = selected_bindings_digest(resolved, &selections)
        .map_err(|_| mismatch("resolved bindings"))?;
    let hold_policy_version =
        positive(&facts.policy.version).ok_or_else(|| unsupported("hold policy version"))?;
    // Only selected bindings determine applicability; diagnostic/unselected cells do not.
    let recurring = resolved
        .cells
        .iter()
        .filter(|cell| selections.contains(&cell.selection))
        .filter_map(|cell| cell.binding.as_ref())
        .any(|binding| binding.kind == bss_pricing_sdk::read::ChargeKind::Recurring);
    let returned_keys = if recurring {
        facts
            .keys
            .get(&line_id)
            .cloned()
            .ok_or_else(|| incomplete("overlap scope key"))?
    } else {
        Vec::new()
    };
    let (overlap_item_keys, legacy_key) = match returned_keys.as_slice() {
        keys if keys.iter().all(|key| key.item_key.is_some()) => (
            Some(
                keys.iter()
                    .filter_map(|key| key.item_key.clone())
                    .collect::<Vec<_>>(),
            ),
            None,
        ),
        [single] if single.item_key.is_none() => (None, Some(single)),
        _ => return Err(mismatch("mixed overlap evidence")),
    };

    let query = NewSaleQuery {
        tenant_axes: acceptance::TenantAxes {
            seller_tenant_id: facts.axes.seller,
            payer_tenant_id: facts.axes.payer,
            resource_tenant_id: facts.axes.resource,
        },
        order_id: facts.order_id,
        order_version,
        line_id,
        plan_id: catalog.plan_id,
        plan_revision_id: catalog.revision_id,
        selections,
        quantity,
        market: acceptance::Market {
            currency: market.currency.clone(),
            region: Some(market.region.clone()),
        },
        start_at,
        term: term(terms.term),
        billing_terms,
        resolved_bindings_digest,
        hold_policy_version,
    };
    let accepted = query
        .selections
        .iter()
        .map(|selection| {
            resolved
                .cells
                .iter()
                .find(|c| c.selection == *selection)
                .and_then(|c| c.binding.clone())
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| mismatch("selected bindings"))?;
    if overlap_item_keys
        .as_ref()
        .is_some_and(|items| !super::item_keys::matches_receipt(items, &query, &accepted))
        || (overlap_item_keys.is_none()
            && accepted
                .iter()
                .filter(|b| b.kind == bss_pricing_sdk::read::ChargeKind::Recurring)
                .count()
                != 1)
    {
        return Err(mismatch("overlap evidence coverage"));
    }
    let request_digest = request_digest(&query);
    Ok(LineRequest {
        line_id,
        key: command_key(facts.order_id, candidate, line_id),
        query,
        request_digest,
        overlap_scope_key: legacy_key.map(|key| key.overlap_scope_key.clone()),
        catalog_subscription_product_key: legacy_key
            .map(|key| key.catalog_subscription_product_key.clone()),
        overlap_item_keys,
    })
}

/// A canonical positive integer (`7`, never `07` or `+7`).
fn positive(text: &str) -> Option<u64> {
    text.parse::<u64>()
        .ok()
        .filter(|v| *v > 0 && v.to_string() == text)
}

const fn term(t: EvaluationTerm) -> Term {
    match t {
        EvaluationTerm::Rolling => Term::Rolling,
        EvaluationTerm::FixedPeriods { count } => Term::FixedPeriods { count },
    }
}

/// The Pricing projection of a stated snapshot, its digest recomputed; the static member that
/// Pricing cannot express otherwise.
fn billing_terms(s: &BillingTermsSnapshot) -> Result<BillingTerms, &'static str> {
    if s.schema_version != 1 {
        return Err("billing terms schema version");
    }
    let timezone = Timezone::from_str(&s.timezone).map_err(|_| "billing terms timezone")?;
    let source = match s.source {
        TermsSnapshotSource::ExplicitOrder => TermsSource::ExplicitOrder,
        TermsSnapshotSource::SellerPolicy { id, version } if version > 0 => {
            TermsSource::SellerPolicy { id, version }
        }
        TermsSnapshotSource::SellerPolicy { .. } => return Err("billing terms source"),
    };
    let mut terms = BillingTerms {
        schema_version: s.schema_version,
        cycle: match s.cycle {
            EvaluationCycle::Month => BillingCycle::Month,
            EvaluationCycle::Year => BillingCycle::Year,
        },
        anchor: match s.anchor {
            TermsAnchor::Calendar => BillingAnchor::Calendar,
            TermsAnchor::SubscriptionStart => BillingAnchor::SubscriptionStart,
        },
        anchor_at: s.anchor_at,
        timezone,
        source,
        digest: [0; 32],
    };
    terms.digest = billing_terms_digest(&terms);
    Ok(terms)
}

/// One line's recorded phase-B result (D-188 step 3): the issued receipt, or Pricing's business
/// refusal. Infrastructure failures are never recorded (the line is retried later).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptResult {
    /// Pricing issued the receipt.
    Accepted(Box<AcceptanceReceipt>),
    /// Pricing refused the line with one of [`RECORDED_REFUSALS`].
    Refused(CommercialReason),
}

/// Pricing's terminal term refusals, carried in an `InvalidArgument` `commercial_terms`
/// field violation. These describe unsupported commercial intent, not request corruption.
pub const RECORDED_TERMS_REFUSALS: [CommercialReason; 8] = [
    CommercialReason::UnsupportedTerms,
    CommercialReason::UnsupportedModel,
    CommercialReason::UnsupportedWindow,
    CommercialReason::UnsupportedScope,
    CommercialReason::MissingBillingTerms,
    CommercialReason::BillingCycleMismatch,
    CommercialReason::UnalignedBillingAnchor,
    CommercialReason::InvalidTermCount,
];

/// The Pricing business refusals a line records as its result. Other invalid arguments
/// remain Orders request defects; denials are authority outages and key/acceptance conflicts
/// ([`CONTRACT_DEFECT_REFUSALS`]) are broken command contracts. None of those is recorded.
///
/// Built from its two parts, the sale refusals then [`RECORDED_TERMS_REFUSALS`], so neither is
/// copied by hand; a length that does not add up fails const evaluation, at compile time.
pub const RECORDED_REFUSALS: [CommercialReason; 15] = {
    let mut out = [CommercialReason::NotSellable; 15];
    let (mut i, mut j) = (0, 0);
    while i < SALE_REFUSALS.len() {
        out[i] = SALE_REFUSALS[i];
        i += 1;
    }
    while j < RECORDED_TERMS_REFUSALS.len() {
        out[i + j] = RECORDED_TERMS_REFUSALS[j];
        j += 1;
    }
    assert!(i + j == 15);
    out
};

/// Pricing's sale refusals: the line is no longer sellable as resolved and held.
const SALE_REFUSALS: [CommercialReason; 7] = [
    CommercialReason::NotSellable,
    CommercialReason::ResolutionChanged,
    CommercialReason::ActivationOutsideAcceptedWindow,
    CommercialReason::HoldExpired,
    CommercialReason::PriceClosed,
    CommercialReason::SkuRetired,
    CommercialReason::MarketChanged,
];

/// The `aborted` Pricing reasons that mean the command contract broke, not that the sale was
/// refused: the frozen key met another request (`IDEMPOTENCY_CONFLICT`) or another acceptance
/// (`ACCEPTANCE_MISMATCH`). The command key and request are frozen (D-188), so neither can
/// happen between a correct Orders and a correct Pricing: phase B records nothing and answers an
/// integration failure, never a settled business refusal.
pub const CONTRACT_DEFECT_REFUSALS: [CommercialReason; 2] = [
    CommercialReason::IdempotencyConflict,
    CommercialReason::AcceptanceMismatch,
];

/// The member of `reasons` an `Aborted` Pricing error carries (its `as_str` spelling, as the
/// SDK builds it, or its domain code).
fn aborted_reason<const N: usize>(
    error: &CanonicalError,
    reasons: [CommercialReason; N],
) -> Option<CommercialReason> {
    let CanonicalError::Aborted { ctx, .. } = error else {
        return None;
    };
    reasons
        .into_iter()
        .find(|r| ctx.reason == r.as_str() || ctx.reason == r.code())
}

/// The recorded business refusal a Pricing `check` error carries: an `Aborted`
/// sale refusal or one of the exact terminal `commercial_terms` field violations.
#[must_use]
pub fn recorded_refusal(error: &CanonicalError) -> Option<CommercialReason> {
    if let CanonicalError::InvalidArgument {
        ctx:
            toolkit_canonical_errors::context::InvalidArgument::FieldViolations { field_violations },
        ..
    } = error
    {
        return field_violations.iter().find_map(|violation| {
            (violation.field == "commercial_terms")
                .then(|| {
                    RECORDED_TERMS_REFUSALS
                        .into_iter()
                        .find(|r| r.as_str() == violation.reason || r.code() == violation.reason)
                })
                .flatten()
        });
    }
    // Term refusals are recognized only in their SDK InvalidArgument encoding.
    aborted_reason(error, RECORDED_REFUSALS)
        .filter(|reason| !RECORDED_TERMS_REFUSALS.contains(reason))
}

/// The command-contract defect a Pricing `check` error carries, if it is one: an `Aborted`
/// error whose reason is a [`CONTRACT_DEFECT_REFUSALS`] member.
#[must_use]
pub fn contract_defect(error: &CanonicalError) -> Option<CommercialReason> {
    aborted_reason(error, CONTRACT_DEFECT_REFUSALS)
}

/// The stored value of a line result: `{"accepted": <receipt in the schema-2 spelling>}` or
/// `{"refused": {"reason": "<CODE>"}}`.
///
/// # Errors
/// `Unsupported` (on `line_id`) for a receipt the codec refuses to encode.
pub fn receipt_result_value(
    line_id: Uuid,
    result: &ReceiptResult,
) -> Result<Value, CommercialError> {
    Ok(match result {
        ReceiptResult::Accepted(receipt) => {
            let receipt =
                encode_acceptance_receipt(receipt).map_err(|_| CommercialError::Unsupported {
                    line_id,
                    what: "receipt encoding",
                })?;
            serde_json::json!({ "accepted": canonical(receipt) })
        }
        ReceiptResult::Refused(reason) => {
            serde_json::json!({ "refused": { "reason": reason.code() } })
        }
    })
}

/// Decode a stored line result ([`receipt_result_value`]).
///
/// # Errors
/// [`MalformedRequests`] for any other shape or an unrecorded reason.
pub fn decode_receipt_result(value: &Value) -> Result<ReceiptResult, MalformedRequests> {
    let members = value
        .as_object()
        .ok_or(MalformedRequests::member("receipt result"))?;
    match (
        members.len(),
        members.get("accepted"),
        members.get("refused"),
    ) {
        (1, Some(receipt), None) => decode_acceptance_receipt(receipt)
            .map(|r| ReceiptResult::Accepted(Box::new(r)))
            .map_err(|e| MalformedRequests::codec("receipt", e)),
        (1, None, Some(refused)) => {
            let code = refused
                .as_object()
                .filter(|m| m.len() == 1)
                .and_then(|m| m.get("reason"))
                .and_then(Value::as_str)
                .ok_or(MalformedRequests::member("refusal"))?;
            RECORDED_REFUSALS
                .into_iter()
                .find(|r| r.code() == code)
                .map(ReceiptResult::Refused)
                .ok_or(MalformedRequests::member("refusal reason"))
        }
        _ => Err(MalformedRequests::member("receipt result")),
    }
}

/// Where the commit settles a recorded Pricing refusal (M2-6): the registered guard and reason.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalDisposition {
    /// The composite gate fails with this reason.
    Composite(Reason),
    /// `gate.binding-admission` fails with this reason (the accepted binding lapsed).
    BindingAdmission(Reason),
    /// The catalog-predicates input is unresolvable with this reason.
    Unevaluable(Reason),
}

/// The disposition of a recorded refusal: a sale Pricing no longer offers is
/// `catalog-predicate-failed`; a lapsed hold, or an activation outside the window the receipt
/// accepted, is the accepted binding no longer admitting the order (`order-binding-expired`).
/// The terminal unsupported-term reasons fail the composite gate with
/// `billing-terms-unsupported`. Any other reason is never recorded ([`RECORDED_REFUSALS`]);
/// it would leave the catalog predicates unevaluable (`catalog-predicates-unavailable`).
#[must_use]
pub const fn refusal_disposition(reason: CommercialReason) -> RefusalDisposition {
    match reason {
        CommercialReason::NotSellable
        | CommercialReason::ResolutionChanged
        | CommercialReason::MarketChanged
        | CommercialReason::SkuRetired
        | CommercialReason::PriceClosed => {
            RefusalDisposition::Composite(Reason::CatalogPredicateFailed)
        }
        CommercialReason::UnsupportedTerms
        | CommercialReason::UnsupportedModel
        | CommercialReason::UnsupportedWindow
        | CommercialReason::UnsupportedScope
        | CommercialReason::MissingBillingTerms
        | CommercialReason::BillingCycleMismatch
        | CommercialReason::UnalignedBillingAnchor
        | CommercialReason::InvalidTermCount => {
            RefusalDisposition::Composite(Reason::BillingTermsUnsupported)
        }
        CommercialReason::HoldExpired | CommercialReason::ActivationOutsideAcceptedWindow => {
            RefusalDisposition::BindingAdmission(Reason::OrderBindingExpired)
        }
        _ => RefusalDisposition::Unevaluable(Reason::CatalogPredicatesUnavailable),
    }
}

/// The encoded bytes of one persisted column value, the unit of the version limit
/// ([`version_capacity`]): a value's PostgreSQL binary width for fixed-width types, its UTF-8
/// length for text and its serialized JSON length for `jsonb`; a NULL counts nothing.
pub mod persisted {
    use serde_json::Value;
    /// `uuid`.
    pub const UUID: usize = 16;
    /// `integer`.
    pub const INT: usize = 4;
    /// `date`.
    pub const DATE: usize = 4;
    /// `timestamptz`.
    pub const TIMESTAMP: usize = 8;
    /// `interval`.
    pub const INTERVAL: usize = 16;
    /// `text`.
    #[must_use]
    pub const fn text(value: &str) -> usize {
        value.len()
    }
    /// Nullable `text`.
    #[must_use]
    pub fn opt_text(value: Option<&str>) -> usize {
        value.map_or(0, text)
    }
    /// `jsonb`, as its serialized JSON (fails closed: an unserializable value never fits).
    #[must_use]
    pub fn json(value: &Value) -> usize {
        serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
    }
    /// A nullable fixed-width value.
    #[must_use]
    pub fn opt<T>(value: Option<&T>, width: usize) -> usize {
        value.map_or(0, |_| width)
    }
}

/// The persisted bytes of the `order_version` row a commit appends for `version` (every column:
/// order, version, supersedes, market, payer, category, contract, the actor's hyphenated UUID
/// reference and tenant, the trigger token as `reason`, the amendment reason and the creation
/// instant).
#[must_use]
pub fn version_row_bytes(version: &VersionContribution, reason: &str) -> usize {
    /// `AuditActor::reference`: a hyphenated UUID.
    const ACTOR_REFERENCE: usize = 36;
    persisted::UUID
        + persisted::INT
        + persisted::INT
        + persisted::opt_text(version.market_currency.as_deref())
        + persisted::opt_text(version.market_region.as_deref())
        + persisted::UUID
        + persisted::text(&version.category)
        + persisted::opt(version.contract_id.as_ref(), persisted::UUID)
        + ACTOR_REFERENCE
        + persisted::UUID
        + persisted::text(reason)
        + persisted::opt_text(version.amendment_reason.as_deref())
        + persisted::TIMESTAMP
}

/// The order-wide consumed-item admission bound (D-158): sum selected item counts across
/// every draft line, without deduplication or quantity multiplication.
#[must_use]
pub fn consumed_item_capacity(counts: impl IntoIterator<Item = usize>) -> GuardVerdict {
    let total = counts.into_iter().fold(0usize, usize::saturating_add);
    if total <= bss_orders_sdk::authoring::MAX_CONSUMED_ITEMS_PER_ORDER {
        GuardVerdict::Pass
    } else {
        GuardVerdict::Fail(Reason::PurchaseCapacityExceeded)
    }
}

/// The serialized immutable-version limit (DESIGN "Capacity and publication", D-192): the
/// version's persisted bytes, every schema-2 pin (receipt, query and bindings) and all other
/// persisted version content included (the line rows, the totals rows and the version row, each
/// counted column by column with [`persisted`]), may not exceed
/// [`MAX_VERSION_BYTES`](bss_orders_sdk::commercial::MAX_VERSION_BYTES); beyond it the admission
/// refuses `purchase-capacity-exceeded` before the commercial commit. A selected receipt is never
/// truncated.
#[must_use]
pub fn version_capacity(persisted: impl IntoIterator<Item = usize>) -> GuardVerdict {
    let total = persisted.into_iter().fold(0usize, usize::saturating_add);
    if total <= bss_orders_sdk::commercial::MAX_VERSION_BYTES {
        GuardVerdict::Pass
    } else {
        GuardVerdict::Fail(Reason::PurchaseCapacityExceeded)
    }
}

/// The receipt the development acceptance double issues (M2-6; `infra::gate::stubs`), pure so
/// the pure tier pins it: the bindings are `resolved`'s cells of the query's selections, in
/// selection order, and must digest to the query's `resolved_bindings_digest`; the request and
/// terms digests are Pricing's own functions; `hold_until = accepted_at + hold_seconds`.
///
/// # Errors
/// [`CommercialReason::ResolutionChanged`] when the bindings are another revision's, do not cover
/// the selections or digest differently.
pub fn deterministic_receipt(
    query: NewSaleQuery,
    resolved: &ResolvedBindings,
    acceptance_id: Uuid,
    accepted_at: OffsetDateTime,
    hold_seconds: u32,
) -> Result<AcceptanceReceipt, CommercialReason> {
    let changed = CommercialReason::ResolutionChanged;
    if (resolved.plan_id, resolved.revision_id) != (query.plan_id, query.plan_revision_id) {
        return Err(changed);
    }
    let digest = selected_bindings_digest(resolved, &query.selections).map_err(|_| changed)?;
    if digest != query.resolved_bindings_digest {
        return Err(changed);
    }
    let bindings = query
        .selections
        .iter()
        .map(|s| {
            resolved
                .cells
                .iter()
                .find(|c| c.selection == *s)
                .and_then(|c| c.binding.clone())
                .ok_or(changed)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(AcceptanceReceipt {
        acceptance_id,
        request_digest: request_digest(&query),
        terms_digest: bss_pricing_sdk::digest::terms_digest(&query, &bindings),
        accepted_at,
        hold_until: accepted_at + time::Duration::seconds(i64::from(hold_seconds)),
        bindings,
        query,
    })
}

/// Check that the row materialized at commit still names the frozen commercial selection.
#[must_use]
pub fn verify_commit_line(
    query: &NewSaleQuery,
    order_id: Uuid,
    version: u64,
    line: &BasketLine,
) -> bool {
    query.order_id == order_id
        && query.order_version == version
        && query.line_id == line.line_id
        && line.catalog.line_id == line.line_id
        && query.plan_id == line.catalog.plan_id
        && query.plan_revision_id == line.catalog.revision_id
        && line.currency.as_deref() == Some(query.market.currency.as_str())
        && query.selections.len() == line.catalog.items.len()
        && query
            .selections
            .iter()
            .zip(&line.catalog.items)
            .all(|(selection, item)| {
                let dimension_matches = selection.dimension_value == item.selected_dim_value;
                selection.item_id == item.item_id
                    && dimension_matches
                    && bss_money::parse_decimal(&item.quantity).is_ok_and(|q| q == query.quantity)
            })
}
