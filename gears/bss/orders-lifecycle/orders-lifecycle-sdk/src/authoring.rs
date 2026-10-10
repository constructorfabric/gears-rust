//! Draft authoring contracts (02 Capture; DESIGN 02 §3.3, §4.3; `models.json` `CreateOrder`,
//! `AddLine`, `HeaderPatch`, `LinePatch`, `OrderView`).
//!
//! These are the normalized SDK call shapes. REST uses the same `snake_case` names: a PATCH body is
//! `{ "fields": { ... } }` and a draft write may carry `expected_draft_revision` beside it, which
//! the adapter extracts into call metadata. Every value here is authored content; actor identity,
//! timestamps, line identities and the order number come from trusted context or the server.
//! Nothing here resolves a catalog reference, a price or a contract (02 §4.1).
use crate::commercial::scalars;
use crate::models::{DelegationProofRef, DraftRevision, IdempotencyKey, OrderVersion};
use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// Invalid authored value: always the boundary `request-invalid` refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid authored value: {0}")]
pub struct AuthoringInvalid(pub &'static str);

/// Line quantity per selected item bound (`models.json` `selected_bindings_per_line`).
pub const MAX_SELECTED_ITEMS: usize = 1_000;
/// Administrative text caps in Unicode scalar values (S1-02 selection; Pricing caps precedent).
pub const EXTERNAL_REFERENCE_MAX: usize = 256;
pub const DISPLAY_LABEL_MAX: usize = 200;
pub const INTERNAL_NOTES_MAX: usize = 2_000;
/// Opaque selected dimension value bound.
pub const DIMENSION_VALUE_MAX: usize = 256;

/// Registered order categories (`gts.cf.bss.orders.category.v1~`). Registration is not admission:
/// `change` is refused `category-not-admitted` by the capture guard until its path ships (D-176).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    NewSale,
    Change,
}
impl Category {
    pub const NEW_SALE: &'static str = "gts.cf.bss.orders.category.v1~cf.bss.orders.new_sale.v1";
    pub const CHANGE: &'static str = "gts.cf.bss.orders.category.v1~cf.bss.orders.change.v1";
    /// The registered GTS identity stored on the aggregate.
    #[must_use]
    pub const fn gts_id(self) -> &'static str {
        match self {
            Self::NewSale => Self::NEW_SALE,
            Self::Change => Self::CHANGE,
        }
    }
    /// A registered category; `None` for an unregistered value (boundary-invalid).
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            Self::NEW_SALE => Some(Self::NewSale),
            Self::CHANGE => Some(Self::Change),
            _ => None,
        }
    }
    /// Whether this phase admits the category (02 §2.2: `new_sale` only).
    #[must_use]
    pub const fn is_admitted(self) -> bool {
        matches!(self, Self::NewSale)
    }
}
impl Serialize for Category {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.gts_id())
    }
}
impl<'de> Deserialize<'de> for Category {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Self::parse(&text).ok_or_else(|| serde::de::Error::custom("unregistered order category"))
    }
}

/// ISO 4217-shaped line currency (three upper-case ASCII letters). Opaque in draft: the gate, not
/// capture, decides whether the market prices in it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Currency(String);
impl Currency {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for Currency {
    type Error = AuthoringInvalid;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() == 3 && value.bytes().all(|b| b.is_ascii_uppercase()) {
            Ok(Self(value))
        } else {
            Err(AuthoringInvalid("currency"))
        }
    }
}
impl From<Currency> for String {
    fn from(value: Currency) -> Self {
        value.0
    }
}

/// The billing cycle the price is quoted against (`PriceBook`'s two periods, D-167).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BillingCycle {
    Month,
    Year,
}
impl BillingCycle {
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Month => "month",
            Self::Year => "year",
        }
    }
}

/// Authored term intent (D-193): explicit rolling, a whole number of invoice periods, or a finite
/// calendar interval. Absence is a separate *missing* intent, never rolling. Calendar units are
/// preserved; nothing converts months to days.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthoredTerm {
    Rolling,
    Periods {
        count: u32,
    },
    Calendar {
        years: u32,
        months: u32,
        days: u32,
        microseconds: u64,
    },
}
impl AuthoredTerm {
    /// Structural validity: a positive period count; a positive, representable calendar interval.
    ///
    /// # Errors
    /// `request-invalid` input.
    pub fn validate(&self) -> Result<(), AuthoringInvalid> {
        let invalid = Err(AuthoringInvalid("term_duration"));
        match *self {
            Self::Rolling => Ok(()),
            // Representable as a whole-month interval under either cycle: the cycle may be
            // authored or changed later, and the stored interval follows it.
            Self::Periods { count } => {
                if count == 0 || i32::try_from(u64::from(count) * 12).is_err() {
                    return invalid;
                }
                Ok(())
            }
            Self::Calendar {
                years,
                months,
                days,
                microseconds,
            } => {
                let months_total = i64::from(years) * 12 + i64::from(months);
                if i32::try_from(months_total).is_err()
                    || i32::try_from(days).is_err()
                    || i64::try_from(microseconds).is_err()
                    || (months_total == 0 && days == 0 && microseconds == 0)
                {
                    return invalid;
                }
                Ok(())
            }
        }
    }
}

/// A positive exact decimal item quantity, carried as its canonical decimal string (no binary
/// floating point; 03 §4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Quantity(#[serde(with = "scalars::decimal")] Decimal);
impl<'de> Deserialize<'de> for Quantity {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = scalars::decimal::deserialize(d)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}
impl Quantity {
    /// # Errors
    /// Zero or negative quantity (02 §3.3: positive quantity).
    pub fn new(value: Decimal) -> Result<Self, AuthoringInvalid> {
        if value > Decimal::ZERO {
            Ok(Self(value))
        } else {
            Err(AuthoringInvalid("quantity"))
        }
    }
    #[must_use]
    pub fn value(self) -> Decimal {
        self.0
    }
}

/// One selected plan item (03 §4.3): item identity, positive exact quantity where applicable and
/// the customer's dimension choice. Drafts may carry unresolved opaque IDs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedItem {
    pub item_id: Uuid,
    #[serde(default)]
    pub quantity: Option<Quantity>,
    #[serde(default)]
    pub selected_dim_value: Option<String>,
}

/// A calendar date `YYYY-MM-DD` (authored line dates; never a timestamp).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CalendarDate(#[serde(with = "scalars::date")] pub time::Date);

/// Create an empty draft (02 §2.1; `models.json` `CreateOrder`). No caller IDs, lines, pin or
/// total; the actor and sales path come from trusted context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateOrder {
    pub resource_tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub seller_tenant_id: Uuid,
    pub category: Category,
    #[serde(default)]
    pub contract_id: Option<Uuid>,
}

/// Trusted adapter metadata of a create: no target, so no expected version (D-105).
#[derive(Debug, Clone)]
pub struct CreateMeta {
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: Option<Uuid>,
    pub delegation_proof_ref: Option<DelegationProofRef>,
}

/// Add one working-set line (02 §2.2; `models.json` `AddLine`). Dates, term and cycle may remain
/// absent in draft and are retained exactly as authored. No external reference: that is an
/// administrative line `PATCH` (D-118).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddLine {
    pub plan_id: Uuid,
    pub plan_revision_id: Uuid,
    pub selected_items: Vec<SelectedItem>,
    pub currency: Currency,
    #[serde(default)]
    pub contract_effective_date: Option<CalendarDate>,
    #[serde(default)]
    pub service_activation_date: Option<CalendarDate>,
    #[serde(default)]
    pub acceptance_due_date: Option<CalendarDate>,
    #[serde(default)]
    pub term_duration: Option<AuthoredTerm>,
    #[serde(default)]
    pub billing_cycle: Option<BillingCycle>,
}

/// A present-but-null value is a named field that clears; omission does not name it.
#[allow(clippy::option_option)] // Omitted, cleared and replaced are distinct.
fn named_nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}
/// A present value of a non-nullable field; `null` is invalid rather than "not named".
fn named<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

/// Header `PATCH` fields (`models.json` `HeaderPatch.fields`). Presence names a field; the named
/// classes alone select the trigger (02 §3.1, D-145). `seller_tenant_id` is accepted only so the
/// fixed-seller guard can refuse it `tenant-axis-immutable` (D-119).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::option_option)] // Omitted, cleared and replaced are distinct.
pub struct HeaderPatch {
    #[serde(
        default,
        deserialize_with = "named",
        skip_serializing_if = "Option::is_none"
    )]
    pub category: Option<Category>,
    #[serde(
        default,
        deserialize_with = "named",
        skip_serializing_if = "Option::is_none"
    )]
    pub payer_tenant_id: Option<Uuid>,
    #[serde(
        default,
        deserialize_with = "named_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub contract_id: Option<Option<Uuid>>,
    #[serde(
        default,
        deserialize_with = "named",
        skip_serializing_if = "Option::is_none"
    )]
    pub resource_tenant_id: Option<Uuid>,
    #[serde(
        default,
        deserialize_with = "named",
        skip_serializing_if = "Option::is_none"
    )]
    pub seller_tenant_id: Option<Uuid>,
    #[serde(
        default,
        deserialize_with = "named_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub external_reference: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "named_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub display_label: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "named_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub internal_notes: Option<Option<String>>,
}
impl HeaderPatch {
    /// The wire names this request names, in declaration order.
    #[must_use]
    pub fn named_fields(&self) -> Vec<&'static str> {
        let mut named = Vec::new();
        let mut push = |present: bool, name| {
            if present {
                named.push(name);
            }
        };
        push(self.category.is_some(), "category");
        push(self.payer_tenant_id.is_some(), "payer_tenant_id");
        push(self.contract_id.is_some(), "contract_id");
        push(self.resource_tenant_id.is_some(), "resource_tenant_id");
        push(self.seller_tenant_id.is_some(), "seller_tenant_id");
        push(self.external_reference.is_some(), "external_reference");
        push(self.display_label.is_some(), "display_label");
        push(self.internal_notes.is_some(), "internal_notes");
        named
    }
    /// Boundary validation: at least one named field and bounded administrative text.
    ///
    /// # Errors
    /// `request-invalid` input.
    pub fn validate(&self) -> Result<(), AuthoringInvalid> {
        if self.named_fields().is_empty() {
            return Err(AuthoringInvalid("fields"));
        }
        validate_admin(
            self.external_reference.as_ref(),
            self.display_label.as_ref(),
            self.internal_notes.as_ref(),
        )
    }
}

/// Line `PATCH` fields (`models.json` `LinePatch.fields`). Nullable authored values (dates, term,
/// cycle) clear with `null`; plan, items and currency cannot be cleared.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::option_option)] // Omitted, cleared and replaced are distinct.
pub struct LinePatch {
    #[serde(
        default,
        deserialize_with = "named",
        skip_serializing_if = "Option::is_none"
    )]
    pub plan_id: Option<Uuid>,
    #[serde(
        default,
        deserialize_with = "named",
        skip_serializing_if = "Option::is_none"
    )]
    pub plan_revision_id: Option<Uuid>,
    #[serde(
        default,
        deserialize_with = "named",
        skip_serializing_if = "Option::is_none"
    )]
    pub selected_items: Option<Vec<SelectedItem>>,
    #[serde(
        default,
        deserialize_with = "named",
        skip_serializing_if = "Option::is_none"
    )]
    pub currency: Option<Currency>,
    #[serde(
        default,
        deserialize_with = "named_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub contract_effective_date: Option<Option<CalendarDate>>,
    #[serde(
        default,
        deserialize_with = "named_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub service_activation_date: Option<Option<CalendarDate>>,
    #[serde(
        default,
        deserialize_with = "named_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub acceptance_due_date: Option<Option<CalendarDate>>,
    #[serde(
        default,
        deserialize_with = "named_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub term_duration: Option<Option<AuthoredTerm>>,
    #[serde(
        default,
        deserialize_with = "named_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub billing_cycle: Option<Option<BillingCycle>>,
    #[serde(
        default,
        deserialize_with = "named_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub external_reference: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "named_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub display_label: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "named_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub internal_notes: Option<Option<String>>,
}
impl LinePatch {
    /// The wire names this request names, in declaration order.
    #[must_use]
    pub fn named_fields(&self) -> Vec<&'static str> {
        let mut named = Vec::new();
        let mut push = |present: bool, name| {
            if present {
                named.push(name);
            }
        };
        push(self.plan_id.is_some(), "plan_id");
        push(self.plan_revision_id.is_some(), "plan_revision_id");
        push(self.selected_items.is_some(), "selected_items");
        push(self.currency.is_some(), "currency");
        push(
            self.contract_effective_date.is_some(),
            "contract_effective_date",
        );
        push(
            self.service_activation_date.is_some(),
            "service_activation_date",
        );
        push(self.acceptance_due_date.is_some(), "acceptance_due_date");
        push(self.term_duration.is_some(), "term_duration");
        push(self.billing_cycle.is_some(), "billing_cycle");
        push(self.external_reference.is_some(), "external_reference");
        push(self.display_label.is_some(), "display_label");
        push(self.internal_notes.is_some(), "internal_notes");
        named
    }
    /// Boundary validation: at least one named field, structural item/term values and bounded
    /// administrative text.
    ///
    /// # Errors
    /// `request-invalid` input.
    pub fn validate(&self) -> Result<(), AuthoringInvalid> {
        if self.named_fields().is_empty() {
            return Err(AuthoringInvalid("fields"));
        }
        if let Some(items) = &self.selected_items {
            validate_items(items)?;
        }
        if let Some(Some(term)) = &self.term_duration {
            term.validate()?;
        }
        validate_admin(
            self.external_reference.as_ref(),
            self.display_label.as_ref(),
            self.internal_notes.as_ref(),
        )
    }
}
impl AddLine {
    /// Boundary validation of the authored line's structure (02 §3.3).
    ///
    /// # Errors
    /// `request-invalid` input.
    pub fn validate(&self) -> Result<(), AuthoringInvalid> {
        validate_items(&self.selected_items)?;
        if let Some(term) = &self.term_duration {
            term.validate()?;
        }
        Ok(())
    }
}

/// Selected items: bounded count, positive quantities (by type) and bounded dimension values.
///
/// # Errors
/// `request-invalid` input.
pub fn validate_items(items: &[SelectedItem]) -> Result<(), AuthoringInvalid> {
    if items.len() > MAX_SELECTED_ITEMS {
        return Err(AuthoringInvalid("selected_items"));
    }
    for item in items {
        if let Some(value) = &item.selected_dim_value
            && (value.is_empty()
                || value.chars().count() > DIMENSION_VALUE_MAX
                || value.chars().any(char::is_control))
        {
            return Err(AuthoringInvalid("selected_dim_value"));
        }
    }
    Ok(())
}

fn validate_admin(
    external_reference: Option<&Option<String>>,
    display_label: Option<&Option<String>>,
    internal_notes: Option<&Option<String>>,
) -> Result<(), AuthoringInvalid> {
    for (value, max, name) in [
        (
            external_reference,
            EXTERNAL_REFERENCE_MAX,
            "external_reference",
        ),
        (display_label, DISPLAY_LABEL_MAX, "display_label"),
        (internal_notes, INTERNAL_NOTES_MAX, "internal_notes"),
    ] {
        if let Some(Some(text)) = value
            && (text.chars().count() > max || text.contains('\0'))
        {
            return Err(AuthoringInvalid(name));
        }
    }
    Ok(())
}

/// The order header as committed (the `order` part of `OrderView`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderHeader {
    pub order_id: Uuid,
    /// Seller-unique display and reconciliation handle; no logic keys on its structure.
    pub order_number: String,
    pub state: crate::catalog::OrderState,
    pub category: Category,
    pub resource_tenant_id: Uuid,
    pub seller_tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub contract_id: Option<Uuid>,
    /// `self_service` or `partner_placed`, recorded once at create (D-106/D-140/D-146).
    pub sales_path: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub state_entered_at: OffsetDateTime,
}

/// The current commercial version summary (the `version` part of `OrderView`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionSummary {
    pub version: OrderVersion,
    pub supersedes_version: Option<OrderVersion>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A draft working line as authored (no pin, total or resolved dates exist in draft).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftLine {
    pub line_id: Uuid,
    pub plan_id: Uuid,
    pub plan_revision_id: Uuid,
    pub selected_items: Vec<SelectedItem>,
    pub currency: Currency,
    pub contract_effective_date: Option<CalendarDate>,
    pub service_activation_date: Option<CalendarDate>,
    pub acceptance_due_date: Option<CalendarDate>,
    pub term_duration: Option<AuthoredTerm>,
    pub billing_cycle: Option<BillingCycle>,
}

/// The composed order view (`models.json` `OrderView`): the order at its current version and its
/// lines. Create returns the empty draft's view; the S6-01 read package owns the remaining
/// optional members (`resolved_total`, `acceptance`, `admin`, fulfillment, read-through terms).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderView {
    pub order: OrderHeader,
    pub version: VersionSummary,
    pub lines: Vec<DraftLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_revision: Option<DraftRevision>,
}
