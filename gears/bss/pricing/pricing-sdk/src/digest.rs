//! Versioned C00 canonical JSON projections. Public contracts never expose untyped JSON.
use crate::{
    Digest,
    read::{
        AcceptedBinding, BindingSelection, ChargeKind, ImmutablePrice, PriceModel,
        ResolvedBindings, Tier,
    },
    terms::{
        AggregationScope, BillingCycle, BillingTiming, Fold, InputSource, InvoiceInputs,
        PartialWindow, RatingWindow, Reset, Rounding, Timezone, UsageRatingPolicy,
        UsageRatingPolicyInput,
    },
};
use aws_lc_rs::digest::{SHA256, digest as sha256};
use rust_decimal::Decimal;
use std::collections::{BTreeMap, BTreeSet};
use toolkit_canonical_errors::CanonicalError;

#[toolkit_canonical_errors::resource_error("gts.cf.bss.pricing.plan.v1~")]
struct BindingResource;

/// Restricted JSON: numeric meaning is always exact text before serialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CanonicalValue {
    Null,
    Bool(bool),
    String(String),
    Array(Vec<Self>),
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
pub(crate) fn canonical_json_bytes(value: &V) -> Vec<u8> {
    let mut out = String::new();
    encode(value, &mut out);
    out.into_bytes()
}
pub(crate) fn hash_document(domain: &str, payload: V) -> Digest {
    let bytes = canonical_json_bytes(&object([("domain", text(domain)), ("payload", payload)]));
    let hashed = sha256(&SHA256, &bytes);
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
/// Financial content only: no identity, dates, closing observations or self digest.
#[must_use]
pub fn money_digest(price: &ImmutablePrice) -> Digest {
    hash_document(
        "pricing.money.v1",
        object([
            ("currency", text(&price.currency)),
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
        ("currency", text(&p.currency)),
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
        ("currency_scale", text(i.currency_scale)),
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
fn sale_query(q: &crate::acceptance::NewSaleQuery) -> V {
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
    object([
        (
            "tenant_axes",
            object([
                ("seller_tenant_id", text(q.tenant_axes.seller_tenant_id)),
                ("payer_tenant_id", text(q.tenant_axes.payer_tenant_id)),
                ("resource_tenant_id", text(q.tenant_axes.resource_tenant_id)),
            ]),
        ),
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
    ])
}
/// Commercial request identity without command metadata or tracing.
#[must_use]
pub fn request_digest(query: &crate::acceptance::NewSaleQuery) -> Digest {
    hash_document("pricing.request.v1", sale_query(query))
}
/// Exact accepted query and binding content, excluding receipt identity and server timestamps.
#[must_use]
pub fn terms_digest(
    query: &crate::acceptance::NewSaleQuery,
    bindings: &[AcceptedBinding],
) -> Digest {
    let mut bindings: Vec<_> = bindings.iter().collect();
    bindings.sort_by(|a, b| {
        compare_cells(
            (a.item_id, a.dimension_value.as_deref()),
            (b.item_id, b.dimension_value.as_deref()),
        )
    });
    hash_document(
        "pricing.terms.v1",
        object([
            ("query", sale_query(query)),
            (
                "bindings",
                V::Array(bindings.into_iter().map(binding).collect()),
            ),
        ]),
    )
}

/// Exact hold command identity; caller and key belong to the authenticated command scope.
#[must_use]
pub fn fulfilment_digest(q: &crate::acceptance::FulfilmentQuery) -> Digest {
    hash_document(
        "pricing.fulfilment-request.v1",
        object([
            (
                "tenant_axes",
                object([
                    ("seller_tenant_id", text(q.tenant_axes.seller_tenant_id)),
                    ("payer_tenant_id", text(q.tenant_axes.payer_tenant_id)),
                    ("resource_tenant_id", text(q.tenant_axes.resource_tenant_id)),
                ]),
            ),
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
