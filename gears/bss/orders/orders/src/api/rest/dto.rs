//! The authoring wire (unique `Orders*` component names, `snake_case`, D-206): these types ARE
//! the decoded REST request bodies and the serialized read responses, and the `OpenAPI` document
//! is generated from them. Each request body converts into its SDK authoring type with
//! `TryFrom`, so the SDK serde shape (kept for in-process callers and stored data) is not the HTTP
//! contract (TOOLKIT-CORE-001). The read responses ([`OrdersOrderView`], [`OrdersOrderPage`],
//! [`OrdersLinePage`]) are converted from the SDK read results with `TryFrom` and serialized; every
//! scalar is spelled by the SDK's own serializer, so the bytes are the SDK serialization's
//! (`orders/tests/rest_responses.rs` pins that). The write responses are not converted: a settled
//! write answers its stored SDK snapshot byte for byte, so a same-key replay is identical to the
//! first answer, and [`OrdersOrderView`] / [`OrdersTransitionResult`] only document that shape.
//!
//! The handlers extract `expected_draft_revision` and, for a `PATCH`, the `{ "fields": { ... } }`
//! envelope first, then decode [`OrdersCreateOrder`], [`OrdersAddLine`], [`OrdersHeaderFields`] or
//! [`OrdersLineFields`]. The rules are the SDK's, reproduced exactly: unknown members are refused at
//! every level; a `PATCH` field that is omitted is not named, a present `null` clears a nullable
//! field and is invalid on a non-nullable one. Every scalar is converted by the SDK's own parser,
//! so dates, quantities, currencies, categories and cycles have one rule each.
#![expect(
    clippy::option_option,
    reason = "a PATCH body distinguishes an omitted field, a null that clears it and a new value"
)]
use bss_orders_sdk::authoring::{
    AddLine, AuthoredTerm, AuthoringInvalid, BillingCycle, CalendarDate, Category, CreateOrder,
    Currency, DraftLine, HeaderPatch, LinePatch, OrderHeader, OrderView, Quantity, SelectedItem,
    VersionSummary,
};
use bss_orders_sdk::reads::{LinePage, OrderPage, OrderSummary, PageInfo};
use serde::Deserialize as _;
use uuid::Uuid;

/// A present `null` names a nullable field (clear); omission does not name it.
fn nullable<'de, D: serde::Deserializer<'de>, T: serde::Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    <Option<T> as serde::Deserialize>::deserialize(d).map(Some)
}

/// A present value names a non-nullable field; `null` is invalid rather than "not named".
fn present<'de, D: serde::Deserializer<'de>, T: serde::Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

/// The billing cycle tokens as the wire has always decoded them: the token string, or the
/// externally tagged unit form `{"month": null}` that a derived unit enum also accepts.
#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum CycleToken {
    Month,
    Year,
}
impl CycleToken {
    fn into_text(self) -> String {
        match self {
            Self::Month => "month",
            Self::Year => "year",
        }
        .to_owned()
    }
}

/// An optional billing cycle: `null` or omitted is absent.
fn cycle<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(Option::<CycleToken>::deserialize(d)?.map(CycleToken::into_text))
}

/// A nullable `PATCH` billing cycle: `null` clears, omission does not name it.
fn nullable_cycle<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<String>>, D::Error> {
    cycle(d).map(Some)
}

/// One wire scalar through the SDK's own decoder, so the rule (canonical spelling, range, sign)
/// is the SDK's exactly.
fn scalar<T: serde::de::DeserializeOwned>(
    text: String,
    field: &'static str,
) -> Result<T, AuthoringInvalid> {
    serde_json::from_value(serde_json::Value::String(text)).map_err(|_| AuthoringInvalid(field))
}

fn category(text: &str) -> Result<Category, AuthoringInvalid> {
    Category::parse(text).ok_or(AuthoringInvalid("category"))
}

fn date(
    text: Option<String>,
    field: &'static str,
) -> Result<Option<CalendarDate>, AuthoringInvalid> {
    text.map(|text| scalar(text, field)).transpose()
}

fn billing_cycle(text: Option<String>) -> Result<Option<BillingCycle>, AuthoringInvalid> {
    text.map(|text| scalar(text, "billing_cycle")).transpose()
}

fn items(items: Vec<OrdersSelectedItem>) -> Result<Vec<SelectedItem>, AuthoringInvalid> {
    items.into_iter().map(SelectedItem::try_from).collect()
}

/// A nullable field converted when named; a cleared or omitted field keeps its presence.
fn named<T, U>(
    value: Option<Option<T>>,
    convert: impl FnOnce(T) -> Result<U, AuthoringInvalid>,
) -> Result<Option<Option<U>>, AuthoringInvalid> {
    value
        .map(|value| value.map(convert).transpose())
        .transpose()
}

/// Authored term intent (D-193): `rolling`, a whole number of `periods`, or a finite `calendar`
/// interval; omitted is the distinct *missing* intent.
#[toolkit_macros::api_dto(request, response)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum OrdersAuthoredTerm {
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

/// One selected plan item: identity, positive exact-decimal quantity string, dimension choice.
#[toolkit_macros::api_dto(request, response)]
#[serde(deny_unknown_fields)]
pub struct OrdersSelectedItem {
    pub item_id: Uuid,
    #[serde(default)]
    pub quantity: Option<String>,
    #[serde(default)]
    pub selected_dim_value: Option<String>,
}

/// Create an empty draft. `category` is a registered `gts.cf.bss.orders.category.v1~` identity.
#[toolkit_macros::api_dto(request, response)]
#[serde(deny_unknown_fields)]
pub struct OrdersCreateOrder {
    pub resource_tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub seller_tenant_id: Uuid,
    pub category: String,
    #[serde(default)]
    pub contract_id: Option<Uuid>,
}

/// Add a line. `billing_cycle` is `month` or `year`.
#[toolkit_macros::api_dto(request, response)]
#[serde(deny_unknown_fields)]
pub struct OrdersAddLine {
    pub plan_id: Uuid,
    pub plan_revision_id: Uuid,
    pub selected_items: Vec<OrdersSelectedItem>,
    pub currency: String,
    #[serde(default)]
    pub contract_effective_date: Option<String>,
    #[serde(default)]
    pub service_activation_date: Option<String>,
    #[serde(default)]
    pub acceptance_due_date: Option<String>,
    #[serde(default)]
    pub term_duration: Option<OrdersAuthoredTerm>,
    #[serde(default, deserialize_with = "cycle")]
    pub billing_cycle: Option<String>,
    /// Optional at the boundary; compared by the engine after admissibility (D-147).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_draft_revision: Option<i64>,
}

/// Header fields a `PATCH` may name; `null` clears a nullable field.
#[toolkit_macros::api_dto(request, response)]
#[serde(deny_unknown_fields)]
pub struct OrdersHeaderFields {
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub category: Option<String>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub payer_tenant_id: Option<Uuid>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub contract_id: Option<Option<Uuid>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub resource_tenant_id: Option<Uuid>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub seller_tenant_id: Option<Uuid>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub external_reference: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub display_label: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub internal_notes: Option<Option<String>>,
}

/// Header `PATCH` body.
#[toolkit_macros::api_dto(request, response)]
#[serde(deny_unknown_fields)]
pub struct OrdersHeaderPatch {
    pub fields: OrdersHeaderFields,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_draft_revision: Option<i64>,
}

/// Line fields a `PATCH` may name; `null` clears a nullable field.
#[toolkit_macros::api_dto(request, response)]
#[serde(deny_unknown_fields)]
pub struct OrdersLineFields {
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub plan_id: Option<Uuid>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub plan_revision_id: Option<Uuid>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub selected_items: Option<Vec<OrdersSelectedItem>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub currency: Option<String>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub contract_effective_date: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub service_activation_date: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub acceptance_due_date: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub term_duration: Option<Option<OrdersAuthoredTerm>>,
    #[serde(
        default,
        deserialize_with = "nullable_cycle",
        skip_serializing_if = "Option::is_none"
    )]
    pub billing_cycle: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub external_reference: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub display_label: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub internal_notes: Option<Option<String>>,
}

/// Line `PATCH` body.
#[toolkit_macros::api_dto(request, response)]
#[serde(deny_unknown_fields)]
pub struct OrdersLinePatch {
    pub fields: OrdersLineFields,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_draft_revision: Option<i64>,
}

/// Submit carries only its independent draft revision; actor/version/key are not body fields.
/// Not mounted yet (S3-12 delivers the submit route); the boundary fixtures decode it.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitBody {
    pub expected_draft_revision: bss_orders_sdk::models::DraftRevision,
}

/// Optional line `DELETE` body.
#[toolkit_macros::api_dto(request, response)]
#[serde(deny_unknown_fields)]
pub struct OrdersRemoveLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_draft_revision: Option<i64>,
}

impl From<OrdersAuthoredTerm> for AuthoredTerm {
    fn from(term: OrdersAuthoredTerm) -> Self {
        match term {
            OrdersAuthoredTerm::Rolling => Self::Rolling,
            OrdersAuthoredTerm::Periods { count } => Self::Periods { count },
            OrdersAuthoredTerm::Calendar {
                years,
                months,
                days,
                microseconds,
            } => Self::Calendar {
                years,
                months,
                days,
                microseconds,
            },
        }
    }
}

impl TryFrom<OrdersSelectedItem> for SelectedItem {
    type Error = AuthoringInvalid;
    fn try_from(item: OrdersSelectedItem) -> Result<Self, Self::Error> {
        Ok(Self {
            item_id: item.item_id,
            quantity: item
                .quantity
                .map(|text| scalar::<Quantity>(text, "quantity"))
                .transpose()?,
            selected_dim_value: item.selected_dim_value,
        })
    }
}

impl TryFrom<OrdersCreateOrder> for CreateOrder {
    type Error = AuthoringInvalid;
    fn try_from(body: OrdersCreateOrder) -> Result<Self, Self::Error> {
        Ok(Self {
            resource_tenant_id: body.resource_tenant_id,
            payer_tenant_id: body.payer_tenant_id,
            seller_tenant_id: body.seller_tenant_id,
            category: category(&body.category)?,
            contract_id: body.contract_id,
        })
    }
}

/// `expected_draft_revision` is extracted into call metadata before the body is decoded, so it
/// is not part of the SDK line.
impl TryFrom<OrdersAddLine> for AddLine {
    type Error = AuthoringInvalid;
    fn try_from(body: OrdersAddLine) -> Result<Self, Self::Error> {
        Ok(Self {
            plan_id: body.plan_id,
            plan_revision_id: body.plan_revision_id,
            selected_items: items(body.selected_items)?,
            currency: Currency::try_from(body.currency)?,
            contract_effective_date: date(body.contract_effective_date, "contract_effective_date")?,
            service_activation_date: date(body.service_activation_date, "service_activation_date")?,
            acceptance_due_date: date(body.acceptance_due_date, "acceptance_due_date")?,
            term_duration: body.term_duration.map(AuthoredTerm::from),
            billing_cycle: billing_cycle(body.billing_cycle)?,
        })
    }
}

impl TryFrom<OrdersHeaderFields> for HeaderPatch {
    type Error = AuthoringInvalid;
    fn try_from(fields: OrdersHeaderFields) -> Result<Self, Self::Error> {
        Ok(Self {
            category: fields.category.as_deref().map(category).transpose()?,
            payer_tenant_id: fields.payer_tenant_id,
            contract_id: fields.contract_id,
            resource_tenant_id: fields.resource_tenant_id,
            seller_tenant_id: fields.seller_tenant_id,
            external_reference: fields.external_reference,
            display_label: fields.display_label,
            internal_notes: fields.internal_notes,
        })
    }
}

impl TryFrom<OrdersLineFields> for LinePatch {
    type Error = AuthoringInvalid;
    fn try_from(fields: OrdersLineFields) -> Result<Self, Self::Error> {
        let dated = |text, field| scalar::<CalendarDate>(text, field);
        Ok(Self {
            plan_id: fields.plan_id,
            plan_revision_id: fields.plan_revision_id,
            selected_items: fields.selected_items.map(items).transpose()?,
            currency: fields.currency.map(Currency::try_from).transpose()?,
            contract_effective_date: named(fields.contract_effective_date, |text| {
                dated(text, "contract_effective_date")
            })?,
            service_activation_date: named(fields.service_activation_date, |text| {
                dated(text, "service_activation_date")
            })?,
            acceptance_due_date: named(fields.acceptance_due_date, |text| {
                dated(text, "acceptance_due_date")
            })?,
            term_duration: fields
                .term_duration
                .map(|term| term.map(AuthoredTerm::from)),
            billing_cycle: named(fields.billing_cycle, |text| {
                scalar::<BillingCycle>(text, "billing_cycle")
            })?,
            external_reference: fields.external_reference,
            display_label: fields.display_label,
            internal_notes: fields.internal_notes,
        })
    }
}

/// The committed (or replayed) transition outcome.
#[toolkit_macros::api_dto(request, response)]
#[serde(deny_unknown_fields)]
pub struct OrdersTransitionResult {
    pub order_id: Uuid,
    pub state: String,
    pub version: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_revision: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_audit_id: Option<Uuid>,
    /// The server-reserved identity of an inserted line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_id: Option<Uuid>,
}

/// The order header at its current version.
#[toolkit_macros::api_dto(request, response)]
#[serde(deny_unknown_fields)]
pub struct OrdersOrderHeader {
    pub order_id: Uuid,
    pub order_number: String,
    pub state: String,
    pub category: String,
    pub resource_tenant_id: Uuid,
    pub seller_tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub contract_id: Option<Uuid>,
    pub sales_path: String,
    pub created_at: String,
    pub state_entered_at: String,
}

/// The current commercial version summary.
#[toolkit_macros::api_dto(request, response)]
#[serde(deny_unknown_fields)]
pub struct OrdersVersionSummary {
    pub version: i64,
    pub supersedes_version: Option<i64>,
    pub created_at: String,
}

/// A draft working line as authored.
#[toolkit_macros::api_dto(request, response)]
#[serde(deny_unknown_fields)]
pub struct OrdersDraftLine {
    pub line_id: Uuid,
    pub plan_id: Uuid,
    pub plan_revision_id: Uuid,
    pub selected_items: Vec<OrdersSelectedItem>,
    pub currency: String,
    pub contract_effective_date: Option<String>,
    pub service_activation_date: Option<String>,
    pub acceptance_due_date: Option<String>,
    pub term_duration: Option<OrdersAuthoredTerm>,
    pub billing_cycle: Option<String>,
}

/// The composed order view (create returns the empty draft's view).
#[toolkit_macros::api_dto(request, response)]
#[serde(deny_unknown_fields)]
pub struct OrdersOrderView {
    pub order: OrdersOrderHeader,
    pub version: OrdersVersionSummary,
    pub lines: Vec<OrdersDraftLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_revision: Option<i64>,
}

/// One order-list element: the header at its current version (early S6-01 draft subset).
#[toolkit_macros::api_dto(request, response)]
#[serde(deny_unknown_fields)]
pub struct OrdersOrderSummary {
    pub order: OrdersOrderHeader,
    pub current_version: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_revision: Option<i64>,
}

/// Page metadata in the shared list format (DNA REST `PAGINATION.md`, D-212).
#[toolkit_macros::api_dto(request, response)]
#[serde(deny_unknown_fields)]
pub struct OrdersPageInfo {
    /// Opaque continuation; present only when another authorized row followed the page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Always absent: reads page forward only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_cursor: Option<String>,
    /// The effective page size (1-200).
    pub limit: u64,
}

/// One authorized keyset page of orders in `(created_at, order_id)` order.
#[toolkit_macros::api_dto(request, response)]
#[serde(deny_unknown_fields)]
pub struct OrdersOrderPage {
    pub items: Vec<OrdersOrderSummary>,
    pub page_info: OrdersPageInfo,
}

/// One authorized keyset page of an order's current (draft working) lines in
/// `(created_at, line_id)` identity order, with the version and draft revision of the same
/// consistent snapshot.
#[toolkit_macros::api_dto(request, response)]
#[serde(deny_unknown_fields)]
pub struct OrdersLinePage {
    pub items: Vec<OrdersDraftLine>,
    pub page_info: OrdersPageInfo,
    pub current_version: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_revision: Option<i64>,
}

// ---------------------------------------------------------------------------------------------
// Read responses: SDK read result -> REST DTO

/// A read result member the SDK serializer refuses (a stored date or instant outside the
/// four-digit year range). The response is the sanitized internal error, as an unserializable
/// SDK body was before; the member name is the only detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("read response member `{0}` has no wire spelling")]
pub struct ResponseUnencodable(pub &'static str);

/// One scalar exactly as the SDK serializes it: the SDK's serializer is the one rule, as the SDK
/// parser is on the request side ([`scalar`]).
fn spelled<T: serde::Serialize>(
    value: &T,
    field: &'static str,
) -> Result<String, ResponseUnencodable> {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => Ok(text),
        _ => Err(ResponseUnencodable(field)),
    }
}

fn spelled_opt<T: serde::Serialize>(
    value: Option<&T>,
    field: &'static str,
) -> Result<Option<String>, ResponseUnencodable> {
    value.map(|value| spelled(value, field)).transpose()
}

impl From<AuthoredTerm> for OrdersAuthoredTerm {
    fn from(term: AuthoredTerm) -> Self {
        match term {
            AuthoredTerm::Rolling => Self::Rolling,
            AuthoredTerm::Periods { count } => Self::Periods { count },
            AuthoredTerm::Calendar {
                years,
                months,
                days,
                microseconds,
            } => Self::Calendar {
                years,
                months,
                days,
                microseconds,
            },
        }
    }
}

impl TryFrom<SelectedItem> for OrdersSelectedItem {
    type Error = ResponseUnencodable;
    fn try_from(item: SelectedItem) -> Result<Self, Self::Error> {
        Ok(Self {
            item_id: item.item_id,
            quantity: spelled_opt(item.quantity.as_ref(), "quantity")?,
            selected_dim_value: item.selected_dim_value,
        })
    }
}

impl TryFrom<OrderHeader> for OrdersOrderHeader {
    type Error = ResponseUnencodable;
    fn try_from(header: OrderHeader) -> Result<Self, Self::Error> {
        Ok(Self {
            order_id: header.order_id,
            order_number: header.order_number,
            state: spelled(&header.state, "state")?,
            category: spelled(&header.category, "category")?,
            resource_tenant_id: header.resource_tenant_id,
            seller_tenant_id: header.seller_tenant_id,
            payer_tenant_id: header.payer_tenant_id,
            contract_id: header.contract_id,
            sales_path: spelled(&header.sales_path, "sales_path")?,
            created_at: instant(header.created_at, "created_at")?,
            state_entered_at: instant(header.state_entered_at, "state_entered_at")?,
        })
    }
}

/// An instant in the SDK's RFC 3339 spelling (`time::serde::rfc3339`).
fn instant(at: time::OffsetDateTime, field: &'static str) -> Result<String, ResponseUnencodable> {
    #[derive(serde::Serialize)]
    #[serde(transparent)]
    struct Rfc3339(#[serde(with = "time::serde::rfc3339")] time::OffsetDateTime);
    spelled(&Rfc3339(at), field)
}

impl TryFrom<VersionSummary> for OrdersVersionSummary {
    type Error = ResponseUnencodable;
    fn try_from(version: VersionSummary) -> Result<Self, Self::Error> {
        Ok(Self {
            version: version.version.into(),
            supersedes_version: version.supersedes_version.map(i64::from),
            created_at: instant(version.created_at, "created_at")?,
        })
    }
}

impl TryFrom<DraftLine> for OrdersDraftLine {
    type Error = ResponseUnencodable;
    fn try_from(line: DraftLine) -> Result<Self, Self::Error> {
        Ok(Self {
            line_id: line.line_id,
            plan_id: line.plan_id,
            plan_revision_id: line.plan_revision_id,
            selected_items: line
                .selected_items
                .into_iter()
                .map(OrdersSelectedItem::try_from)
                .collect::<Result<_, _>>()?,
            currency: spelled(&line.currency, "currency")?,
            contract_effective_date: spelled_opt(
                line.contract_effective_date.as_ref(),
                "contract_effective_date",
            )?,
            service_activation_date: spelled_opt(
                line.service_activation_date.as_ref(),
                "service_activation_date",
            )?,
            acceptance_due_date: spelled_opt(
                line.acceptance_due_date.as_ref(),
                "acceptance_due_date",
            )?,
            term_duration: line.term_duration.map(OrdersAuthoredTerm::from),
            billing_cycle: spelled_opt(line.billing_cycle.as_ref(), "billing_cycle")?,
        })
    }
}

fn lines(lines: Vec<DraftLine>) -> Result<Vec<OrdersDraftLine>, ResponseUnencodable> {
    lines.into_iter().map(OrdersDraftLine::try_from).collect()
}

impl TryFrom<OrderView> for OrdersOrderView {
    type Error = ResponseUnencodable;
    fn try_from(view: OrderView) -> Result<Self, Self::Error> {
        Ok(Self {
            order: view.order.try_into()?,
            version: view.version.try_into()?,
            lines: lines(view.lines)?,
            draft_revision: view.draft_revision.map(i64::from),
        })
    }
}

impl TryFrom<OrderSummary> for OrdersOrderSummary {
    type Error = ResponseUnencodable;
    fn try_from(summary: OrderSummary) -> Result<Self, Self::Error> {
        Ok(Self {
            order: summary.order.try_into()?,
            current_version: summary.current_version.into(),
            draft_revision: summary.draft_revision.map(i64::from),
        })
    }
}

impl From<PageInfo> for OrdersPageInfo {
    fn from(info: PageInfo) -> Self {
        Self {
            next_cursor: info.next_cursor.map(|c| c.as_str().to_owned()),
            prev_cursor: info.prev_cursor.map(|c| c.as_str().to_owned()),
            limit: info.limit.into(),
        }
    }
}

impl TryFrom<OrderPage> for OrdersOrderPage {
    type Error = ResponseUnencodable;
    fn try_from(page: OrderPage) -> Result<Self, Self::Error> {
        Ok(Self {
            items: page
                .items
                .into_iter()
                .map(OrdersOrderSummary::try_from)
                .collect::<Result<_, _>>()?,
            page_info: page.page_info.into(),
        })
    }
}

impl TryFrom<LinePage> for OrdersLinePage {
    type Error = ResponseUnencodable;
    fn try_from(page: LinePage) -> Result<Self, Self::Error> {
        Ok(Self {
            items: lines(page.items)?,
            page_info: page.page_info.into(),
            current_version: page.current_version.into(),
            draft_revision: page.draft_revision.map(i64::from),
        })
    }
}
