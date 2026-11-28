//! Versioned C00 canonical JSON projections. Public contracts never expose untyped JSON.
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-plan-quota-rules-store:p1
//! @cpt-dod:cpt-cf-bss-pricing-dod-plan-quota-rules-acceptance:p1
use crate::{
    Digest,
    acceptance::{AcceptedQuotaRule, AcceptedQuotaSnapshot, NewSaleQuery, TenantAxes},
    plan_quota::PlanQuotaRuleV1,
    read::{
        AcceptedBinding, BindingSelection, ChargeKind, ImmutablePrice, PinnedQuotaEvidence,
        PriceModel, QuotaOwnerRef, QuotaTerms, ResolvedBindings, ResolvedQuotaRule, Tier,
        UncoveredQuota,
    },
    terms::{
        AggregationScope, BillingCycle, BillingTiming, Fold, InputSource, InvoiceInputs,
        PartialWindow, RatingWindow, Reset, Rounding, Timezone, UsageRatingPolicy,
        UsageRatingPolicyInput,
    },
};
use rust_decimal::Decimal;
use std::collections::{BTreeMap, BTreeSet};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::crypto::hash::Sha256;

#[toolkit_canonical_errors::resource_error(gts_id!("cf.bss.pricing.plan.v1~"))]
struct BindingResource;

/// Restricted JSON: numeric meaning is always exact text before serialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CanonicalValue {
    /// JSON `null`.
    Null,
    /// JSON boolean.
    Bool(bool),
    /// JSON string; also carries exact decimal and identifier text.
    String(String),
    /// JSON array, encoded in element order.
    Array(Vec<Self>),
    /// JSON object, encoded with keys sorted by UTF-16 code units.
    Object(BTreeMap<String, Self>),
}
use CanonicalValue as V;
fn text(value: impl ToString + Copy) -> V {
    V::String(value.to_string())
}
fn object<const N: usize>(fields: [(&str, V); N]) -> V {
    V::Object(fields.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}
fn optional<T>(value: Option<T>, encode: impl FnOnce(T) -> V) -> V {
    value.map_or(V::Null, encode)
}
fn decimal(value: Decimal) -> V {
    text(value.normalize())
}
fn hex(value: &Digest) -> V {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    V::String(
        value
            .iter()
            .flat_map(|b| {
                [
                    char::from(HEX[usize::from(b >> 4)]),
                    char::from(HEX[usize::from(b & 15)]),
                ]
            })
            .collect(),
    )
}

/// RFC 8785 string escaping for valid Rust Unicode strings.
fn string(value: &str, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{0c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            '\0'..='\u{1f}' => {
                let n = ch as usize;
                out.push_str("\\u00");
                out.push(char::from(HEX[n >> 4]));
                out.push(char::from(HEX[n % 16]));
            }
            _ => out.push(ch),
        }
    }
    out.push('"');
}
fn encode(value: &V, out: &mut String) {
    match value {
        V::Null => out.push_str("null"),
        V::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        V::String(s) => string(s, out),
        V::Array(values) => {
            out.push('[');
            for (i, v) in values.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                encode(v, out);
            }
            out.push(']');
        }
        V::Object(fields) => {
            let mut fields: Vec<_> = fields.iter().collect();
            fields.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, (k, v)) in fields.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                string(k, out);
                out.push(':');
                encode(v, out);
            }
            out.push('}');
        }
    }
}
/// Encodes a value as canonical JSON bytes.
pub(crate) fn canonical_json_bytes(value: &V) -> Vec<u8> {
    let mut out = String::new();
    encode(value, &mut out);
    out.into_bytes()
}
/// SHA-256 over the canonical `{domain, payload}` envelope, domain-separating digests.
pub(crate) fn hash_document(domain: &str, payload: V) -> Digest {
    let bytes = canonical_json_bytes(&object([("domain", text(domain)), ("payload", payload)]));
    let hashed = Sha256::digest(&bytes);
    let mut out = [0u8; 32];
    out.copy_from_slice(hashed.as_ref());
    out
}
fn tiers(values: &[Tier]) -> V {
    V::Array(
        values
            .iter()
            .map(|t| {
                object([
                    ("up_to", optional(t.up_to, decimal)),
                    ("rate", decimal(t.rate)),
                ])
            })
            .collect(),
    )
}
fn model(value: &PriceModel) -> V {
    match value {
        PriceModel::Flat { amount } => {
            object([("kind", text("flat")), ("amount", decimal(*amount))])
        }
        PriceModel::PerUnit { unit_amount } => object([
            ("kind", text("per_unit")),
            ("unit_amount", decimal(*unit_amount)),
        ]),
        PriceModel::Volume { tiers: t } => object([("kind", text("volume")), ("tiers", tiers(t))]),
        PriceModel::Graduated { tiers: t } => {
            object([("kind", text("graduated")), ("tiers", tiers(t))])
        }
        PriceModel::Package {
            package_size,
            package_price,
        } => object([
            ("kind", text("package")),
            ("package_size", decimal(*package_size)),
            ("package_price", decimal(*package_price)),
        ]),
    }
}
/// Financial content only: no identity, dates, closing observations or self digest. The currency
/// scale is money: the minimum fee and the invoice line post at it.
#[must_use]
pub fn money_digest(price: &ImmutablePrice) -> Digest {
    hash_document(
        "pricing.money.v1",
        object([
            ("currency", text(price.currency.code())),
            ("currency_scale", text(price.currency.scale())),
            ("model", model(&price.model)),
            ("minimum_fee", optional(price.minimum_fee, decimal)),
        ]),
    )
}
/// Exact template content; no Unicode or whitespace normalization.
#[must_use]
pub fn template_digest(template: &str) -> Digest {
    hash_document("pricing.template.v1", text(template))
}
fn policy_content(p: &UsageRatingPolicyInput) -> V {
    let window = match p.rating_window {
        RatingWindow::BillingCycle => text("billing_cycle"),
        RatingWindow::CalendarHour {
            timezone: Timezone::Utc,
        } => object([("kind", text("calendar_hour")), ("timezone", text("UTC"))]),
    };
    object([
        ("rating_window", window),
        (
            "aggregation_scope",
            text(match p.aggregation_scope {
                AggregationScope::SubscriptionLine => "subscription_line",
                AggregationScope::Resource => "resource",
            }),
        ),
        (
            "reset",
            text(match p.reset {
                Reset::RatingWindowStart => "rating_window_start",
            }),
        ),
        (
            "fold",
            text(match p.fold {
                Fold::Sum => "SUM",
            }),
        ),
        (
            "partial_window",
            text(match p.partial_window {
                PartialWindow::ActualQuantityFullThresholds => "actual_quantity_full_thresholds",
            }),
        ),
    ])
}
/// Entry policy content, independent of the policy record's identity/version.
#[must_use]
pub fn policy_digest(input: &UsageRatingPolicyInput) -> Digest {
    hash_document("pricing.policy.v1", policy_content(input))
}
fn policy(p: &UsageRatingPolicy) -> V {
    object([
        ("policy_id", text(p.policy_id)),
        ("version", text(p.version)),
        ("digest", hex(&p.digest)),
        ("content", policy_content(&p.content)),
    ])
}
/// The binding's price. `state` is not digested: a binding's price is always approved (D-520).
fn price(p: &ImmutablePrice) -> V {
    object([
        ("price_id", text(p.price_id)),
        ("price_book_entry_id", text(p.price_book_entry_id)),
        ("money_digest", hex(&p.money_digest)),
        ("currency", text(p.currency.code())),
        ("currency_scale", text(p.currency.scale())),
        ("model", model(&p.model)),
        ("minimum_fee", optional(p.minimum_fee, decimal)),
        ("effective_from", text(p.effective_from)),
        ("ends_on", optional(p.ends_on, text)),
    ])
}
fn invoice(i: &InvoiceInputs) -> V {
    object([
        ("template", text(&i.template)),
        ("template_digest", hex(&i.template_digest)),
        (
            "template_source",
            text(match i.template_source {
                InputSource::Entry => "entry",
                InputSource::SkuVersion => "sku_version",
                InputSource::SellerSettings => "seller_settings",
            }),
        ),
        ("gl_code", text(&i.gl_code)),
        ("tax_category", text(&i.tax_category)),
        (
            "timing",
            text(match i.timing {
                BillingTiming::Advance => "advance",
                BillingTiming::Arrears => "arrears",
            }),
        ),
        (
            "rounding",
            text(match i.rounding {
                Rounding::HalfEven => "half_even",
            }),
        ),
    ])
}
fn binding(b: &AcceptedBinding) -> V {
    object([
        ("item_id", text(b.item_id)),
        ("price_book_entry_id", text(b.price_book_entry_id)),
        ("dimension_key", optional(b.dimension_key.as_ref(), text)),
        (
            "dimension_value",
            optional(b.dimension_value.as_ref(), text),
        ),
        ("sku_id", text(b.sku_id)),
        ("sku_version", text(b.sku_version)),
        ("sku_code", text(&b.sku_code)),
        ("sku_name", text(&b.sku_name)),
        ("unit", optional(b.unit.as_ref(), text)),
        ("price", price(&b.price)),
        (
            "kind",
            text(match b.kind {
                ChargeKind::Recurring => "recurring",
                ChargeKind::Usage => "usage",
                ChargeKind::OneTime => "one_time",
            }),
        ),
        (
            "recurring_period",
            optional(b.recurring_period.as_ref(), |p| {
                text(match p {
                    BillingCycle::Month => "month",
                    BillingCycle::Year => "year",
                })
            }),
        ),
        ("via_default", V::Bool(b.via_default)),
        (
            "usage_rating_policy",
            optional(b.usage_rating_policy.as_ref(), policy),
        ),
        ("invoice", invoice(&b.invoice)),
    ])
}
fn invalid(code: &str) -> CanonicalError {
    BindingResource::invalid_argument()
        .with_field_violation("selections", code, code)
        .create()
}
/// Hash exactly the requested covered cells, sorted by item UUID and UTF-16 dimension value.
/// # Errors
/// Duplicate, unknown, uncovered or inconsistent selections/bindings are rejected.
pub fn selected_bindings_digest(
    resolved: &ResolvedBindings,
    selections: &[BindingSelection],
) -> Result<Digest, CanonicalError> {
    let mut seen = BTreeSet::new();
    let mut selected = Vec::with_capacity(selections.len());
    for s in selections {
        if !seen.insert((s.item_id, s.dimension_value.clone())) {
            return Err(invalid("DUPLICATE_SELECTION"));
        }
        let mut cells = resolved.cells.iter().filter(|c| c.selection == *s);
        let cell = cells.next().ok_or_else(|| invalid("UNKNOWN_SELECTION"))?;
        if cells.next().is_some() {
            return Err(invalid("DUPLICATE_CELL"));
        }
        let b = cell
            .binding
            .as_ref()
            .ok_or_else(|| invalid("UNCOVERED_SELECTION"))?;
        if b.item_id != s.item_id
            || b.dimension_value != s.dimension_value
            || b.price_book_entry_id != b.price.price_book_entry_id
            || b.sku_version <= 0
            || b.sku_code.trim().is_empty()
            || b.sku_name.trim().is_empty()
            || money_digest(&b.price) != b.price.money_digest
            || template_digest(&b.invoice.template) != b.invoice.template_digest
        {
            return Err(invalid("INVALID_BINDING"));
        }
        selected.push((s, b));
    }
    selected.sort_by(|(a, _), (b, _)| {
        compare_cells(
            (a.item_id, a.dimension_value.as_deref()),
            (b.item_id, b.dimension_value.as_deref()),
        )
    });
    let bindings = selected
        .into_iter()
        .map(|(s, b)| {
            object([
                (
                    "selection",
                    object([
                        ("item_id", text(s.item_id)),
                        (
                            "dimension_value",
                            optional(s.dimension_value.as_ref(), text),
                        ),
                    ]),
                ),
                ("binding", binding(b)),
            ])
        })
        .collect();
    Ok(hash_document(
        "pricing.bindings.v1",
        object([
            ("plan_id", text(resolved.plan_id)),
            ("revision_id", text(resolved.revision_id)),
            ("bindings", V::Array(bindings)),
        ]),
    ))
}

fn instant(at: time::OffsetDateTime) -> V {
    let at = at.to_offset(time::UtcOffset::UTC);
    text(&format!(
        "{}T{:02}:{:02}:{:02}.{:09}Z",
        at.date(),
        at.hour(),
        at.minute(),
        at.second(),
        at.nanosecond()
    ))
}
fn billing_terms(t: &crate::terms::BillingTerms) -> V {
    use crate::terms::{BillingAnchor, TermsSource};
    object([
        ("schema_version", text(t.schema_version)),
        (
            "cycle",
            text(match t.cycle {
                BillingCycle::Month => "month",
                BillingCycle::Year => "year",
            }),
        ),
        (
            "anchor",
            text(match t.anchor {
                BillingAnchor::Calendar => "calendar",
                BillingAnchor::SubscriptionStart => "subscription_start",
            }),
        ),
        ("anchor_at", instant(t.anchor_at)),
        (
            "timezone",
            text(match t.timezone {
                Timezone::Utc => "UTC",
            }),
        ),
        (
            "source",
            match t.source {
                TermsSource::ExplicitOrder => text("explicit_order"),
                TermsSource::SellerPolicy { id, version } => object([
                    ("kind", text("seller_policy")),
                    ("id", text(id)),
                    ("version", text(version)),
                ]),
            },
        ),
    ])
}
/// Canonical versioned invoice snapshot, excluding its own digest.
#[must_use]
pub fn billing_terms_digest(terms: &crate::terms::BillingTerms) -> Digest {
    hash_document("bss.billing-terms.v1", billing_terms(terms))
}
fn selection(s: &BindingSelection) -> V {
    object([
        ("item_id", text(s.item_id)),
        (
            "dimension_value",
            optional(s.dimension_value.as_ref(), text),
        ),
    ])
}
fn compare_cells(
    a: (uuid::Uuid, Option<&str>),
    b: (uuid::Uuid, Option<&str>),
) -> std::cmp::Ordering {
    a.0.cmp(&b.0).then_with(|| {
        a.1.map(|s| s.encode_utf16().collect::<Vec<_>>())
            .cmp(&b.1.map(|s| s.encode_utf16().collect::<Vec<_>>()))
    })
}
fn tenant_axes(t: &TenantAxes) -> V {
    object([
        ("seller_tenant_id", text(t.seller_tenant_id)),
        ("payer_tenant_id", text(t.payer_tenant_id)),
        ("resource_tenant_id", text(t.resource_tenant_id)),
    ])
}
/// The request's projection; `expected_quota_digest` is a member only when it is `Some`, so a
/// quota-free request keeps its bytes (D-548).
fn sale_query(q: &NewSaleQuery) -> V {
    use crate::acceptance::Term;
    let mut selections: Vec<_> = q.selections.iter().collect();
    selections.sort_by(|a, b| {
        compare_cells(
            (a.item_id, a.dimension_value.as_deref()),
            (b.item_id, b.dimension_value.as_deref()),
        )
    });
    let mut terms = billing_terms(&q.billing_terms);
    if let V::Object(fields) = &mut terms {
        fields.insert("digest".into(), hex(&q.billing_terms.digest));
    }
    let mut query = object([
        ("tenant_axes", tenant_axes(&q.tenant_axes)),
        ("order_id", text(q.order_id)),
        ("order_version", text(q.order_version)),
        ("line_id", text(q.line_id)),
        ("plan_id", text(q.plan_id)),
        ("plan_revision_id", text(q.plan_revision_id)),
        (
            "selections",
            V::Array(selections.into_iter().map(selection).collect()),
        ),
        ("quantity", decimal(q.quantity)),
        (
            "market",
            object([
                ("currency", text(&q.market.currency)),
                ("region", optional(q.market.region.as_ref(), text)),
            ]),
        ),
        ("start_at", instant(q.start_at)),
        (
            "term",
            match q.term {
                Term::Rolling => text("rolling"),
                Term::FixedPeriods { count } => {
                    object([("kind", text("fixed_periods")), ("count", text(count))])
                }
            },
        ),
        ("billing_terms", terms),
        ("resolved_bindings_digest", hex(&q.resolved_bindings_digest)),
        ("hold_policy_version", text(q.hold_policy_version)),
    ]);
    if let (Some(expected), V::Object(fields)) = (&q.expected_quota_digest, &mut query) {
        fields.insert("expected_quota_digest".into(), hex(expected));
    }
    query
}
/// Commercial request identity without command metadata or tracing. The query's
/// `expected_quota_digest` is hashed only when it is `Some` (D-548).
#[must_use]
pub fn request_digest(query: &NewSaleQuery) -> Digest {
    hash_document("pricing.request.v1", sale_query(query))
}
/// Exact accepted query and binding content, excluding receipt identity and server timestamps.
/// A frozen quota snapshot adds its `quota_snapshot_digest` (D-548); `None` hashes the same bytes
/// as a quota-free acceptance always did.
#[must_use]
pub fn terms_digest(
    query: &NewSaleQuery,
    bindings: &[AcceptedBinding],
    quota: Option<&AcceptedQuotaSnapshot>,
) -> Digest {
    let mut bindings: Vec<_> = bindings.iter().collect();
    bindings.sort_by(|a, b| {
        compare_cells(
            (a.item_id, a.dimension_value.as_deref()),
            (b.item_id, b.dimension_value.as_deref()),
        )
    });
    let mut payload = object([
        ("query", sale_query(query)),
        (
            "bindings",
            V::Array(bindings.into_iter().map(binding).collect()),
        ),
    ]);
    if let (Some(quota), V::Object(fields)) = (quota, &mut payload) {
        fields.insert(
            "quota_snapshot_digest".into(),
            hex(&quota.quota_snapshot_digest),
        );
    }
    hash_document("pricing.terms.v1", payload)
}

/// Exact hold command identity; caller and key belong to the authenticated command scope.
#[must_use]
pub fn fulfilment_digest(q: &crate::acceptance::FulfilmentQuery) -> Digest {
    hash_document(
        "pricing.fulfilment-request.v1",
        object([
            ("tenant_axes", tenant_axes(&q.tenant_axes)),
            ("acceptance_id", text(q.acceptance.acceptance_id)),
            ("terms_digest", hex(&q.acceptance.terms_digest)),
            (
                "current_market",
                object([
                    ("currency", text(&q.current_market.currency)),
                    ("region", optional(q.current_market.region.as_ref(), text)),
                ]),
            ),
            ("activation_at", instant(q.activation_at)),
        ]),
    )
}

/// One rule's payload, shared by the set, selected and snapshot digests: its rule id, its
/// definition reference, item, dimension value, binding, subject token, scope selection (an
/// object, so its keys are sorted), amount, unit and basis token.
pub(crate) fn quota_rule(rule: &PlanQuotaRuleV1) -> V {
    object([
        ("rule_id", text(rule.rule_id)),
        (
            "definition_ref",
            object([
                ("id", text(rule.definition_ref.id)),
                ("version", text(rule.definition_ref.version)),
                ("digest", hex(&rule.definition_ref.digest)),
            ]),
        ),
        ("item_id", text(rule.item_id)),
        (
            "dimension_value",
            optional(rule.dimension_value.as_ref(), text),
        ),
        ("logical_binding_key", text(&rule.logical_binding_key)),
        ("subject_selector", text(rule.subject_selector.token())),
        (
            "scope_selection",
            V::Object(
                rule.scope_selection
                    .iter()
                    .map(|(key, value)| (key.clone(), V::String(value.clone())))
                    .collect(),
            ),
        ),
        ("amount", text(&rule.amount)),
        ("unit_ref", text(&rule.unit_ref)),
        ("basis", text(rule.basis.token())),
    ])
}
/// A revision's whole rule collection (D-541): the rules ordered by `rule_id`, each with its
/// definition reference, item, dimension value, binding, subject, scope selection (sorted),
/// amount, unit and basis token; domain `pricing.quota-rules.v1`. Input order does not matter:
/// rules that share a `rule_id`, which a stored set never holds, are ordered by their canonical
/// bytes. The empty set has its own digest.
#[must_use]
pub fn quota_rules_digest(rules: &[PlanQuotaRuleV1]) -> Digest {
    let mut ordered: Vec<(uuid::Uuid, Vec<u8>, V)> = rules
        .iter()
        .map(|rule| {
            let payload = quota_rule(rule);
            (rule.rule_id, canonical_json_bytes(&payload), payload)
        })
        .collect();
    ordered.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    hash_document(
        "pricing.quota-rules.v1",
        object([(
            "rules",
            V::Array(ordered.into_iter().map(|(_, _, payload)| payload).collect()),
        )]),
    )
}

fn owner_ref(reference: &QuotaOwnerRef) -> V {
    object([
        ("id", text(reference.id)),
        ("version", text(reference.version)),
        ("digest", hex(&reference.digest)),
    ])
}
/// A rule's pinned evidence (D-547): every member, digests as lowercase hex, versions as decimal
/// text, the quota kind as its token, the unit factor as its two integers, and the unit's grid
/// and maximum as canonical decimal integers (`unit_min`, `unit_step`, and `unit_max`, null when
/// the unit has none). No text, locale or availability.
pub(crate) fn quota_evidence(e: &PinnedQuotaEvidence) -> V {
    object([
        ("sku_id", text(e.sku_id)),
        ("sku_version", text(e.sku_version)),
        ("metric_ref", text(&e.metric_ref)),
        ("metric_digest", hex(&e.metric_digest)),
        ("quota_kind", text(e.quota_kind.token())),
        ("canonical_unit_ref", text(&e.canonical_unit_ref)),
        ("unit_digest", hex(&e.unit_digest)),
        ("unit_numerator", text(&e.unit_numerator)),
        ("unit_denominator", text(&e.unit_denominator)),
        ("unit_min", text(&e.unit_min)),
        ("unit_step", text(&e.unit_step)),
        ("unit_max", optional(e.unit_max.as_ref(), text)),
        ("mapping", owner_ref(&e.mapping)),
        ("profile", owner_ref(&e.profile)),
        ("accounting_policy_ref", text(&e.accounting_policy_ref)),
        ("accounting_policy_digest", hex(&e.accounting_policy_digest)),
        ("scope_policy_ref", text(&e.scope_policy_ref)),
        ("scope_policy_digest", hex(&e.scope_policy_digest)),
        ("initial_set_supported", V::Bool(e.initial_set_supported)),
        ("increase_supported", V::Bool(e.increase_supported)),
    ])
}
/// One selected rule's payload: `{rule, evidence}`, the rule as [`quota_rule`] projects it.
pub(crate) fn selected_quota_rule(rule: &ResolvedQuotaRule) -> V {
    object([
        ("rule", quota_rule(&rule.rule)),
        ("evidence", quota_evidence(&rule.evidence)),
    ])
}
/// A rule as the selection and the coverage read it (D-547): its item, its dimension value
/// (`None`: the item's default rule) and its quota. A quota is a metric (its definition's
/// `metric_ref`); the rule's logical binding plays no part, so two rules of one metric on two
/// bindings are rules of one quota.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverageRule<'a> {
    /// The rule's item.
    pub item_id: uuid::Uuid,
    /// The rule's dimension value; `None` for an item's default rule (or an item without a
    /// dimension).
    pub dimension_value: Option<&'a str>,
    /// The rule's quota: its definition's metric.
    pub metric_ref: &'a str,
}
impl<'a> From<&'a ResolvedQuotaRule> for CoverageRule<'a> {
    fn from(rule: &'a ResolvedQuotaRule) -> Self {
        Self {
            item_id: rule.rule.item_id,
            dimension_value: rule.rule.dimension_value.as_deref(),
            metric_ref: rule.evidence.metric_ref.as_str(),
        }
    }
}
/// Whether `rules` hold a rule of `item` for `metric` at exactly `value` (`None`: the item's
/// default rule).
fn has_rule<'a>(
    mut rules: impl Iterator<Item = CoverageRule<'a>>,
    item: uuid::Uuid,
    metric: &str,
    value: Option<&str>,
) -> bool {
    rules.any(|r| r.item_id == item && r.metric_ref == metric && r.dimension_value == value)
}
/// Whether `selections` select `rule` among `rules` (D-547): a selection names its item, and
/// `rule` is a rule of the item for exactly the requested value, or a default rule of the item
/// (no dimension value) for a value that has no rule of `rule`'s metric in `rules`.
fn selects<'a>(
    rules: &(impl Iterator<Item = CoverageRule<'a>> + Clone),
    selections: &[BindingSelection],
    rule: CoverageRule<'_>,
) -> bool {
    selections.iter().any(|s| {
        s.item_id == rule.item_id
            && (s.dimension_value.as_deref() == rule.dimension_value
                || (rule.dimension_value.is_none()
                    && !has_rule(
                        rules.clone(),
                        s.item_id,
                        rule.metric_ref,
                        s.dimension_value.as_deref(),
                    )))
    })
}
/// The rules of `terms` that `selections` select (D-547), ordered by `rule_id` (rules that share
/// one keep their order in `terms`). A quota is a metric. For each selection and each metric that
/// the item's rules carry, the selection takes every rule of the item for exactly the requested
/// value and that metric (two bindings of one metric are both taken); if there is none, every
/// default rule of the item (no dimension value) for that metric; else none. A selection with no
/// dimension value takes the default rules. Nothing is inferred beyond an authored default: a
/// default of one metric never stands in for another, a default is never taken beside a rule of
/// its metric for the requested value whatever its binding, a rule never applies to another item,
/// and the price's default chain plays no part. A selection given twice selects its rules once.
/// [`uncovered_quotas`] names the metrics a selection leaves without a rule.
#[must_use]
pub fn select_quota_rules<'a>(
    terms: &'a QuotaTerms,
    selections: &[BindingSelection],
) -> Vec<&'a ResolvedQuotaRule> {
    let all = terms.rules.iter().map(CoverageRule::from);
    let mut selected: Vec<&ResolvedQuotaRule> = terms
        .rules
        .iter()
        .filter(|r| selects(&all, selections, CoverageRule::from(*r)))
        .collect();
    selected.sort_by_key(|r| r.rule.rule_id);
    selected
}
/// The metrics that `selections` leave without a rule among `rules` (D-547): for each selection,
/// each metric that the item's rules carry with neither a rule for the requested value nor a
/// default rule of the item. Each is named once, ordered by item, requested value and metric. An
/// item without rules has no metric to name. [`uncovered_quotas`] applies it to a revision's
/// terms, and Pricing's checks to every sellable cell of a revision's items.
#[must_use]
pub fn uncovered_metrics(
    rules: &[CoverageRule<'_>],
    selections: &[BindingSelection],
) -> Vec<UncoveredQuota> {
    let mut uncovered = BTreeSet::new();
    for s in selections {
        let value = s.dimension_value.as_deref();
        for rule in rules.iter().filter(|r| r.item_id == s.item_id) {
            let metric = rule.metric_ref;
            if !has_rule(rules.iter().copied(), s.item_id, metric, value)
                && !has_rule(rules.iter().copied(), s.item_id, metric, None)
            {
                uncovered.insert(UncoveredQuota {
                    item_id: s.item_id,
                    dimension_value: s.dimension_value.clone(),
                    metric_ref: metric.to_owned(),
                });
            }
        }
    }
    uncovered.into_iter().collect()
}
/// The quotas (metrics) of the selected items that [`select_quota_rules`] leaves without a rule
/// (D-547): [`uncovered_metrics`] over the rules of `terms`, each rule's metric its evidence's
/// `metric_ref`. A metric is uncovered exactly when the selection selects no rule of it. An
/// acceptance refuses a sale that leaves any as `QUOTA_RULE_MISSING`.
#[must_use]
pub fn uncovered_quotas(
    terms: &QuotaTerms,
    selections: &[BindingSelection],
) -> Vec<UncoveredQuota> {
    let rules: Vec<CoverageRule<'_>> = terms.rules.iter().map(CoverageRule::from).collect();
    uncovered_metrics(&rules, selections)
}
/// Hash already selected rules, each with its pinned evidence, ordered by `rule_id`, then by
/// canonical bytes; domain `pricing.selected-quota.v1`.
fn selected_rules_digest<'a>(rules: impl IntoIterator<Item = &'a ResolvedQuotaRule>) -> Digest {
    let mut ordered: Vec<(uuid::Uuid, Vec<u8>, V)> = rules
        .into_iter()
        .map(|rule| {
            let payload = selected_quota_rule(rule);
            (rule.rule.rule_id, canonical_json_bytes(&payload), payload)
        })
        .collect();
    ordered.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    hash_document(
        "pricing.selected-quota.v1",
        object([(
            "rules",
            V::Array(ordered.into_iter().map(|(_, _, payload)| payload).collect()),
        )]),
    )
}
/// Hash the rules [`select_quota_rules`] selects, each with its pinned evidence, ordered by
/// `rule_id` (then by canonical bytes, as [`quota_rules_digest`] orders); domain
/// `pricing.selected-quota.v1`. The evidence includes the quota kind and the rule unit's grid
/// and maximum (`unit_min`, `unit_step`, `unit_max`), so a limit recomputed at another quantity
/// is checked against the frozen grid. No text, locale or availability (D-547). A selection that
/// selects no rule has its own digest.
#[must_use]
pub fn selected_quota_digest(terms: &QuotaTerms, selections: &[BindingSelection]) -> Digest {
    selected_rules_digest(select_quota_rules(terms, selections))
}
/// One accepted rule's final limit and availability revision, by its rule id.
fn accepted_limit(rule: &AcceptedQuotaRule) -> V {
    object([
        ("rule_id", text(rule.rule.rule.rule_id)),
        ("limit", text(&rule.limit)),
        ("availability_revision", text(rule.availability_revision)),
    ])
}
/// The snapshot payload: the selected digest, each accepted rule's final limit and availability
/// revision (`limits`, ordered by `rule_id`, then by canonical bytes), the original quantity
/// (exact text), the tenant axes, the plan revision and the order line (order id, order version,
/// line id).
pub(crate) fn quota_snapshot(
    selected: &Digest,
    rules: &[AcceptedQuotaRule],
    query: &NewSaleQuery,
) -> V {
    let mut limits: Vec<(uuid::Uuid, Vec<u8>, V)> = rules
        .iter()
        .map(|rule| {
            let payload = accepted_limit(rule);
            (
                rule.rule.rule.rule_id,
                canonical_json_bytes(&payload),
                payload,
            )
        })
        .collect();
    limits.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    object([
        ("selected_quota_digest", hex(selected)),
        (
            "limits",
            V::Array(limits.into_iter().map(|(_, _, payload)| payload).collect()),
        ),
        ("original_quantity", decimal(query.quantity)),
        ("tenant_axes", tenant_axes(&query.tenant_axes)),
        ("plan_revision_id", text(query.plan_revision_id)),
        ("order_id", text(query.order_id)),
        ("order_version", text(query.order_version)),
        ("line_id", text(query.line_id)),
    ])
}
/// Bind the selected digest to the accepted limits and the sale (D-548): each accepted rule's
/// final limit and availability revision, ordered by `rule_id`, the original quantity
/// (`query.quantity`, exact), the seller, payer and resource tenants, the plan revision and the
/// order line (order id, order version, line id); domain `pricing.quota-snapshot.v1`.
#[must_use]
pub fn quota_snapshot_digest(
    selected: &Digest,
    rules: &[AcceptedQuotaRule],
    query: &NewSaleQuery,
) -> Digest {
    hash_document(
        "pricing.quota-snapshot.v1",
        quota_snapshot(selected, rules, query),
    )
}
/// A canonical decimal integer (D-541): `0`, or ASCII digits without a leading zero, at most 39
/// of them, that fit in 128 bits; no sign, point, exponent or space.
fn canonical_integer(text: &str) -> bool {
    (1..=39).contains(&text.len())
        && text.bytes().all(|b| b.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'))
        && text.parse::<u128>().is_ok()
}
/// The check of [`verify_quota_snapshot`] that a stored quota snapshot fails (D-548).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum QuotaSnapshotMismatch {
    /// The query has an expected quota digest and no snapshot, or a snapshot and no expected
    /// digest.
    #[error("the quota snapshot and the expected quota digest are not both present or both absent")]
    Pairing,
    /// The query's selections do not select a stored rule, or the stored rules are not in strict
    /// `rule_id` order.
    #[error(
        "the sale's selections do not select a rule of the quota snapshot, or its rules are not in \
         strict rule_id order"
    )]
    Selection,
    /// A stored final limit is not a canonical decimal integer (D-541).
    #[error("a final limit of the quota snapshot is not a canonical decimal integer")]
    Limit,
    /// The selected digest recomputed from the stored rules is not the stored selected digest or
    /// the query's expected digest.
    #[error(
        "the selected quota digest recomputed from the snapshot's rules is not the stored or the \
         expected one"
    )]
    SelectedDigest,
    /// The original quantity is not the query's quantity.
    #[error("the quota snapshot's original quantity is not the sale's quantity")]
    Quantity,
    /// The snapshot digest recomputed from the selected digest, the stored limits and the query
    /// is not the stored one.
    #[error(
        "the quota snapshot digest recomputed from the snapshot and the sale is not the stored one"
    )]
    SnapshotDigest,
}
/// Check a stored quota snapshot against the query it was accepted for (D-548), the same way in
/// Pricing and in every copy of a receipt. It holds when the query has no expected quota digest
/// and there is no snapshot. Otherwise there must be both, and, in this order:
/// - the query's selections select every stored rule by [`select_quota_rules`]'s rule applied to
///   the stored rules (an item's default rule only where no stored rule of its metric names the
///   requested value, whatever its binding), and the rules are in strict `rule_id` order; which
///   rules the revision's whole terms select is bound by the expected digest;
/// - each stored limit is a canonical decimal integer (D-541);
/// - the selected digest recomputed from the stored rules equals the stored selected digest and
///   the query's expected digest;
/// - the original quantity equals the query's quantity;
/// - the snapshot digest recomputed from that selected digest, the stored limits and the query
///   equals the stored one.
///
/// The terms digest is a separate check: it binds the stored snapshot digest to the query and the
/// bindings ([`terms_digest`]).
/// # Errors
/// The first check above that fails, as its [`QuotaSnapshotMismatch`]: `Pairing`, `Selection`,
/// `Limit`, `SelectedDigest`, `Quantity`, then `SnapshotDigest`.
pub fn verify_quota_snapshot(
    query: &NewSaleQuery,
    snapshot: Option<&AcceptedQuotaSnapshot>,
) -> Result<(), QuotaSnapshotMismatch> {
    let (expected, snapshot) = match (&query.expected_quota_digest, snapshot) {
        (None, None) => return Ok(()),
        (Some(expected), Some(snapshot)) => (expected, snapshot),
        _ => return Err(QuotaSnapshotMismatch::Pairing),
    };
    let rules = &snapshot.rules;
    let stored = rules.iter().map(|r| CoverageRule::from(&r.rule));
    let selected =
        |r: &AcceptedQuotaRule| selects(&stored, &query.selections, CoverageRule::from(&r.rule));
    if !rules.iter().all(selected)
        || !rules
            .windows(2)
            .all(|pair| pair[0].rule.rule.rule_id < pair[1].rule.rule.rule_id)
    {
        return Err(QuotaSnapshotMismatch::Selection);
    }
    if !rules.iter().all(|r| canonical_integer(&r.limit)) {
        return Err(QuotaSnapshotMismatch::Limit);
    }
    let selected = selected_rules_digest(rules.iter().map(|r| &r.rule));
    if selected != snapshot.selected_quota_digest || selected != *expected {
        return Err(QuotaSnapshotMismatch::SelectedDigest);
    }
    if snapshot.original_quantity != query.quantity {
        return Err(QuotaSnapshotMismatch::Quantity);
    }
    if quota_snapshot_digest(&selected, rules, query) != snapshot.quota_snapshot_digest {
        return Err(QuotaSnapshotMismatch::SnapshotDigest);
    }
    Ok(())
}
