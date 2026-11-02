//! `FakeSubscriptionsBilling` (DESIGN 11 §4.12; feature 01 §5.4): Subscriptions' commercial facts
//! in Rating's reading of the `subscriptions-rating/1` profile of Subscriptions PR #5516 (DESIGN 11
//! §4.3; T-D-87). Subscriptions has no SDK yet, so a test builds each fact version here and hands
//! its payload to fact intake as an event consumer or a recovery read would.
//!
//! Every binding is built with digests Pricing would compute (`money_digest`, `template_digest`,
//! the policy digest), so the profile's checks pass unless a test breaks one on purpose.

use bss_pricing_sdk::digest::{money_digest, policy_digest, template_digest};
use bss_rating_core::engine::current::exact::CoreCurrency;
use bss_rating_core::engine::current::input::{
    BillingTiming, ChargeKind, CoreBinding, CoreInvoiceInputs, CorePrice, CorePriceModel,
    CoreUsagePolicy, InputSource, MeterRef, PartialWindowPolicy, PolicyScope, PriceState,
    RatingWindowKind, RecurringPeriod, ResetPolicy, StoredRounding, WindowFold,
};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use time::Date;
use uuid::Uuid;

use crate::domain::fact::profile::PROFILE;
use crate::domain::pricing::convert::{digest_hex, to_pricing};

/// The placeholder of a digest a builder computes afterwards.
const ZERO_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// The parties and identities a subscription's facts share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeSubscriptionsBilling {
    /// The seller tenant: the facts' tenant and the catalog tenant.
    pub seller: Uuid,
    /// The payer tenant.
    pub payer: Uuid,
    /// The resource tenant.
    pub resource: Uuid,
    /// The subscription.
    pub subscription_id: Uuid,
    /// The billing group.
    pub billing_group_id: Uuid,
    /// The billing currency.
    pub currency: String,
}

impl FakeSubscriptionsBilling {
    /// A subscription of `seller`, billed in EUR.
    #[must_use]
    pub fn new(seller: Uuid) -> Self {
        Self {
            seller,
            payer: Uuid::new_v4(),
            resource: Uuid::new_v4(),
            subscription_id: Uuid::new_v4(),
            billing_group_id: Uuid::new_v4(),
            currency: "EUR".to_owned(),
        }
    }

    /// A version-1 fact of `kind` over `[period_start, period_end)`, served whole, with no slice:
    /// the caller adds slices and terms.
    #[must_use]
    pub fn fact(
        &self,
        kind: &str,
        item_id: Uuid,
        period_start: &str,
        period_end: &str,
    ) -> FactSpec {
        FactSpec {
            party: self.clone(),
            fact_id: Uuid::new_v4(),
            fact_version: 1,
            predecessor_version: None,
            kind: kind.to_owned(),
            obligation: "active".to_owned(),
            timing: "advance".to_owned(),
            sub_line_key: "plan#1".to_owned(),
            item_id,
            period: (period_start.to_owned(), period_end.to_owned()),
            served: (period_start.to_owned(), period_end.to_owned()),
            anchor_at: period_start.to_owned(),
            slices: Vec::new(),
            terms: Vec::new(),
        }
    }
}

/// One slice of a fact under construction.
#[derive(Debug, Clone, PartialEq)]
pub struct SliceSpec {
    /// `[from, to)` as RFC 3339 text.
    pub from: String,
    /// The slice's end.
    pub to: String,
    /// `chargeable`, `nonchargeable` or `unresolved`.
    pub coverage: String,
    /// The reason of a non-chargeable or unresolved slice.
    pub reason: Option<String>,
    /// The terms the slice names.
    pub terms: Vec<String>,
    /// The lots, as JSON.
    pub lots: Vec<Value>,
}

/// One accepted term of a fact under construction.
#[derive(Debug, Clone, PartialEq)]
pub struct TermSpec {
    /// Its reference.
    pub terms_ref: String,
    /// The plan.
    pub plan_id: Uuid,
    /// The plan revision.
    pub revision_id: Uuid,
    /// The binding.
    pub binding: CoreBinding,
}

/// A fact version under construction.
#[derive(Debug, Clone, PartialEq)]
pub struct FactSpec {
    /// The subscription's parties.
    pub party: FakeSubscriptionsBilling,
    /// The fact.
    pub fact_id: Uuid,
    /// Its version.
    pub fact_version: i64,
    /// The version it replaces.
    pub predecessor_version: Option<i64>,
    /// `recurring`, `usage` or `one_time`.
    pub kind: String,
    /// `active` or `zeroed`.
    pub obligation: String,
    /// `advance` or `arrears`.
    pub timing: String,
    /// The component interval alias.
    pub sub_line_key: String,
    /// The `PriceBook` item.
    pub item_id: Uuid,
    /// The billing period.
    pub period: (String, String),
    /// The served extent.
    pub served: (String, String),
    /// `BillingTerms.anchor_at`.
    pub anchor_at: String,
    /// The slices.
    pub slices: Vec<SliceSpec>,
    /// The accepted terms.
    pub terms: Vec<TermSpec>,
}

impl FactSpec {
    /// Adds a term and a chargeable slice over `[from, to)` naming it.
    #[must_use]
    pub fn chargeable(mut self, from: &str, to: &str, term: TermSpec, lots: Vec<Value>) -> Self {
        let terms_ref = term.terms_ref.clone();
        if !self.terms.iter().any(|t| t.terms_ref == terms_ref) {
            self.terms.push(term);
        }
        self.slices.push(SliceSpec {
            from: from.to_owned(),
            to: to.to_owned(),
            coverage: "chargeable".to_owned(),
            reason: None,
            terms: vec![terms_ref],
            lots,
        });
        self
    }

    /// Adds an unresolved slice over `[from, to)`.
    #[must_use]
    pub fn unresolved(mut self, from: &str, to: &str, reason: &str) -> Self {
        self.slices.push(SliceSpec {
            from: from.to_owned(),
            to: to.to_owned(),
            coverage: "unresolved".to_owned(),
            reason: Some(reason.to_owned()),
            terms: Vec::new(),
            lots: Vec::new(),
        });
        self
    }

    /// Adds a nonchargeable slice over `[from, to)`.
    #[must_use]
    pub fn nonchargeable(mut self, from: &str, to: &str, reason: &str) -> Self {
        self.slices.push(SliceSpec {
            from: from.to_owned(),
            to: to.to_owned(),
            coverage: "nonchargeable".to_owned(),
            reason: Some(reason.to_owned()),
            terms: Vec::new(),
            lots: Vec::new(),
        });
        self
    }

    /// The next version of the same fact: version + 1, with the same content.
    #[must_use]
    pub fn next_version(&self) -> Self {
        let mut next = self.clone();
        next.predecessor_version = Some(self.fact_version);
        next.fact_version = self.fact_version.saturating_add(1);
        next
    }

    /// The payload Subscriptions would publish. Its `payload_digest` is a digest of the payload
    /// without it, standing in for the producer's codec (S-U13).
    #[must_use]
    pub fn payload(&self) -> Value {
        let mut payload = json!({
            "profile": PROFILE,
            "fact_id": self.fact_id,
            "fact_version": self.fact_version,
            "predecessor_version": self.predecessor_version,
            "billing_group": {
                "billing_group_id": self.party.billing_group_id,
                "subscription_id": self.party.subscription_id,
                "seller_tenant_id": self.party.seller,
                "payer_tenant_id": self.party.payer,
                "currency": self.party.currency,
            },
            "resource_tenant_id": self.party.resource,
            "sub_line_key": self.sub_line_key,
            "item_id": self.item_id,
            "kind": self.kind,
            "obligation": self.obligation,
            "timing": self.timing,
            "period": {"start": self.period.0, "end": self.period.1},
            "served": {"start": self.served.0, "end": self.served.1},
            "billing_terms": {
                "schema_version": 1,
                "cycle": "month",
                "anchor": "calendar",
                "anchor_at": self.anchor_at,
                "timezone": "UTC",
                "source": "explicit_order",
            },
            "slices": self.slices.iter().map(|slice| json!({
                "from": slice.from,
                "to": slice.to,
                "coverage": slice.coverage,
                "reason": slice.reason,
                "terms": slice.terms,
                "lots": slice.lots,
            })).collect::<Vec<_>>(),
            "terms": self.terms.iter().map(|term| json!({
                "terms_ref": term.terms_ref,
                "catalog_tenant_id": self.party.seller,
                "plan_id": term.plan_id,
                "revision_id": term.revision_id,
                "binding": term.binding,
            })).collect::<Vec<_>>(),
        });
        let digest = crate::domain::digest::Sha256Hex::of_json(&payload);
        if let Some(object) = payload.as_object_mut() {
            object.insert("payload_digest".to_owned(), json!(digest.as_str()));
        }
        payload
    }
}

/// A lot of `quantity` units priced by `terms_ref`.
#[must_use]
pub fn lot(lot_id: &str, quantity: &str, terms_ref: &str) -> Value {
    json!({
        "lot_id": lot_id,
        "quantity": quantity,
        "unit": "seat",
        "terms_ref": terms_ref,
        "purchase_origin": format!("order:{lot_id}"),
    })
}

/// A term `terms_ref` of a fresh plan revision with `binding`.
#[must_use]
pub fn term(terms_ref: &str, binding: CoreBinding) -> TermSpec {
    TermSpec {
        terms_ref: terms_ref.to_owned(),
        plan_id: Uuid::new_v4(),
        revision_id: Uuid::new_v4(),
        binding,
    }
}

/// A usage binding of `item_id` to `price_id`, rated per `window` on `meter`
/// (`products.derived/<code>@<n>`), with its digests as Pricing computes them.
///
/// # Panics
///
/// If a digest of the built binding cannot be computed, which its fields rule out.
#[expect(clippy::expect_used, reason = "a test builder of known-good bindings")]
#[must_use]
pub fn usage_binding(
    item_id: Uuid,
    price_id: Uuid,
    window: RatingWindowKind,
    meter: &str,
    ends_on: Option<Date>,
) -> CoreBinding {
    let policy = CoreUsagePolicy {
        policy_id: Uuid::new_v4(),
        version: 1,
        digest: ZERO_DIGEST.to_owned(),
        rating_window: window,
        aggregation_scope: PolicyScope::SubscriptionLine,
        reset: ResetPolicy::RatingWindowStart,
        partial_window: PartialWindowPolicy::ActualQuantityFullThresholds,
        fold: WindowFold::Sum,
    };
    let version = meter.rsplit('@').next().unwrap_or("1").to_owned();
    let mut binding = base_binding(
        item_id,
        price_id,
        ChargeKind::Usage,
        CorePriceModel::PerUnit {
            unit_amount: Decimal::new(2, 2),
        },
        ends_on,
    );
    binding.unit = Some("cloudlet-hour".to_owned());
    binding.meter = Some(MeterRef {
        usage_type_id: meter.to_owned(),
        version,
    });
    binding.usage_policy = Some(policy);
    binding.invoice.timing = BillingTiming::Arrears;
    seal(binding).expect("a valid usage binding")
}

/// A recurring binding of `item_id` to `price_id` at `unit_amount` per seat and month.
///
/// # Panics
///
/// As [`usage_binding`].
#[expect(clippy::expect_used, reason = "a test builder of known-good bindings")]
#[must_use]
pub fn recurring_binding(
    item_id: Uuid,
    price_id: Uuid,
    unit_amount: Decimal,
    timing: BillingTiming,
) -> CoreBinding {
    let mut binding = base_binding(
        item_id,
        price_id,
        ChargeKind::Recurring,
        CorePriceModel::PerUnit { unit_amount },
        None,
    );
    binding.recurring_period = Some(RecurringPeriod::Month);
    binding.invoice.timing = timing;
    seal(binding).expect("a valid recurring binding")
}

fn base_binding(
    item_id: Uuid,
    price_id: Uuid,
    kind: ChargeKind,
    model: CorePriceModel,
    ends_on: Option<Date>,
) -> CoreBinding {
    let entry = Uuid::new_v4();
    CoreBinding {
        item_id,
        price_book_entry_id: entry,
        dimension_key: None,
        dimension_value: None,
        sku_id: Uuid::new_v4(),
        sku_version: 1,
        sku_code: "SKU-TEST".to_owned(),
        sku_name: "A test SKU".to_owned(),
        unit: Some("seat".to_owned()),
        meter: None,
        price: CorePrice {
            price_id,
            price_book_entry_id: entry,
            money_digest: ZERO_DIGEST.to_owned(),
            currency: CoreCurrency {
                code: "EUR".to_owned(),
                scale: 2,
            },
            model,
            minimum_fee: None,
            effective_from: Date::from_ordinal_date(2026, 1).unwrap_or(Date::MIN),
            ends_on,
            state: PriceState::Approved,
        },
        kind,
        recurring_period: None,
        via_default: false,
        usage_policy: None,
        invoice: CoreInvoiceInputs {
            template: "{sku_name}".to_owned(),
            template_digest: ZERO_DIGEST.to_owned(),
            template_source: InputSource::Entry,
            gl_code: "4000".to_owned(),
            tax_category: "standard".to_owned(),
            timing: BillingTiming::Advance,
            rounding: StoredRounding::HalfEven,
        },
    }
}

/// Fills in the digests Pricing would compute for `binding`.
///
/// # Errors
///
/// The conversion's error for a field that does not convert.
pub fn seal(mut binding: CoreBinding) -> Result<CoreBinding, String> {
    binding.invoice.template_digest = digest_hex(&template_digest(&binding.invoice.template));
    let pricing = to_pricing(&binding).map_err(|e| e.to_string())?;
    binding.price.money_digest = digest_hex(&money_digest(&pricing.price));
    if let (Some(policy), Some(input)) = (
        binding.usage_policy.as_mut(),
        pricing.usage_rating_policy.as_ref(),
    ) {
        policy.digest = digest_hex(&policy_digest(&input.content));
    }
    Ok(binding)
}
