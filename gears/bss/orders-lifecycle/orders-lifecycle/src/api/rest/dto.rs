//! `OpenAPI` declarations of the authoring wire (unique `Orders*` component names, `snake_case`,
//! D-206). The handlers decode the SDK authoring types directly, so the SDK serde shape is the
//! single wire definition; these mirrors only document it, and unit tests pin every mirror to the
//! SDK shape in both directions.
#![allow(clippy::option_option)] // PATCH distinguishes omission, null clearing, and a new value.
use uuid::Uuid;

/// A present `null` names a nullable field (clear); omission does not name it.
fn nullable<'de, D: serde::Deserializer<'de>, T: serde::Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    <Option<T> as serde::Deserialize>::deserialize(d).map(Some)
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
    #[serde(default)]
    pub billing_cycle: Option<String>,
    /// Optional at the boundary; compared by the engine after admissibility (D-147).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_draft_revision: Option<i64>,
}

/// Header fields a `PATCH` may name; `null` clears a nullable field.
#[toolkit_macros::api_dto(request, response)]
#[serde(deny_unknown_fields)]
pub struct OrdersHeaderFields {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer_tenant_id: Option<Uuid>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub contract_id: Option<Option<Uuid>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_tenant_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_revision_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_items: Option<Vec<OrdersSelectedItem>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
        deserialize_with = "nullable",
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

/// Optional line `DELETE` body.
#[toolkit_macros::api_dto(request, response)]
#[serde(deny_unknown_fields)]
pub struct OrdersRemoveLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_draft_revision: Option<i64>,
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

/// One authorized keyset page of orders in `(created_at, order_id)` order.
#[toolkit_macros::api_dto(request, response)]
#[serde(deny_unknown_fields)]
pub struct OrdersOrderPage {
    pub orders: Vec<OrdersOrderSummary>,
    /// Opaque continuation; present only when another authorized row followed the page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// One authorized keyset page of an order's current (draft working) lines in
/// `(created_at, line_id)` identity order, with the version and draft revision of the same
/// consistent snapshot.
#[toolkit_macros::api_dto(request, response)]
#[serde(deny_unknown_fields)]
pub struct OrdersLinePage {
    pub lines: Vec<OrdersDraftLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub current_version: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_revision: Option<i64>,
}
