//! Gear-local `snake_case` wire types; SDK enums are represented by their stable tokens. A closed
//! set on a response is its `enum` (P-D-217); a request keeps `string`, so its door refuses.
use super::closed_sets::{
    ProductsApprovalKind, ProductsBillingTiming, ProductsCategoryStatus, ProductsDecisionKind,
    ProductsLifecycle, ProductsReferenceKind, ProductsReferenceState, ProductsSkuType,
    ProductsUnitState, ProductsVoteOutcome,
};
use crate::domain::sku::{NewSku, SkuPatch};
use crate::domain::validation::ValidationReport;
use crate::infra::storage::RepoError;
use bss_products_sdk::models::{
    BillingTiming, Category, Lifecycle, Sku, SkuContent, SkuType, SkuVersion,
};
use serde::Deserialize;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

// `Products`-prefixed like the gear's other shared names: settings-service serves its own
// `CategoryDto`, and the toolkit refuses two definitions under one component name at boot.
/// Wire representation of the registry Category.
#[toolkit_macros::api_dto(response)]
pub struct ProductsCategoryDto {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub code: String,
    pub name: String,
    pub is_default: bool,
    pub sort_order: i32,
    pub status: ProductsCategoryStatus,
    pub version: i64,
    /// When the retired category was archived (P-D-263); null while it is not. Its list hides it
    /// unless asked `archived eq true`; a read by id ignores the mark.
    #[serde(with = "time::serde::rfc3339::option")]
    pub archived_at: Option<OffsetDateTime>,
    /// Who archived it; null while it is not archived.
    pub archived_by: Option<Uuid>,
    /// The current name of `archived_by` (P-D-262), as `created_by_name` names its actor; null
    /// when no name is available now, and on a write answer.
    pub archived_by_name: Option<String>,
}
impl TryFrom<Category> for ProductsCategoryDto {
    type Error = RepoError;
    fn try_from(value: Category) -> Result<Self, RepoError> {
        Ok(Self {
            id: value.id,
            tenant_id: value.tenant_id,
            code: value.code,
            name: value.name,
            is_default: value.is_default,
            sort_order: value.sort_order,
            status: ProductsCategoryStatus::stored(
                &value.status,
                &format_args!("category {} status", value.id),
            )?,
            version: value.version,
            archived_at: value.archived_at,
            archived_by: value.archived_by,
            archived_by_name: None,
        })
    }
}
/// A lifecycle change that takes effect on `from` (P-D-249).
#[toolkit_macros::api_dto(response)]
pub struct LifecycleNextDto {
    pub lifecycle: ProductsLifecycle,
    #[serde(with = "crate::infra::serde_date")]
    pub from: Date,
}
/// Wire representation of the registry Sku.
#[toolkit_macros::api_dto(response)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "sellable, type_change_pending and retire_pending are three independent flags (P-D-248)"
)]
pub struct SkuDto {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub code: String,
    pub name: String,
    #[serde(rename = "type")]
    pub r#type: ProductsSkuType,
    /// `null`: the SKU has no category (P-D-196).
    pub category_id: Option<Uuid>,
    pub description: String,
    pub sellable: bool,
    pub lifecycle: ProductsLifecycle,
    /// Set while a retire is in review (P-D-248). The lifecycle stays.
    pub retire_pending: bool,
    /// A dated lifecycle change that has not arrived (P-D-249). Null when none is pending.
    pub lifecycle_next: Option<LifecycleNextDto>,
    pub revision: i64,
    pub published_version: i64,
    pub gl_code: Option<String>,
    pub tax_category: Option<String>,
    pub invoice_line_template: Option<String>,
    pub billing_timing: Option<ProductsBillingTiming>,
    pub usage_type_ref: Option<String>,
    pub unit: Option<String>,
    pub type_change_pending: bool,
    pub pending_unit_id: Option<Uuid>,
    pub approved_by_unit_id: Option<Uuid>,
    pub created_by: Uuid,
    /// The current name of `created_by` (P-D-262): its display name, else first and last name, else
    /// username, from Account Management under the caller's rights; "System" for a system actor.
    /// Null when no name is available now, and on a write answer.
    pub created_by_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// When the retired SKU was archived (P-D-263); null while it is not. The SKU list hides it
    /// unless asked `archived eq true`; a read by id ignores the mark. Not a lifecycle.
    #[serde(with = "time::serde::rfc3339::option")]
    pub archived_at: Option<OffsetDateTime>,
    /// Who archived it; null while it is not archived.
    pub archived_by: Option<Uuid>,
    /// The current name of `archived_by` (P-D-262), as `created_by_name` names its actor; null
    /// when no name is available now, and on a write answer.
    pub archived_by_name: Option<String>,
}
impl From<Sku> for SkuDto {
    fn from(value: Sku) -> Self {
        Self {
            id: value.id,
            tenant_id: value.tenant_id,
            code: value.code,
            name: value.name,
            r#type: value.r#type.into(),
            category_id: value.category_id,
            description: value.description,
            sellable: value.sellable,
            lifecycle: value.lifecycle.into(),
            retire_pending: value.retire_pending,
            lifecycle_next: value.lifecycle_next.map(|next| LifecycleNextDto {
                lifecycle: next.lifecycle.into(),
                from: next.from,
            }),
            revision: value.revision,
            published_version: value.published_version,
            gl_code: value.gl_code,
            tax_category: value.tax_category,
            invoice_line_template: value.invoice_line_template,
            billing_timing: value.billing_timing.map(Into::into),
            usage_type_ref: value.usage_type_ref,
            unit: value.unit,
            type_change_pending: value.type_change_pending,
            pending_unit_id: value.pending_unit_id,
            approved_by_unit_id: value.approved_by_unit_id,
            created_by: value.created_by,
            created_by_name: None,
            created_at: value.created_at,
            updated_at: value.updated_at,
            archived_at: value.archived_at,
            archived_by: value.archived_by,
            archived_by_name: None,
        }
    }
}
/// Wire representation of the registry `SkuContent`.
#[toolkit_macros::api_dto(response)]
pub struct SkuContentDto {
    pub code: String,
    pub name: String,
    #[serde(rename = "type")]
    pub r#type: ProductsSkuType,
    /// `null`: the SKU has no category (P-D-196).
    pub category_id: Option<Uuid>,
    pub description: String,
    pub sellable: bool,
    pub gl_code: Option<String>,
    pub tax_category: Option<String>,
    pub invoice_line_template: Option<String>,
    pub billing_timing: Option<ProductsBillingTiming>,
    pub usage_type_ref: Option<String>,
    pub unit: Option<String>,
}
impl From<SkuContent> for SkuContentDto {
    fn from(value: SkuContent) -> Self {
        Self {
            code: value.code,
            name: value.name,
            r#type: value.r#type.into(),
            category_id: value.category_id,
            description: value.description,
            sellable: value.sellable,
            gl_code: value.gl_code,
            tax_category: value.tax_category,
            invoice_line_template: value.invoice_line_template,
            billing_timing: value.billing_timing.map(Into::into),
            usage_type_ref: value.usage_type_ref,
            unit: value.unit,
        }
    }
}
/// Wire representation of the registry `SkuVersion`.
#[toolkit_macros::api_dto(response)]
pub struct SkuVersionDto {
    pub sku_id: Uuid,
    pub published_version: i64,
    #[serde(with = "crate::infra::serde_date")]
    pub effective_from: Date,
    pub content: SkuContentDto,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}
impl From<SkuVersion> for SkuVersionDto {
    fn from(value: SkuVersion) -> Self {
        Self {
            sku_id: value.sku_id,
            published_version: value.published_version,
            effective_from: value.effective_from,
            content: value.content.into(),
            created_at: value.created_at,
        }
    }
}

/// Parse one enum token, retaining the wire field in validation failures.
pub(crate) fn parse_token<T>(
    value: &str,
    field: &str,
    parse: fn(&str) -> Option<T>,
) -> Result<T, ValidationReport> {
    parse(value).ok_or_else(|| {
        let mut r = ValidationReport::new();
        r.violate("VALIDATION", field, format!("unknown {field}: {value}"));
        r
    })
}
/// Preserve explicit null as a present patch value.
#[expect(clippy::option_option, reason = "a PATCH field has three states")]
fn double_option<'de, T: Deserialize<'de>, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}
#[toolkit_macros::api_dto(request)]
pub struct CategoryRequest {
    pub code: String,
    pub name: String,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub sort_order: i32,
}
#[toolkit_macros::api_dto(request)]
pub struct CategoryPatchRequest {
    pub name: Option<String>,
    pub is_default: Option<bool>,
    pub sort_order: Option<i32>,
}
/// A category as its reads answer it (P-D-215): the category's fields and `sku_count`, the SKUs
/// that are not retired naming it — the ones that keep it in use (P-D-208), so a category with
/// `sku_count` 0 may be retired.
#[toolkit_macros::api_dto(response)]
pub struct ProductsCategoryItem {
    #[serde(flatten)]
    pub category: ProductsCategoryDto,
    pub sku_count: u64,
}
#[toolkit_macros::api_dto(response)]
pub struct SkuCard {
    pub sku: SkuDto,
    pub references: ReferencesDto,
    /// Pricing's usage of the SKU through the port pricing fills (P-D-197); `null` when no port is
    /// registered, when it refuses the caller, or when it cannot answer.
    pub usage: Option<SkuUsageDto>,
}
/// A SKU's prices by state, as pricing counts them (pricing D-428).
#[toolkit_macros::api_dto(response)]
pub struct SkuUsagePricesDto {
    pub approved: u64,
    pub pending: u64,
    pub draft: u64,
}
/// Pricing's usage of a SKU (P-D-197): its price-book entries, the distinct currencies of their
/// books (sorted), their prices by state, and the distinct plans naming them. Information for the
/// SKUs screen; never a fence input.
#[toolkit_macros::api_dto(response)]
pub struct SkuUsageDto {
    pub entries: u64,
    pub currencies: Vec<String>,
    pub prices: SkuUsagePricesDto,
    pub plans: u64,
}
impl From<bss_products_sdk::sku_usage::SkuUsage> for SkuUsageDto {
    fn from(u: bss_products_sdk::sku_usage::SkuUsage) -> Self {
        Self {
            entries: u.entries,
            currencies: u.currencies,
            prices: SkuUsagePricesDto {
                approved: u.prices.approved,
                pending: u.prices.pending,
                draft: u.prices.draft,
            },
            plans: u.plans,
        }
    }
}
#[toolkit_macros::api_dto(response)]
pub struct ReferencesDto {
    pub price_book_entries: u32,
    pub plans: u32,
    pub reserved: u32,
    pub by_owner: std::collections::BTreeMap<String, std::collections::BTreeMap<String, u32>>,
}
impl From<crate::domain::references::ReferenceSummary> for ReferencesDto {
    fn from(v: crate::domain::references::ReferenceSummary) -> Self {
        Self {
            price_book_entries: v.price_book_entries,
            plans: v.plans,
            reserved: v.reserved,
            by_owner: v.by_owner,
        }
    }
}
/// One item of the SKU list: the SKU's fields and pricing's `usage` (P-D-197), `null` when the
/// port is absent, refuses or cannot answer.
#[toolkit_macros::api_dto(response)]
pub struct SkuListItem {
    #[serde(flatten)]
    pub sku: SkuDto,
    pub usage: Option<SkuUsageDto>,
}
/// The SKU list's tab counts (P-D-211): every SKU the narrowing keeps, those in each lifecycle,
/// and those a pending approval unit locks (in any lifecycle), none of them archived; and the
/// archived SKUs the narrowing keeps, which no other number counts (P-D-263).
#[toolkit_macros::api_dto(response)]
pub struct ProductsSkuCounts {
    pub all: u64,
    pub draft: u64,
    pub published: u64,
    pub deprecated: u64,
    pub retired: u64,
    pub in_review: u64,
    pub archived: u64,
}
impl From<crate::infra::storage::repo::SkuCounts> for ProductsSkuCounts {
    fn from(c: crate::infra::storage::repo::SkuCounts) -> Self {
        Self {
            all: c.all,
            draft: c.draft,
            published: c.published,
            deprecated: c.deprecated,
            retired: c.retired,
            in_review: c.in_review,
            archived: c.archived,
        }
    }
}
/// One act in a SKU's history (P-D-213): when (`at`, the row's `written_at`: the submit, change and
/// draft doors take it before their transaction and keep it across a retry, the other writers inside
/// the attempt — never the commit; the history is in the order the acts wrote, by the audit row's id), who (`actor`, the nil uuid for the system's orphan-fence expiry), what
/// (`action`), the lifecycle it found and left (`null` on a row written before the audit log
/// carried them, on a create's `from`), the approval unit it concerned and its kind, and the note
/// it carried (a change's note, a decision's note, or the expiry's TTL).
#[toolkit_macros::api_dto(response)]
pub struct ProductsSkuHistoryEntry {
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub actor: Uuid,
    /// The current name of `actor` (P-D-262): its display name, else first and last name, else
    /// username, from Account Management under the caller's rights; "System" for a system actor.
    /// Null when no name is available now.
    pub actor_name: Option<String>,
    /// The audit row's action: a string, since no CHECK holds the column to a set (P-D-217).
    pub action: String,
    pub from_lifecycle: Option<ProductsLifecycle>,
    pub to_lifecycle: Option<ProductsLifecycle>,
    pub unit_id: Option<Uuid>,
    pub unit_kind: Option<String>,
    pub note: Option<String>,
}
impl From<crate::infra::storage::repo::SkuHistoryEntry> for ProductsSkuHistoryEntry {
    fn from(e: crate::infra::storage::repo::SkuHistoryEntry) -> Self {
        Self {
            at: e.at,
            actor: e.actor,
            actor_name: None,
            action: e.action,
            from_lifecycle: e.from_lifecycle.map(Into::into),
            to_lifecycle: e.to_lifecycle.map(Into::into),
            unit_id: e.unit_id,
            unit_kind: e.unit_kind,
            note: e.note,
        }
    }
}
#[toolkit_macros::api_dto(request)]
pub struct SkuRequest {
    pub code: String,
    pub name: String,
    #[serde(rename = "type")]
    pub r#type: String,
    /// Optional (P-D-196): omitted or `null` stays null, with no fallback to the default category.
    #[serde(default)]
    pub category_id: Option<Uuid>,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_sellable")]
    pub sellable: bool,
    pub gl_code: Option<String>,
    pub tax_category: Option<String>,
    pub invoice_line_template: Option<String>,
    pub billing_timing: Option<String>,
    pub usage_type_ref: Option<String>,
    pub unit: Option<String>,
}
const fn default_sellable() -> bool {
    true
}
impl TryFrom<SkuRequest> for NewSku {
    type Error = ValidationReport;
    fn try_from(v: SkuRequest) -> Result<Self, Self::Error> {
        Ok(Self {
            code: v.code.trim().to_owned(),
            name: v.name.trim().to_owned(),
            r#type: parse_token(&v.r#type, "type", SkuType::parse)?,
            category_id: v.category_id,
            description: v.description,
            sellable: v.sellable,
            gl_code: v.gl_code,
            tax_category: v.tax_category,
            invoice_line_template: v.invoice_line_template,
            billing_timing: v
                .billing_timing
                .as_deref()
                .map(|s| parse_token(s, "billing_timing", BillingTiming::parse))
                .transpose()?,
            usage_type_ref: v.usage_type_ref,
            unit: v.unit,
        })
    }
}
#[toolkit_macros::api_dto(request)]
#[expect(
    clippy::option_option,
    reason = "None = omitted; Some(None) = clear; Some(Some(_)) = set"
)]
pub struct SkuPatchRequest {
    pub name: Option<String>,
    /// Omitted keeps the category, `null` clears it (P-D-196), a value sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub category_id: Option<Option<Uuid>>,
    pub description: Option<String>,
    pub sellable: Option<bool>,
    #[serde(default, deserialize_with = "double_option")]
    pub gl_code: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub tax_category: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub invoice_line_template: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub billing_timing: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub usage_type_ref: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub unit: Option<Option<String>>,
    pub lifecycle: Option<String>,
    #[serde(rename = "type")]
    pub r#type: Option<String>,
}
impl TryFrom<SkuPatchRequest> for SkuPatch {
    type Error = ValidationReport;
    fn try_from(v: SkuPatchRequest) -> Result<Self, Self::Error> {
        let mut report = ValidationReport::new();
        let mut parse = |value: &str, field: &str| {
            report.violate("VALIDATION", field, format!("unknown {field}: {value}"));
        };
        let kind = v.r#type.as_deref().and_then(|s| {
            let parsed = SkuType::parse(s);
            if parsed.is_none() {
                parse(s, "type");
            }
            parsed
        });
        let lifecycle = v.lifecycle.as_deref().and_then(|s| {
            let parsed = Lifecycle::parse(s);
            if parsed.is_none() {
                parse(s, "lifecycle");
            }
            parsed
        });
        let billing_timing = v.billing_timing.map(|o| {
            o.and_then(|s| {
                let parsed = BillingTiming::parse(&s);
                if parsed.is_none() {
                    parse(&s, "billing_timing");
                }
                parsed
            })
        });
        if !report.is_empty() {
            return Err(report);
        }
        Ok(Self {
            name: v.name.map(|s| s.trim().to_owned()),
            category_id: v.category_id,
            description: v.description,
            sellable: v.sellable,
            gl_code: v.gl_code,
            tax_category: v.tax_category,
            invoice_line_template: v.invoice_line_template,
            billing_timing,
            usage_type_ref: v.usage_type_ref,
            unit: v.unit,
            lifecycle,
            r#type: kind,
        })
    }
}
#[toolkit_macros::api_dto(request)]
pub struct SkuChangeRequest {
    #[serde(flatten)]
    pub patch: SkuPatchRequest,
    #[serde(default, with = "crate::infra::serde_date::option")]
    pub effective_from: Option<Date>,
    /// The submitter's reason: at most 2000 characters (400 `NOTE_TOO_LONG`), stored on the unit
    /// as `submit_note` and on the submit's history row, as sent (P-D-213, P-D-219).
    pub note: Option<String>,
}
/// The optional body of `POST /skus/{id}/submit` and `/retire` (P-D-219): the submitter's note,
/// at most 2000 characters (400 `NOTE_TOO_LONG`), stored on the unit as `submit_note` and on the
/// submit's history row, as sent. No body, `{}` and `note: null` carry none; any other field is a
/// 400.
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct ProductsSkuSubmitRequest {
    #[serde(default)]
    pub note: Option<String>,
}
#[expect(
    clippy::struct_excessive_bools,
    reason = "the wire carries three independent caller flags (P-D-255)"
)]
#[toolkit_macros::api_dto(response)]
pub struct UnitDto {
    pub id: Uuid,
    /// The unit's kind, one products records: a stored unit of another kind is a corrupt row
    /// (500), never served.
    pub kind: ProductsApprovalKind,
    pub ref_type: String,
    pub ref_id: Uuid,
    pub state: ProductsUnitState,
    pub generation: i32,
    pub quorum_required: u32,
    #[serde(with = "crate::infra::serde_date::option")]
    pub common_effective_date: Option<Date>,
    pub submitted_by: Uuid,
    /// The current name of `submitted_by` (P-D-262): its display name, else first and last name, else
    /// username, from Account Management under the caller's rights; "System" for a system actor.
    /// Null when no name is available now, and on a write answer.
    pub submitted_by_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub submitted_at: OffsetDateTime,
    /// The submitter's note, as sent to the submit, change or retire door; null when none was
    /// sent, and on every unit submitted before the note was stored (P-D-219). Not content: the
    /// snapshot and its fingerprint do not carry it.
    pub submit_note: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub decided_at: Option<OffsetDateTime>,
    pub decided_note: Option<String>,
    pub snapshot: serde_json::Value,
    pub decisions: Vec<DecisionDto>,
    pub impact_live: Option<serde_json::Value>,
    /// Whether the caller may approve this unit now (P-D-228, P-D-255): the approval engine's own
    /// rule (`bss_approval::approve_eligibility`, pricing D-459) over the unit's stored items and
    /// its decisions, with the caller as the voter, and the caller's `approval_unit:approve` grant
    /// on this unit. It is false for a decided unit, for its submitter and every author of its
    /// items (the SKU's creator: separation of duties), for a caller who already voted in its
    /// current generation, and for a caller without the grant. It means Approve only: a reject
    /// judges no separation of duties, so the submitter and the SKU's creator may reject a unit
    /// whose flag is false (`caller_can_reject`). Without the grant the vote door is still 403.
    pub caller_can_approve: bool,
    /// Whether the caller may reject this unit now (P-D-255): the approve grant, the unit
    /// pending, and no vote by the caller in this generation. That is what the engine allows.
    pub caller_can_reject: bool,
    /// Whether the caller may withdraw this unit now (P-D-255): the caller submitted it, the
    /// unit is pending, and the caller holds the submit grant the withdraw door asks.
    pub caller_can_withdraw: bool,
}
/// `GET /approval-units/counts` (P-D-227): the units the list's narrowing keeps, by state and by
/// kind, every state and kind named (0 when none), and their total.
#[toolkit_macros::api_dto(response)]
pub struct ProductsApprovalUnitCounts {
    pub by_state: ProductsApprovalUnitStateCounts,
    pub by_kind: ProductsApprovalUnitKindCounts,
    pub total: u64,
}
/// The units in each state (P-D-227).
#[toolkit_macros::api_dto(response)]
#[derive(Default)]
pub struct ProductsApprovalUnitStateCounts {
    pub pending: u64,
    pub approved: u64,
    pub rejected: u64,
    pub withdrawn: u64,
}
/// The units of each kind products records (P-D-227).
#[toolkit_macros::api_dto(response)]
#[derive(Default)]
#[expect(
    clippy::struct_field_names,
    reason = "the fields are the stored kind names, sku_publish, sku_change and sku_retire"
)]
pub struct ProductsApprovalUnitKindCounts {
    pub sku_publish: u64,
    pub sku_change: u64,
    pub sku_retire: u64,
}
#[toolkit_macros::api_dto(response)]
pub struct DecisionDto {
    pub actor: Uuid,
    /// The current name of `actor` (P-D-262): its display name, else first and last name, else
    /// username, from Account Management under the caller's rights; "System" for a system actor.
    /// Null when no name is available now, and on a write answer.
    pub actor_name: Option<String>,
    pub generation: i32,
    pub decision: ProductsDecisionKind,
    pub note: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub stale: bool,
}
/// One page of the unit list (P-D-224): its units, and the toolkit pager's `page_info`, whose
/// `next_cursor` continues it.
#[toolkit_macros::api_dto(response)]
pub struct UnitList {
    pub items: Vec<UnitDto>,
    pub page_info: toolkit_odata::PageInfo,
}
/// `GET /approval-units` (P-D-224, P-D-227, P-D-254): the narrowing, the page and the order. A key
/// the list does not know is 400, as the counts refuse one.
// `$orderby` is the served query name.
#[allow(unknown_lints, de0803_api_snake_case)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct UnitListQuery {
    pub state: Option<String>,
    pub kind: Option<String>,
    pub ref_id: Option<Uuid>,
    /// Page size (P-D-224): 200 by default, clamped at 500.
    pub limit: Option<u64>,
    /// The opaque continuation of a page's `page_info.next_cursor`.
    pub cursor: Option<String>,
    /// `submitted_at asc` (the default, P-D-224) or `submitted_at desc` (P-D-227); the id breaks a
    /// tie in the same direction. A cursor carries its order, so a continuation sends none.
    #[serde(rename = "$orderby")]
    pub orderby: Option<String>,
}
/// `GET /approval-units/counts` (P-D-227): the list's narrowing, and nothing else.
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct UnitCountsQuery {
    pub state: Option<String>,
    pub kind: Option<String>,
    pub ref_id: Option<Uuid>,
}
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct VoteRequest {
    pub generation: i32,
    pub note: Option<String>,
}
#[toolkit_macros::api_dto(response)]
pub struct SubmitReceipt {
    pub applied: bool,
    pub unit: UnitDto,
    pub sku: SkuDto,
}
#[toolkit_macros::api_dto(response)]
pub struct VoteReceipt {
    pub have: Option<u32>,
    pub need: Option<u32>,
    pub outcome: ProductsVoteOutcome,
    pub unit: UnitDto,
}
#[toolkit_macros::api_dto(request)]
pub struct ApprovalPolicyRequest {
    pub kind: Option<String>,
    pub quorum: u32,
}
#[toolkit_macros::api_dto(response)]
pub struct ApprovalPolicyDto {
    pub default_quorum: u32,
    pub overrides: std::collections::BTreeMap<String, u32>,
}
#[cfg(test)]
#[path = "dto_tests.rs"]
mod dto_tests;

impl UnitDto {
    /// The unit as `reader` reads it: its decisions of every generation, and whether `reader` may
    /// approve it, judged by the engine's own predicate over the `authors` of the unit's stored
    /// (current generation) items and its `decisions` (P-D-228). `impact_live` is the caller's to
    /// fill.
    /// # Errors
    /// `CorruptRow` for a kind products does not record.
    pub fn of(
        u: bss_approval::Unit,
        authors: &[Uuid],
        decisions: Vec<bss_approval::Decision>,
        reader: Uuid,
        approve_scope: &toolkit_db::secure::AccessScope,
        submit_scope: &toolkit_db::secure::AccessScope,
    ) -> Result<Self, RepoError> {
        let grant_approve = super::governance::scope_holds(approve_scope, u.tenant_id, u.id);
        let grant_submit = super::governance::scope_holds(submit_scope, u.tenant_id, u.id);
        let engine =
            bss_approval::approve_eligibility(&u, authors.iter().copied(), &decisions, reader)
                .refusal
                .is_none();
        let pending = u.state == bss_approval::UnitState::Pending;
        let voted = bss_approval::already_voted(&u, &decisions, reader);
        Ok(Self {
            id: u.id,
            kind: ProductsApprovalKind::stored(&u.kind, &format_args!("approval unit {}", u.id))?,
            ref_type: u.ref_type,
            ref_id: u.ref_id,
            state: u.state.into(),
            generation: u.generation,
            quorum_required: u.quorum_required,
            common_effective_date: u.common_effective_date,
            submitted_by: u.submitted_by,
            submitted_by_name: None,
            submitted_at: u.submitted_at,
            submit_note: u.submit_note,
            decided_at: u.decided_at,
            decided_note: u.decided_note,
            snapshot: u.snapshot,
            decisions: decisions.into_iter().map(Into::into).collect(),
            impact_live: None,
            caller_can_approve: engine && grant_approve,
            caller_can_reject: grant_approve && pending && !voted,
            caller_can_withdraw: grant_submit && pending && u.submitted_by == reader,
        })
    }
}
impl From<bss_approval::Decision> for DecisionDto {
    fn from(d: bss_approval::Decision) -> Self {
        Self {
            actor: d.actor,
            actor_name: None,
            generation: d.generation,
            decision: d.verdict.into(),
            note: d.note,
            at: d.at,
            stale: d.stale,
        }
    }
}
impl From<bss_approval::Policy> for ApprovalPolicyDto {
    fn from(p: bss_approval::Policy) -> Self {
        Self {
            default_quorum: p.default_quorum,
            overrides: p.overrides,
        }
    }
}
#[toolkit_macros::api_dto(request)]
pub struct ReserveRequest {
    pub owner: String,
    pub kind: String,
    pub ref_id: Uuid,
}
#[toolkit_macros::api_dto(request)]
pub struct ReleaseRequest {
    #[serde(default)]
    pub force: bool,
    pub reason: Option<String>,
}
#[toolkit_macros::api_dto(response)]
pub struct ReferenceReceipt {
    pub reservation_id: Uuid,
    pub sku_id: Uuid,
    pub owner: String,
    pub kind: ProductsReferenceKind,
    pub ref_id: Uuid,
    pub state: ProductsReferenceState,
    pub forced: bool,
}
impl TryFrom<crate::infra::storage::repo::SkuReference> for ReferenceReceipt {
    type Error = RepoError;
    fn try_from(r: crate::infra::storage::repo::SkuReference) -> Result<Self, RepoError> {
        Ok(Self {
            reservation_id: r.id,
            sku_id: r.sku_id,
            owner: r.owner_gear,
            kind: ProductsReferenceKind::stored(
                &r.ref_kind,
                &format_args!("reference {} ref_kind", r.id),
            )?,
            ref_id: r.ref_id,
            state: ProductsReferenceState::stored(
                &r.state,
                &format_args!("reference {} state", r.id),
            )?,
            forced: r.forced,
        })
    }
}

// ------------------------------------------------------- P-D-231: derived usage types

/// One node of a derived usage type's formula, in the shape the SDK's canonical bytes encode it.
/// `op` names the node, and each operator carries its own fields and no other:
/// - `input`: `name`, an input's name;
/// - `const`: `value`, a decimal string;
/// - `add`, `sub`, `mul`: `left` and `right`;
/// - `div_const`: `arg`, and `divisor`, a non-zero decimal string;
/// - `max`, `min`: `args`, at least two;
/// - `ceil`, `floor`: `arg`;
/// - `round`: `arg`, `scale` (0 to 12) and `mode` (`half_even`, `half_up`, `up`, `down`).
///
/// Every token is a plain string (P-D-217), so the door refuses an unknown one with its own code.
#[derive(Debug, Clone, Default)]
#[toolkit_macros::api_dto(request, response)]
pub struct ProductsDerivedExpr {
    pub op: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(no_recursion)]
    pub left: Option<Box<ProductsDerivedExpr>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(no_recursion)]
    pub right: Option<Box<ProductsDerivedExpr>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(no_recursion)]
    pub arg: Option<Box<ProductsDerivedExpr>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(no_recursion)]
    pub args: Option<Vec<ProductsDerivedExpr>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub divisor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}
/// One input of a derived usage type: a raw GTS usage type, folded over one granule.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct ProductsDerivedInput {
    /// `^[a-z][a-z0-9_]{0,31}$`, what the formula's `input` nodes name.
    pub name: String,
    /// A GTS usage type id; never a `products.derived/` id.
    pub usage_type_ref: String,
    /// `sum`, `peak` or `time_weighted`.
    pub granule_fold: String,
    /// Required for `time_weighted` (1 to 86 400 seconds) and refused otherwise; absent when none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_hold_seconds: Option<u32>,
    pub unit: String,
}
/// A derived usage type's declaration (P-D-229, P-D-230): its inputs, the formula applied to one
/// granule's folded inputs, and the output's unit, scale and rounding.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct ProductsDerivedDeclaration {
    /// The selling SKU's unit, e.g. `cloudlet·hour`.
    pub output_unit: String,
    /// `hour`, the one granularity of v1.
    pub granularity: String,
    pub inputs: Vec<ProductsDerivedInput>,
    pub formula: ProductsDerivedExpr,
    /// 0 to 12.
    pub output_scale: u32,
    /// `half_even`, `half_up`, `up` or `down`.
    pub output_round: String,
}
/// `POST /derived-usage-types`: a new type and its version 1.
#[toolkit_macros::api_dto(request)]
pub struct ProductsDerivedUsageTypeRequest {
    /// `^[a-z0-9][a-z0-9._-]{0,63}$`, unique in the tenant.
    pub code: String,
    pub name: String,
    pub declaration: ProductsDerivedDeclaration,
}
/// `POST /derived-usage-types/{code}/versions`: the next version.
#[toolkit_macros::api_dto(request)]
pub struct ProductsDerivedUsageTypeVersionRequest {
    pub declaration: ProductsDerivedDeclaration,
}
/// What a pricing usage policy names as its meter (O-2, H1).
#[toolkit_macros::api_dto(response)]
pub struct ProductsDerivedMeterRef {
    /// `products.derived/<code>@<n>`.
    pub usage_type_id: String,
    /// `<n>`, a canonical decimal.
    pub version: String,
}
/// One version of a derived usage type, with what a pricing author copies into a usage policy.
#[toolkit_macros::api_dto(response)]
pub struct ProductsDerivedUsageTypeVersion {
    /// The type's id.
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub version: u32,
    pub declaration: ProductsDerivedDeclaration,
    /// The stored SHA-256 of the declaration's canonical bytes, 64 lowercase hex digits.
    pub digest: String,
    pub meter_ref: ProductsDerivedMeterRef,
    /// The declaration's `output_unit`.
    pub canonical_unit: String,
    /// `derived-v1:<digest>`.
    pub accrual_policy_version: String,
    pub created_by: Uuid,
    /// The current name of `created_by` (P-D-262): its display name, else first and last name, else
    /// username, from Account Management under the caller's rights; "System" for a system actor.
    /// Null when no name is available now, and on a write answer.
    pub created_by_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}
/// A version as the type read lists it.
#[toolkit_macros::api_dto(response)]
pub struct ProductsDerivedVersionHeader {
    pub version: u32,
    pub digest: String,
    pub meter_ref: ProductsDerivedMeterRef,
    pub accrual_policy_version: String,
    pub created_by: Uuid,
    /// The current name of `created_by` (P-D-262): its display name, else first and last name, else
    /// username, from Account Management under the caller's rights; "System" for a system actor.
    /// Null when no name is available now, and on a write answer.
    pub created_by_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}
/// A derived usage type with its versions' headers, oldest first.
#[toolkit_macros::api_dto(response)]
pub struct ProductsDerivedUsageType {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub created_by: Uuid,
    /// The current name of `created_by` (P-D-262): its display name, else first and last name, else
    /// username, from Account Management under the caller's rights; "System" for a system actor.
    /// Null when no name is available now, and on a write answer.
    pub created_by_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub versions: Vec<ProductsDerivedVersionHeader>,
}
/// A derived usage type as the list pages it.
#[toolkit_macros::api_dto(response)]
pub struct ProductsDerivedUsageTypeItem {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    /// The highest version number. Kept beside [`Self::latest`] (P-D-257).
    pub latest_version: u32,
    /// The latest version, in the version-read shape (P-D-257).
    pub latest: ProductsDerivedUsageTypeVersion,
    pub created_by: Uuid,
    /// The current name of `created_by` (P-D-262): its display name, else first and last name, else
    /// username, from Account Management under the caller's rights; "System" for a system actor.
    /// Null when no name is available now, and on a write answer.
    pub created_by_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// The declaration's tokens, one table each way.
mod derived_tokens {
    use bss_products_sdk::derived::{Granularity, GranuleFold, RoundMode};
    pub const fn granularity(g: Granularity) -> &'static str {
        match g {
            Granularity::Hour => "hour",
        }
    }
    pub fn parse_granularity(token: &str) -> Option<Granularity> {
        [Granularity::Hour]
            .into_iter()
            .find(|g| granularity(*g) == token)
    }
    pub const fn fold(f: GranuleFold) -> &'static str {
        match f {
            GranuleFold::Sum => "sum",
            GranuleFold::Peak => "peak",
            GranuleFold::TimeWeighted => "time_weighted",
        }
    }
    pub fn parse_fold(token: &str) -> Option<GranuleFold> {
        [
            GranuleFold::Sum,
            GranuleFold::Peak,
            GranuleFold::TimeWeighted,
        ]
        .into_iter()
        .find(|f| fold(*f) == token)
    }
    pub const fn round(m: RoundMode) -> &'static str {
        match m {
            RoundMode::HalfEven => "half_even",
            RoundMode::HalfUp => "half_up",
            RoundMode::Up => "up",
            RoundMode::Down => "down",
        }
    }
    pub fn parse_round(token: &str) -> Option<RoundMode> {
        [
            RoundMode::HalfEven,
            RoundMode::HalfUp,
            RoundMode::Up,
            RoundMode::Down,
        ]
        .into_iter()
        .find(|m| round(*m) == token)
    }
}

/// The shape rules of the wire declaration, before the SDK's: an unknown token, a decimal that does
/// not parse, or a node whose fields are not its operator's is 400 `DERIVED_DECLARATION_INVALID`,
/// naming the rule (`unknown_granularity`, `unknown_fold`, `unknown_round_mode`,
/// `unknown_operator`, `invalid_decimal`, `malformed_expression`).
impl TryFrom<&ProductsDerivedDeclaration> for bss_products_sdk::derived::DerivedUsageDeclaration {
    type Error = crate::domain::error::DomainError;
    fn try_from(d: &ProductsDerivedDeclaration) -> Result<Self, Self::Error> {
        use crate::domain::derived::declaration_invalid;
        let granularity = derived_tokens::parse_granularity(&d.granularity).ok_or_else(|| {
            declaration_invalid(
                "unknown_granularity",
                format!("granularity `{}` is not `hour`", d.granularity),
            )
        })?;
        let output_round = derived_tokens::parse_round(&d.output_round).ok_or_else(|| {
            declaration_invalid(
                "unknown_round_mode",
                format!(
                    "output_round `{}` is not half_even, half_up, up or down",
                    d.output_round
                ),
            )
        })?;
        let inputs = d
            .inputs
            .iter()
            .map(|i| {
                Ok(bss_products_sdk::derived::DerivedInput {
                    name: i.name.clone(),
                    usage_type_ref: i.usage_type_ref.clone(),
                    granule_fold: derived_tokens::parse_fold(&i.granule_fold).ok_or_else(|| {
                        declaration_invalid(
                            "unknown_fold",
                            format!(
                                "input `{}` granule_fold `{}` is not sum, peak or time_weighted",
                                i.name, i.granule_fold
                            ),
                        )
                    })?,
                    max_hold_seconds: i.max_hold_seconds,
                    unit: i.unit.clone(),
                })
            })
            .collect::<Result<_, Self::Error>>()?;
        Ok(Self {
            output_unit: d.output_unit.clone(),
            granularity,
            inputs,
            formula: d.formula.parse("formula")?,
            output_scale: d.output_scale,
            output_round,
        })
    }
}

impl ProductsDerivedExpr {
    /// The fields this node carries, by name.
    fn present(&self) -> Vec<&'static str> {
        [
            ("name", self.name.is_some()),
            ("value", self.value.is_some()),
            ("left", self.left.is_some()),
            ("right", self.right.is_some()),
            ("arg", self.arg.is_some()),
            ("args", self.args.is_some()),
            ("divisor", self.divisor.is_some()),
            ("scale", self.scale.is_some()),
            ("mode", self.mode.is_some()),
        ]
        .into_iter()
        .filter_map(|(field, here)| here.then_some(field))
        .collect()
    }

    /// The SDK node at `path`: its fields exactly its operator's, its operands parsed in turn.
    fn parse(
        &self,
        path: &str,
    ) -> Result<bss_products_sdk::derived::Expr, crate::domain::error::DomainError> {
        use crate::domain::derived::declaration_invalid;
        use bss_products_sdk::derived::Expr;
        let fields: &[&str] = match self.op.as_str() {
            "input" => &["name"],
            "const" => &["value"],
            "add" | "sub" | "mul" => &["left", "right"],
            "div_const" => &["arg", "divisor"],
            "max" | "min" => &["args"],
            "ceil" | "floor" => &["arg"],
            "round" => &["arg", "scale", "mode"],
            other => {
                return Err(declaration_invalid(
                    "unknown_operator",
                    format!("{path}: `{other}` is not an operator"),
                ));
            }
        };
        if self.present() != fields {
            return Err(declaration_invalid(
                "malformed_expression",
                format!(
                    "{path}: `{}` carries {}, and no other field",
                    self.op,
                    fields.join(", ")
                ),
            ));
        }
        let decimal = |text: &Option<String>, field: &str| {
            let text = text.as_deref().unwrap_or_default();
            text.parse::<rust_decimal::Decimal>().map_err(|err| {
                declaration_invalid(
                    "invalid_decimal",
                    format!("{path}.{field}: `{text}` is not a decimal: {err}"),
                )
            })
        };
        let operand = |slot: &Option<Box<Self>>, field: &str| {
            let node = slot.as_deref().ok_or_else(|| {
                declaration_invalid(
                    "malformed_expression",
                    format!("{path}: `{}` needs `{field}`", self.op),
                )
            })?;
            node.parse(&format!("{path}.{field}")).map(Box::new)
        };
        Ok(match self.op.as_str() {
            "input" => Expr::Input(self.name.clone().unwrap_or_default()),
            "const" => Expr::Const(decimal(&self.value, "value")?),
            "add" => Expr::Add(operand(&self.left, "left")?, operand(&self.right, "right")?),
            "sub" => Expr::Sub(operand(&self.left, "left")?, operand(&self.right, "right")?),
            "mul" => Expr::Mul(operand(&self.left, "left")?, operand(&self.right, "right")?),
            "div_const" => Expr::DivConst(
                operand(&self.arg, "arg")?,
                decimal(&self.divisor, "divisor")?,
            ),
            "ceil" => Expr::Ceil(operand(&self.arg, "arg")?),
            "floor" => Expr::Floor(operand(&self.arg, "arg")?),
            "round" => {
                let mode = self.mode.as_deref().unwrap_or_default();
                Expr::Round(
                    operand(&self.arg, "arg")?,
                    self.scale.unwrap_or_default(),
                    derived_tokens::parse_round(mode).ok_or_else(|| {
                        declaration_invalid(
                            "unknown_round_mode",
                            format!("{path}.mode: `{mode}` is not half_even, half_up, up or down"),
                        )
                    })?,
                )
            }
            many => {
                let args = self
                    .args
                    .iter()
                    .flatten()
                    .enumerate()
                    .map(|(i, e)| e.parse(&format!("{path}.args.{i}")))
                    .collect::<Result<Vec<_>, _>>()?;
                if many == "max" {
                    Expr::Max(args)
                } else {
                    Expr::Min(args)
                }
            }
        })
    }
}

impl From<&bss_products_sdk::derived::DerivedUsageDeclaration> for ProductsDerivedDeclaration {
    /// The declaration as the doors store and serve it: decimals normalized, every token the
    /// table's.
    fn from(d: &bss_products_sdk::derived::DerivedUsageDeclaration) -> Self {
        Self {
            output_unit: d.output_unit.clone(),
            granularity: derived_tokens::granularity(d.granularity).to_owned(),
            inputs: d
                .inputs
                .iter()
                .map(|i| ProductsDerivedInput {
                    name: i.name.clone(),
                    usage_type_ref: i.usage_type_ref.clone(),
                    granule_fold: derived_tokens::fold(i.granule_fold).to_owned(),
                    max_hold_seconds: i.max_hold_seconds,
                    unit: i.unit.clone(),
                })
                .collect(),
            formula: (&d.formula).into(),
            output_scale: d.output_scale,
            output_round: derived_tokens::round(d.output_round).to_owned(),
        }
    }
}

impl From<&bss_products_sdk::derived::Expr> for ProductsDerivedExpr {
    fn from(e: &bss_products_sdk::derived::Expr) -> Self {
        use bss_products_sdk::derived::Expr;
        let node = |op: &str| Self {
            op: op.to_owned(),
            name: None,
            value: None,
            left: None,
            right: None,
            arg: None,
            args: None,
            divisor: None,
            scale: None,
            mode: None,
        };
        let boxed = |e: &Expr| Some(Box::new(Self::from(e)));
        let decimal = |d: &rust_decimal::Decimal| Some(d.normalize().to_string());
        match e {
            Expr::Input(name) => Self {
                name: Some(name.clone()),
                ..node("input")
            },
            Expr::Const(value) => Self {
                value: decimal(value),
                ..node("const")
            },
            Expr::Add(l, r) => Self {
                left: boxed(l),
                right: boxed(r),
                ..node("add")
            },
            Expr::Sub(l, r) => Self {
                left: boxed(l),
                right: boxed(r),
                ..node("sub")
            },
            Expr::Mul(l, r) => Self {
                left: boxed(l),
                right: boxed(r),
                ..node("mul")
            },
            Expr::DivConst(arg, divisor) => Self {
                arg: boxed(arg),
                divisor: decimal(divisor),
                ..node("div_const")
            },
            Expr::Max(args) => Self {
                args: Some(args.iter().map(Self::from).collect()),
                ..node("max")
            },
            Expr::Min(args) => Self {
                args: Some(args.iter().map(Self::from).collect()),
                ..node("min")
            },
            Expr::Ceil(arg) => Self {
                arg: boxed(arg),
                ..node("ceil")
            },
            Expr::Floor(arg) => Self {
                arg: boxed(arg),
                ..node("floor")
            },
            Expr::Round(arg, scale, mode) => Self {
                arg: boxed(arg),
                scale: Some(*scale),
                mode: Some(derived_tokens::round(*mode).to_owned()),
                ..node("round")
            },
        }
    }
}
