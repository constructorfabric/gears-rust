//! Pricing authoring wire contracts, with unique `OpenAPI` names and `snake_case` fields. A closed
//! set on a response is its `enum` (D-439); a request keeps `string`, so its door's code refuses.
use crate::api::rest::closed_sets::{
    PricingApprovalKind, PricingBillingTiming, PricingChangeKind, PricingChargeKind,
    PricingDecisionKind, PricingEligibility, PricingEntryReferenceState, PricingItemReferenceState,
    PricingModel, PricingPeriod, PricingPlanChange, PricingPriceState, PricingPriceStatus,
    PricingReferenceOpKind, PricingReferenceOpReason, PricingReferenceOpRefKind,
    PricingReferenceOpState, PricingRevisionState, PricingSkuEntryStatus, PricingUnitState,
    PricingVoteOutcome,
};
use crate::domain::plan::{self, EffectiveRevision};
use crate::infra::plan_revisions::{effective_revisions, stored_revisions};
use crate::infra::storage::{RepoError, entity, repo::approval_repo::UnitInstants};
use std::collections::BTreeMap;
use uuid::Uuid;

#[toolkit_macros::api_dto(response)]
pub struct PriceBookDto {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub code: String,
    pub name: String,
    pub currency: String,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    /// Free text, at most 2000 characters, or `null` (D-444).
    pub description: Option<String>,
    pub version: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
    /// When the book was archived (D-522); null while it is not. The book list hides it unless
    /// asked `archived eq true`; a read by id, the export and the consumer reads ignore the mark.
    #[serde(with = "time::serde::rfc3339::option")]
    pub archived_at: Option<time::OffsetDateTime>,
    /// Who archived it; null while it is not archived.
    pub archived_by: Option<Uuid>,
    /// The current name of `archived_by` (D-519); null when no name is available now, and on a
    /// write answer.
    pub archived_by_name: Option<String>,
}
impl From<entity::price_book::Model> for PriceBookDto {
    fn from(m: entity::price_book::Model) -> Self {
        Self {
            id: m.id,
            tenant_id: m.tenant_id,
            code: m.code,
            name: m.name,
            currency: m.currency,
            valid_from: m.valid_from.map(|v| v.to_string()),
            valid_until: m.valid_until.map(|v| v.to_string()),
            description: m.description,
            version: m.version,
            created_at: m.created_at,
            updated_at: m.updated_at,
            archived_at: m.archived_at,
            archived_by: m.archived_by,
            archived_by_name: None,
        }
    }
}
/// `POST /price-books/{id}/unarchive` (D-522): the book, listed again, and the entries still
/// `released` after the door drove their re-reservations: their SKU refused a new reservation
/// (retired, say), or Products has not answered yet. Each stays read-only until it is re-reserved.
#[toolkit_macros::api_dto(response)]
pub struct PricingPriceBookUnarchiveDto {
    #[serde(flatten)]
    pub book: PriceBookDto,
    /// The entries still `released` when the answer is built; null when they could not be read
    /// after the unarchive committed (the book's entry list says which).
    pub released_entries: Option<Vec<Uuid>>,
}
#[toolkit_macros::api_dto(response)]
pub struct PricingPriceBookEntryDto {
    /// Immutable policy materialized from the entry; null for legacy/non-usage entries.
    pub usage_rating_policy: Option<crate::infra::usage_policy_wire::UsageRatingPolicy>,
    /// The SKU head's `published_version` when the meter was checked. Null for a non-usage entry
    /// and for a usage entry written before D-514.
    pub usage_sku_version: Option<i64>,
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub book_id: Uuid,
    pub sku_id: Uuid,
    pub charge_kind: PricingChargeKind,
    pub period: Option<PricingPeriod>,
    /// The entry's model (D-427), fixed for its life and part of its key.
    pub model: PricingModel,
    pub dimension_key: Option<String>,
    pub invoice_line_override: Option<String>,
    pub reservation_id: Uuid,
    pub reference_state: PricingEntryReferenceState,
    pub version: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
}
impl PricingPriceBookEntryDto {
    /// The entry and its policy, together. A non-usage entry passes `None`.
    /// # Errors
    /// A stored token outside its closed set is a corrupt row (D-439).
    pub fn from_stored(
        m: entity::price_book_entry::Model,
        usage_rating_policy: Option<crate::infra::usage_policy_wire::UsageRatingPolicy>,
    ) -> Result<Self, RepoError> {
        let id = m.id;
        Ok(Self {
            usage_rating_policy,
            usage_sku_version: m.usage_sku_version,
            id,
            tenant_id: m.tenant_id,
            book_id: m.book_id,
            sku_id: m.sku_id,
            charge_kind: PricingChargeKind::stored(
                &m.charge_kind,
                &format_args!("entry {id} charge_kind"),
            )?,
            period: m
                .period
                .as_deref()
                .map(|p| PricingPeriod::stored(p, &format_args!("entry {id} period")))
                .transpose()?,
            model: PricingModel::stored(&m.model, &format_args!("entry {id} model"))?,
            dimension_key: m.dimension_key,
            invoice_line_override: m.invoice_line_override,
            reservation_id: m.reservation_id,
            reference_state: PricingEntryReferenceState::stored(
                &m.reference_state,
                &format_args!("entry {id} reference_state"),
            )?,
            version: m.version,
            created_at: m.created_at,
            updated_at: m.updated_at,
        })
    }
}
impl TryFrom<entity::price_book_entry::Model> for PricingPriceBookEntryDto {
    type Error = RepoError;
    fn try_from(m: entity::price_book_entry::Model) -> Result<Self, RepoError> {
        Self::from_stored(m, None)
    }
}
impl PricingPriceBookEntryDto {
    /// Materialize immutable policy content along with an entry.
    /// # Errors
    /// Refuses corrupt or dangling policy references and storage failures.
    pub async fn load(
        tx: &impl toolkit_db::secure::DBRunner,
        m: entity::price_book_entry::Model,
    ) -> Result<Self, RepoError> {
        let policy = crate::infra::storage::repo::usage_policy_repo::for_entries(
            tx,
            m.tenant_id,
            std::slice::from_ref(&m),
        )
        .await?
        .remove(&m.id);
        Self::from_stored(m, policy)
    }
}
/// Why rendering an entry body for a durable replay failed.
pub(crate) enum EntryJsonError {
    /// The entry or its policy could not be read.
    Storage(RepoError),
    /// The served entry did not serialize.
    Serialize,
}
/// The JSON a create stores for replay: the same body `PricingPriceBookEntryDto` serves.
///
/// # Errors
/// A storage failure is [`EntryJsonError::Storage`]. A serialize failure is
/// [`EntryJsonError::Serialize`].
pub(crate) async fn price_book_entry_json(
    tx: &impl toolkit_db::secure::DBRunner,
    model: entity::price_book_entry::Model,
) -> Result<String, EntryJsonError> {
    let dto = PricingPriceBookEntryDto::load(tx, model)
        .await
        .map_err(EntryJsonError::Storage)?;
    serde_json::to_string(&dto).map_err(|_| EntryJsonError::Serialize)
}
/// An entry's prices by state; a rejected price is not counted (D-428). The approved ones are
/// also counted by where their window stands on the day of the read (D-440): today, or the
/// `as_of` of the book's entries list (D-473). `approved` = `scheduled + active + superseded`.
#[toolkit_macros::api_dto(response)]
pub struct PricingEntryPriceCounts {
    pub approved: u64,
    pub pending: u64,
    pub draft: u64,
    /// Approved prices that start after the day.
    pub scheduled: u64,
    /// Approved prices in force on the day.
    pub active: u64,
    /// Approved prices whose window ended on or before the day.
    pub superseded: u64,
}
/// An entry's usage (D-428): its prices by state; `plans`, the distinct plans with a draft,
/// pending, scheduled or published revision whose items name it; `plans_superseded_only`, the
/// distinct plans that name it only through superseded revisions (they still keep it
/// `ENTRY_IN_USE`).
#[toolkit_macros::api_dto(response)]
pub struct PricingEntryUsage {
    pub prices: PricingEntryPriceCounts,
    pub plans: u64,
    pub plans_superseded_only: u64,
}
impl From<crate::infra::usage::EntryUsage> for PricingEntryUsage {
    fn from(u: crate::infra::usage::EntryUsage) -> Self {
        Self {
            prices: PricingEntryPriceCounts {
                approved: u.prices.approved,
                pending: u.prices.pending,
                draft: u.prices.draft,
                scheduled: u.prices.scheduled,
                active: u.prices.active,
                superseded: u.prices.superseded,
            },
            plans: u.plans,
            plans_superseded_only: u.plans_superseded_only,
        }
    }
}
/// What the two entry reads answer (D-428): the entry's fields, its `usage`, its `current_price`
/// (D-440): the default chain's approved price in force on the day, as D-434 chooses and shows it
/// — and its `next_price` (D-472): the default chain's earliest scheduled price, else its newest
/// draft or pending price (the highest `version_no`, then the latest `created_at`). Each is `null`
/// when there is none, or when the caller does not hold `price_book` read on the entry's book.
/// The day is today, or the `as_of` of the book's entries list (D-473).
/// Every other answer that carries an entry (POST, PATCH, the stored receipt, the export,
/// publish-changes) keeps [`PricingPriceBookEntryDto`].
#[toolkit_macros::api_dto(response)]
pub struct PricingPriceBookEntryReadDto {
    #[serde(flatten)]
    pub entry: PricingPriceBookEntryDto,
    pub usage: PricingEntryUsage,
    pub current_price: Option<PricingPriceDto>,
    pub next_price: Option<PricingPriceDto>,
}
impl PricingPriceBookEntryReadDto {
    /// # Errors
    /// `CorruptRow` for a stored token outside its closed set (D-439).
    pub fn of(
        m: entity::price_book_entry::Model,
        usage: crate::infra::usage::EntryUsage,
        current_price: Option<PricingPriceDto>,
        next_price: Option<PricingPriceDto>,
        policy: Option<crate::infra::usage_policy_wire::UsageRatingPolicy>,
    ) -> Result<Self, RepoError> {
        Ok(Self {
            entry: PricingPriceBookEntryDto::from_stored(m, policy)?,
            usage: usage.into(),
            current_price,
            next_price,
        })
    }
}
/// `GET /price-book-entries/{id}/prices` (D-440): every price of the entry in every state, the
/// default chain first, then each dimension value's chain in ascending order; each chain by
/// `effective_from`, then `version_no`.
#[toolkit_macros::api_dto(response)]
pub struct PricingEntryPriceList {
    pub items: Vec<PricingPriceDto>,
}
/// The query of `GET /price-book-entries/{id}/prices`: an optional `status`, one display status
/// or several comma-separated.
#[derive(Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PricingEntryPricesQuery {
    pub status: Option<String>,
}
/// A book's prices by state — a rejected price included — and its approved prices by where their
/// window stands today: `approved` = `scheduled + active + superseded` (D-441).
#[toolkit_macros::api_dto(response)]
pub struct PricingBookPriceCounts {
    pub draft: u64,
    pub pending: u64,
    pub approved: u64,
    pub scheduled: u64,
    pub active: u64,
    pub superseded: u64,
    pub rejected: u64,
}
/// A book's stats (D-441): its entries and their distinct SKUs; the distinct plans with a draft,
/// pending, scheduled or published revision on the book (`plans`), and those that name it only
/// through superseded revisions (`plans_superseded_only`, what the delete refuses as
/// `BOOK_IN_PLAN_HISTORY`); its prices by state; its `prices` units in review; and the latest
/// change of the book, its entries, their prices and its units. `DELETE /price-books/{id}`
/// succeeds exactly when `entries`, `plans` and `plans_superseded_only` are 0 (D-444).
#[toolkit_macros::api_dto(response)]
pub struct PricingPriceBookStats {
    pub entries: u64,
    pub skus: u64,
    pub plans: u64,
    pub plans_superseded_only: u64,
    pub prices: PricingBookPriceCounts,
    pub pending_units: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub last_change_at: time::OffsetDateTime,
}
impl From<crate::infra::book_stats::BookStats> for PricingPriceBookStats {
    fn from(s: crate::infra::book_stats::BookStats) -> Self {
        let p = s.prices;
        Self {
            entries: s.entries,
            skus: s.skus,
            plans: s.plans,
            plans_superseded_only: s.plans_superseded_only,
            prices: PricingBookPriceCounts {
                draft: p.draft,
                pending: p.pending,
                approved: p.approved,
                scheduled: p.scheduled,
                active: p.active,
                superseded: p.superseded,
                rejected: p.rejected,
            },
            pending_units: s.pending_units,
            last_change_at: s.last_change_at,
        }
    }
}
/// What the two book reads answer (D-441): the book's fields and its `stats`. The write answers
/// (POST, PATCH, the stored receipt), the export and publish-changes keep [`PriceBookDto`].
#[toolkit_macros::api_dto(response)]
pub struct PricingPriceBookReadDto {
    #[serde(flatten)]
    pub book: PriceBookDto,
    pub stats: PricingPriceBookStats,
}
/// One entry of a SKU as `GET /price-book-entries?sku_id=` answers it (D-434, D-486): the entry,
/// its book's code, name and currency, its usage (D-428), its `status` and `changing` on today,
/// the default chain's price in force today (`current_price`) and its `next_price` (D-472),
/// chosen as the entry reads choose it (the highest `version_no` orders the drafts and pending
/// prices). Each price is `null` when there is none, or when the caller does not hold
/// `price_book` read (the export's grant). `status` and `changing` are not money.
#[toolkit_macros::api_dto(response)]
pub struct PricingSkuEntryDto {
    #[serde(flatten)]
    pub entry: PricingPriceBookEntryDto,
    pub book_code: String,
    pub book_name: String,
    pub currency: String,
    pub usage: PricingEntryUsage,
    /// `priced` when an approved price is in force today, else `scheduled` when one starts
    /// later, else `unpriced` (D-486).
    pub status: PricingSkuEntryStatus,
    /// A draft or a pending price exists (D-486).
    pub changing: bool,
    pub current_price: Option<PricingPriceDto>,
    pub next_price: Option<PricingPriceDto>,
}
/// `GET /price-book-entries?sku_id=` (D-486): one page of the SKU's entries.
#[toolkit_macros::api_dto(response)]
pub struct PricingSkuEntryList {
    pub items: Vec<PricingSkuEntryDto>,
    /// `limit` is the page size. `next_cursor` continues the page.
    pub page_info: toolkit_odata::PageInfo,
}
#[toolkit_macros::api_dto(response)]
pub struct PricingPriceDto {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub price_book_entry_id: Uuid,
    pub version_no: i32,
    pub dim_value: Option<String>,
    /// The entry's model, read-only (D-427): a price has no model of its own.
    pub model: PricingModel,
    pub price_json: serde_json::Value,
    pub min_fee: Option<String>,
    pub eligibility: PricingEligibility,
    pub effective_from: String,
    pub effective_to: Option<String>,
    pub keep_for_bound: bool,
    pub closed_explicitly: bool,
    pub temporary_until: Option<String>,
    pub paired_price_id: Option<Uuid>,
    pub return_of_price_id: Option<Uuid>,
    pub state: PricingPriceState,
    /// `set` for a price; `cancel` or `end` for a row that asks to cancel or end the approved
    /// price `target_price_id` names (D-520, D-521). Such a row carries that price's money
    /// unchanged and is never a price in force.
    pub change_kind: PricingChangeKind,
    /// The approved price a `cancel` or `end` row names; null on a `set` row.
    pub target_price_id: Option<Uuid>,
    /// The unit that cancelled this price, when `state` is `cancelled` (D-520).
    pub cancelled_by_unit_id: Option<Uuid>,
    /// Display state of matrix row 10: an approved price shows where its window stands today.
    /// A cancelled price shows `cancelled` (D-520). A `cancel` or `end` row shows its state, and
    /// `superseded` once applied (D-520, D-521).
    pub status: PricingPriceStatus,
    pub pending_unit_id: Option<Uuid>,
    pub approved_by_unit_id: Option<Uuid>,
    pub note: Option<String>,
    pub created_by: Uuid,
    /// The current name of `created_by` (D-519): its display name, else first and last name, else
    /// username, from Account Management under the caller's rights; "System" for a system actor.
    /// Null when no name is available now, and on a write answer.
    pub created_by_name: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub approved_at: Option<time::OffsetDateTime>,
    pub version: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
}
impl PricingPriceDto {
    /// A stored price with its entry's model (D-427).
    /// # Errors
    /// `CorruptRow` for a stored token outside its closed set (D-439).
    pub fn of(m: entity::price::Model, model: &str) -> Result<Self, RepoError> {
        Self::at(m, model, time::OffsetDateTime::now_utc().date())
    }
    /// [`Self::of`] with its display status on `today`, the one day a whole read is dated on
    /// (D-440).
    /// # Errors
    /// `CorruptRow` for a stored token outside its closed set (D-439).
    pub fn at(m: entity::price::Model, model: &str, today: time::Date) -> Result<Self, RepoError> {
        let id = m.id;
        let state = PricingPriceState::stored(&m.state, &format_args!("price {id} state"))?;
        let change_kind =
            PricingChangeKind::stored(&m.change_kind, &format_args!("price {id} change_kind"))?;
        // D-520, D-521: a cancel or an end has no window of its own.
        let status = if change_kind == PricingChangeKind::Set {
            crate::domain::price::window_display(
                state.into(),
                m.effective_from,
                m.effective_to,
                today,
            )
        } else {
            crate::domain::price::change_display(state.into())
        }
        .into();
        Ok(Self {
            id,
            tenant_id: m.tenant_id,
            price_book_entry_id: m.price_book_entry_id,
            version_no: m.version_no,
            dim_value: m.dim_value,
            model: PricingModel::stored(model, &format_args!("price {id} model"))?,
            price_json: m.price_json,
            min_fee: m.min_fee,
            eligibility: PricingEligibility::stored(
                &m.eligibility,
                &format_args!("price {id} eligibility"),
            )?,
            effective_from: m.effective_from.to_string(),
            effective_to: m.effective_to.map(|v| v.to_string()),
            keep_for_bound: m.keep_for_bound,
            closed_explicitly: m.closed_explicitly,
            temporary_until: m.temporary_until.map(|v| v.to_string()),
            paired_price_id: m.paired_price_id,
            return_of_price_id: m.return_of_price_id,
            state,
            change_kind,
            target_price_id: m.target_price_id,
            cancelled_by_unit_id: m.cancelled_by_unit_id,
            status,
            pending_unit_id: m.pending_unit_id,
            approved_by_unit_id: m.approved_by_unit_id,
            note: m.note,
            created_by: m.created_by,
            created_by_name: None,
            approved_at: m.approved_at,
            version: m.version,
            created_at: m.created_at,
            updated_at: m.updated_at,
        })
    }
}

#[toolkit_macros::api_dto(request)]
#[derive(Clone)]
#[serde(deny_unknown_fields)]
pub struct PriceBookCreate {
    pub code: String,
    pub name: String,
    pub currency: String,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    /// Free text, at most 2000 characters (D-444).
    pub description: Option<String>,
}
#[toolkit_macros::api_dto(request)]
#[derive(Clone)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::option_option,
    reason = "PATCH distinguishes omission, null clearing, and a new date"
)]
pub struct PriceBookPatch {
    pub name: Option<String>,
    #[serde(default, deserialize_with = "nullable_date")]
    pub valid_from: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable_date")]
    pub valid_until: Option<Option<String>>,
    /// Omitted keeps the description, `null` clears it; at most 2000 characters (D-444).
    #[serde(default, deserialize_with = "nullable_date")]
    pub description: Option<Option<String>>,
}
#[allow(
    clippy::option_option,
    reason = "PATCH distinguishes omission, null clearing, and a new date"
)]
fn nullable_date<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<String>>, D::Error> {
    <Option<String> as serde::Deserialize>::deserialize(d).map(Some)
}
/// One page of a book's entries (D-483), ordered `(sku_id, charge_kind, model, id)`.
#[toolkit_macros::api_dto(response)]
pub struct PricingPriceBookEntryList {
    pub items: Vec<PricingPriceBookEntryReadDto>,
    /// `limit` is the page size (500 by default and at most); `next_cursor` continues the page and
    /// carries the list's `$filter` and day.
    pub page_info: toolkit_odata::PageInfo,
}
#[toolkit_macros::api_dto(response)]
pub struct PricingExportEntry {
    pub entry: PricingPriceBookEntryDto,
    pub prices: Vec<PricingPriceDto>,
}
#[toolkit_macros::api_dto(response)]
pub struct PriceBookExport {
    pub book: PriceBookDto,
    pub entries: Vec<PricingExportEntry>,
}
/// One key of `PUT /dimension-keys`: the key and its whole list of values (a full replace).
#[toolkit_macros::api_dto(request)]
#[derive(Clone)]
#[serde(deny_unknown_fields)]
pub struct PricingDimensionEntry {
    pub key: String,
    pub values: Vec<String>,
}
/// `PUT /dimension-keys`: the tenant's whole registry.
#[toolkit_macros::api_dto(request)]
#[derive(Clone)]
#[serde(deny_unknown_fields)]
pub struct PricingDimensions {
    pub items: Vec<PricingDimensionEntry>,
}
/// `PATCH /dimension-keys` (D-436): the values of ONE declared key to add and to remove; keys
/// themselves are added and removed by the PUT.
#[toolkit_macros::api_dto(request)]
#[derive(Clone)]
#[serde(deny_unknown_fields)]
pub struct PricingDimensionKeyPatch {
    pub key: String,
    #[serde(default)]
    pub add: Vec<String>,
    #[serde(default)]
    pub remove: Vec<String>,
}
/// What uses one dimension value (D-436): the prices, of any state, whose entry names the key
/// and whose chain is the value. A value with prices is not removed (`DIM_VALUE_IN_USE`).
#[toolkit_macros::api_dto(response)]
pub struct PricingDimensionValueUsage {
    pub prices: u64,
}
/// One value of a key with its use.
#[toolkit_macros::api_dto(response)]
pub struct PricingDimensionValue {
    pub value: String,
    pub usage: PricingDimensionValueUsage,
}
/// One key of the registry as the reads and writes answer it.
#[toolkit_macros::api_dto(response)]
pub struct PricingDimensionKey {
    pub key: String,
    pub values: Vec<PricingDimensionValue>,
}
/// The registry as `GET`, `PUT` and `PATCH /dimension-keys` answer it (D-436).
#[toolkit_macros::api_dto(response)]
pub struct PricingDimensionRegistry {
    pub items: Vec<PricingDimensionKey>,
}
#[toolkit_macros::api_dto(request)]
#[derive(Clone)]
#[serde(deny_unknown_fields)]
pub struct PricingSettingsPut {
    pub default_timing: String,
    /// `half_up`, `half_even`, `half_down`, `up` or `down` (D-437).
    pub default_rounding: String,
    pub default_gl: Option<String>,
    pub default_tax_category: Option<String>,
    pub invoice_line_templates: std::collections::BTreeMap<String, String>,
    /// Required (D-438): the currencies a NEW book may take, a full replace; `[]` is any.
    pub currencies: Vec<String>,
    /// Ignored (D-519): the writer's name that `GET /settings` shows. It is accepted so that a
    /// read's body without `version`, `updated_at` and `updated_by` stays a PUT body (D-438).
    #[serde(default)]
    pub updated_by_name: Option<String>,
}
#[toolkit_macros::api_dto(response)]
pub struct PricingSettingsDto {
    pub default_timing: PricingBillingTiming,
    /// One of the five modes (D-437) once written through the door; a string, not an enum: no
    /// CHECK guards the column, and a legacy value reads back as stored (D-439).
    pub default_rounding: String,
    pub default_gl: Option<String>,
    pub default_tax_category: Option<String>,
    pub invoice_line_templates: serde_json::Value,
    /// The currencies a new book may take; empty means any (D-438).
    pub currencies: Vec<String>,
    pub version: i64,
    /// When the settings were last written; null before the first write (version 0).
    #[serde(with = "time::serde::rfc3339::option")]
    pub updated_at: Option<time::OffsetDateTime>,
    /// Who wrote them last; null before the first write and on a row written before D-438.
    pub updated_by: Option<Uuid>,
    /// The current name of `updated_by` (D-519): its display name, else first and last name, else
    /// username, from Account Management under the caller's rights; "System" for a system actor.
    /// Null when `updated_by` is null, when no name is available now, and on a write answer.
    pub updated_by_name: Option<String>,
}

#[toolkit_macros::api_dto(request)]
#[derive(Debug, Clone, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct PricingPriceBookEntryCreate {
    /// Required for usage entries; immutable after creation. Identity is server assigned.
    /// `quantity_semantics.fold`, `reset` and `partial_window` may be absent or null; the server
    /// fills `SUM`, `rating_window_start` and `actual_quantity_full_thresholds` (D-513).
    pub usage_rating_policy: Option<crate::infra::usage_policy_wire::UsageRatingPolicyRequest>,
    pub sku_id: Uuid,
    /// Required and fixed for the entry's life (D-427): `flat`, `per_unit`, `graduated`,
    /// `volume` or `package`, one the SKU's charge kind allows.
    pub model: String,
    pub period: Option<String>,
    pub dimension_key: Option<String>,
    pub invoice_line_override: Option<String>,
}
#[toolkit_macros::api_dto(request)]
#[derive(Clone)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::option_option,
    reason = "PATCH distinguishes omission and clearing"
)]
pub struct PricingPriceBookEntryPatch {
    #[serde(default, deserialize_with = "nullable_date")]
    pub dimension_key: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable_date")]
    pub invoice_line_override: Option<Option<String>>,
}

/// `POST /plan-revisions/{id}/items`: one item, one op with its own key (D-407). A plan item is a
/// SKU and, once the author has chosen it, its entry in the plan's book (D-467, D-512). The entry
/// may be absent: the draft holds the SKU and a later PATCH sets the entry. Submit still needs an
/// entry for every item. `treatment`, `included_qty` and `qty_min` are refused (400
/// `BODY_UNEXPECTED`).
#[toolkit_macros::api_dto(request)]
#[derive(Debug, Clone, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct PricingPlanItemCreate {
    pub sku_id: Uuid,
    /// The entry of the plan's book that prices the SKU. Absent or null adds an entry-less item
    /// (D-512). A given entry is still judged: another book, another SKU, or an unknown entry.
    #[serde(default)]
    pub price_book_entry_id: Option<Uuid>,
}
#[toolkit_macros::api_dto(response)]
pub struct PricingPlanItemDto {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub revision_id: Uuid,
    pub sku_id: Uuid,
    /// The entry that prices the item. Null while a draft item waits for its entry (D-512), and
    /// for a legacy item stored without one (D-467).
    pub price_book_entry_id: Option<Uuid>,
    /// None until a reserve answers: a copied item attaches after its write (D-413).
    pub reservation_id: Option<Uuid>,
    pub reference_state: PricingItemReferenceState,
    pub version: i64,
    pub created_by: Uuid,
    /// The current name of `created_by` (D-519): its display name, else first and last name, else
    /// username, from Account Management under the caller's rights; "System" for a system actor.
    /// Null when no name is available now, and on a write answer.
    pub created_by_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
}
impl TryFrom<entity::plan_item::Model> for PricingPlanItemDto {
    type Error = RepoError;
    fn try_from(m: entity::plan_item::Model) -> Result<Self, RepoError> {
        let id = m.id;
        Ok(Self {
            id,
            tenant_id: m.tenant_id,
            revision_id: m.revision_id,
            sku_id: m.sku_id,
            price_book_entry_id: m.price_book_entry_id,
            reservation_id: m.reservation_id,
            reference_state: PricingItemReferenceState::stored(
                &m.reference_state,
                &format_args!("plan item {id} reference_state"),
            )?,
            version: m.version,
            created_by: m.created_by,
            created_by_name: None,
            created_at: m.created_at,
            updated_at: m.updated_at,
        })
    }
}
/// `GET /plan-items/{id}` (D-434): the item with its revision's number and state and its plan.
#[toolkit_macros::api_dto(response)]
pub struct PricingPlanItemReadDto {
    #[serde(flatten)]
    pub item: PricingPlanItemDto,
    pub plan_id: Uuid,
    pub rev_no: i32,
    /// The revision's state as it reads today (D-447): a scheduled revision whose date has come
    /// reads `published`, and the one it replaces `superseded`, before the switch is persisted.
    pub state: PricingRevisionState,
}
/// `POST /plans`: a plan and its draft rev 1 on `book_id`.
#[toolkit_macros::api_dto(request)]
#[derive(Clone)]
#[serde(deny_unknown_fields)]
pub struct PricingPlanCreate {
    /// 1 to 32 characters of `A-Z`, `0-9`, `-` and `_`, starting with a letter or a digit, judged as
    /// sent (D-468): blank is 400 `PLAN_CODE_REQUIRED`, any other code off the rule 400
    /// `PLAN_CODE_INVALID`, and a code over 64 characters 400 `FIELD_TOO_LONG` (D-457).
    pub code: String,
    pub name: String,
    pub book_id: Uuid,
    /// Rev 1's sale date, `YYYY-MM-DD` (D-463); omitted or null means "at publish". A malformed
    /// date is 400 `DATE_INVALID`, as the revision PATCH answers it.
    #[serde(default)]
    pub available_from: Option<String>,
}
/// `POST /plans/{id}/clone`: the new plan's own code and name; its draft rev 1 copies the source's
/// published revision, and its sale date unless the body names one (D-463).
#[toolkit_macros::api_dto(request)]
#[derive(Clone)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::option_option,
    reason = "the clone distinguishes omission (keep the source's), null (clear) and a new date"
)]
pub struct PricingPlanClone {
    /// The new plan's code, under the rule of `POST /plans` (D-468).
    pub code: String,
    pub name: String,
    /// Rev 1's sale date, `YYYY-MM-DD` (D-463): omitted keeps the source's, a date overrides it,
    /// null clears it ("at publish"). A malformed date is 400 `DATE_INVALID`.
    #[serde(default, deserialize_with = "nullable_date")]
    pub available_from: Option<Option<String>>,
}
/// `PATCH /plans/{id}`: the plan's name, under If-Match.
#[toolkit_macros::api_dto(request)]
#[derive(Clone)]
#[serde(deny_unknown_fields)]
pub struct PricingPlanPatch {
    pub name: String,
}
/// One revision of a plan as its plan lists it: the header, without items.
#[toolkit_macros::api_dto(response)]
pub struct PricingPlanRevisionHeader {
    pub id: Uuid,
    pub rev_no: i32,
    pub book_id: Uuid,
    /// The book `book_id` names (D-516). `book_id` stays.
    pub book: PricingBookIdentity,
    /// The state as it reads today (D-447).
    pub state: PricingRevisionState,
    pub available_from: Option<String>,
    /// When it took effect: its approval's instant, or 00:00 UTC of its sale date for a revision
    /// that waited for it (D-447, D-450); null until then.
    #[serde(with = "time::serde::rfc3339::option")]
    pub published_at: Option<time::OffsetDateTime>,
    /// Its author (D-461): the one principal who edits it while it is a draft (D-404).
    pub created_by: Uuid,
    /// The current name of `created_by` (D-519): its display name, else first and last name, else
    /// username, from Account Management under the caller's rights; "System" for a system actor.
    /// Null when no name is available now, and on a write answer.
    pub created_by_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    /// When it was submitted for approval (D-461): the submission of its pending unit, or of the
    /// unit that approved it; null for a draft, also one back from a reject, a withdraw or an
    /// unschedule.
    #[serde(with = "time::serde::rfc3339::option")]
    pub submitted_at: Option<time::OffsetDateTime>,
    /// When it was approved (D-461): the decision of the unit that approved it, which a scheduled
    /// revision needs because its `published_at` is null until its date; null before.
    #[serde(with = "time::serde::rfc3339::option")]
    pub approved_at: Option<time::OffsetDateTime>,
}
impl PricingPlanRevisionHeader {
    /// The header of `m` with its effective state and `published_at` (D-447), the instants of
    /// the unit it names among `units` (D-461), and the book `books` holds for `m.book_id` (D-516).
    /// # Errors
    /// `CorruptRow` when `books` does not hold that book.
    pub fn of(
        m: &entity::plan_revision::Model,
        effective: &EffectiveRevision,
        units: &BTreeMap<Uuid, UnitInstants>,
        books: &BTreeMap<Uuid, PricingPlanBook>,
    ) -> Result<Self, RepoError> {
        let (submitted_at, approved_at) =
            unit_instants(m.pending_unit_id, m.approved_by_unit_id, units);
        let book = books.get(&m.book_id).ok_or_else(|| {
            RepoError::CorruptRow(format!("revision {} names lost book {}", m.id, m.book_id))
        })?;
        Ok(Self {
            id: m.id,
            rev_no: m.rev_no,
            book_id: m.book_id,
            book: PricingBookIdentity::from(book),
            state: effective.state.into(),
            available_from: m.available_from.map(|d| d.to_string()),
            published_at: effective.published_at,
            created_by: m.created_by,
            created_by_name: None,
            created_at: m.created_at,
            submitted_at,
            approved_at,
        })
    }
}
/// A book named beside its id (D-516): id, code, name and currency. Validity stays on
/// [`PricingPlanBook`].
#[toolkit_macros::api_dto(response)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PricingBookIdentity {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub currency: String,
}
impl From<&PricingPlanBook> for PricingBookIdentity {
    fn from(book: &PricingPlanBook) -> Self {
        Self {
            id: book.id,
            code: book.code.clone(),
            name: book.name.clone(),
            currency: book.currency.clone(),
        }
    }
}
/// When a revision was submitted and approved (D-461), from the unit it names among `units`: the
/// submission of its pending or approving unit, and the approving unit's decision. A draft names
/// no unit, so it has neither; a unit that does not read leaves both null.
fn unit_instants(
    pending: Option<Uuid>,
    approved_by: Option<Uuid>,
    units: &BTreeMap<Uuid, UnitInstants>,
) -> (Option<time::OffsetDateTime>, Option<time::OffsetDateTime>) {
    let submitted = pending
        .or(approved_by)
        .and_then(|u| units.get(&u))
        .map(|u| u.submitted_at);
    let approved = approved_by
        .and_then(|u| units.get(&u))
        .and_then(|u| u.decided_at);
    (submitted, approved)
}
/// A plan's current revision as its list row names it (D-460): the draft or pending one, else
/// the scheduled one, else the published one in effect, chosen over the states the revisions read
/// today (D-447).
#[toolkit_macros::api_dto(response)]
pub struct PricingPlanCurrent {
    pub revision_id: Uuid,
    pub rev_no: i32,
    /// The state it reads today (D-447): `draft`, `pending`, `scheduled` or `published`.
    pub state: PricingRevisionState,
    /// How many items it holds, every item counted (a legacy one without an entry too, D-467).
    pub item_count: u32,
    /// The SKUs of its items, one per item, in ascending order. They may differ from the plans
    /// `GET /plans?sku_id=` keeps, which need an item with an entry and read the stored state
    /// (D-434).
    pub sku_ids: Vec<Uuid>,
    /// Its author, who edits it while it is a draft (D-404); not the plan's.
    pub created_by: Uuid,
    /// The current name of `created_by` (D-519): its display name, else first and last name, else
    /// username, from Account Management under the caller's rights; "System" for a system actor.
    /// Null when no name is available now, and on a write answer.
    pub created_by_name: Option<String>,
    /// The book it prices on (D-485, D-515): that book's id, code, name, currency and validity.
    pub book: PricingPlanBook,
}
/// The book a plan's current revision prices on (D-485, D-515).
#[toolkit_macros::api_dto(response)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PricingPlanBook {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub currency: String,
    /// `YYYY-MM-DD`, or null when the book is open on that side.
    pub valid_from: Option<String>,
    /// `YYYY-MM-DD`, exclusive, or null when the book is open on that side.
    pub valid_until: Option<String>,
}
impl PricingPlanBook {
    /// The book a plan row names (D-515).
    #[must_use]
    pub fn of(m: &entity::price_book::Model) -> Self {
        Self {
            id: m.id,
            code: m.code.clone(),
            name: m.name.clone(),
            currency: m.currency.clone(),
            valid_from: m.valid_from.map(|d| d.to_string()),
            valid_until: m.valid_until.map(|d| d.to_string()),
        }
    }
}
/// The revision a plan sells today (D-460): its published revision in effect (D-447).
#[toolkit_macros::api_dto(response)]
pub struct PricingPlanInEffect {
    pub revision_id: Uuid,
    pub rev_no: i32,
    /// The SKUs of that revision's items, one per item, in ascending order (D-480). The same set
    /// a draft or pending revision reports as `carried_sku_ids`.
    pub sku_ids: Vec<Uuid>,
}
/// What the plan DTO shows beside its own rows (D-460, D-461, D-480): the item SKUs of the
/// current revisions and of the revisions in effect, by revision id, and the instants of the
/// units the revisions name, by unit id. A read fills it from its two grouped reads; a write from
/// the rows it holds (D-453: a write answers what it wrote).
#[derive(Debug, Default, Clone)]
pub struct PlanReading {
    pub skus: BTreeMap<Uuid, Vec<Uuid>>,
    pub units: BTreeMap<Uuid, UnitInstants>,
    /// The current revisions' books, by book id (D-485).
    pub books: BTreeMap<Uuid, PricingPlanBook>,
}
/// A plan with the headers of its revisions in revision order.
#[toolkit_macros::api_dto(response)]
pub struct PricingPlanDto {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub code: String,
    pub name: String,
    /// The revision number the plan sells today (D-447): the last one published, or the number of
    /// a scheduled revision whose date has come, before its switch is persisted. It never counts
    /// a revision that is still waiting for its date.
    pub published_rev: Option<i32>,
    pub version: i64,
    pub created_by: Uuid,
    /// The current name of `created_by` (D-519): its display name, else first and last name, else
    /// username, from Account Management under the caller's rights; "System" for a system actor.
    /// Null when no name is available now, and on a write answer.
    pub created_by_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
    /// The latest `updated_at` of the plan and its revisions (D-484). A page ordered by it carries
    /// the same instant. Not the If-Match clock (`updated_at`).
    #[serde(with = "time::serde::rfc3339")]
    pub last_activity_at: time::OffsetDateTime,
    /// Whether the plan sells on the request's day (D-484). Never null.
    pub selling: bool,
    /// The change on that day: `draft`, `pending`, `scheduled` or `none` (D-484).
    pub change: PricingPlanChange,
    pub revisions: Vec<PricingPlanRevisionHeader>,
    /// The revision being changed or waiting, else the one in effect (D-460); null without
    /// revisions.
    pub current: Option<PricingPlanCurrent>,
    /// The published revision in effect today (D-460); null before the first publication.
    pub in_effect: Option<PricingPlanInEffect>,
}
impl PricingPlanDto {
    /// The plan with the headers of `revisions` (all of its own) as they read on `today`, its
    /// effective `published_rev` (D-447), its current revision and the one in effect (D-460) and
    /// each header's instants (D-461), from `reading`: derived in memory.
    /// # Errors
    /// `CorruptRow` for a stored token outside its closed set (D-439).
    pub fn of(
        m: entity::plan::Model,
        revisions: &[entity::plan_revision::Model],
        today: time::Date,
        reading: &PlanReading,
    ) -> Result<Self, RepoError> {
        let stored = stored_revisions(revisions)?;
        let effective = plan::effective(&stored, today);
        let facts: Vec<crate::infra::plan_summary::RevisionFact> = revisions
            .iter()
            .map(|r| crate::infra::plan_summary::RevisionFact {
                id: r.id,
                state: r.state.clone(),
                available_from: r.available_from,
                updated_at: r.updated_at,
                book_id: r.book_id,
                currency: None,
            })
            .collect();
        let summary = crate::infra::plan_summary::summarize(m.updated_at, &facts)?;
        let current = if let Some(chosen) = plan::current(&effective) {
            let row = revisions
                .iter()
                .find(|r| r.id == chosen.id)
                .ok_or_else(|| {
                    RepoError::CorruptRow(format!("plan {} current revision {}", m.id, chosen.id))
                })?;
            let book = reading.books.get(&row.book_id).cloned().ok_or_else(|| {
                RepoError::CorruptRow(format!("plan {} current book {}", m.id, row.book_id))
            })?;
            let sku_ids = reading.skus.get(&chosen.id).cloned().unwrap_or_default();
            Some(PricingPlanCurrent {
                revision_id: chosen.id,
                rev_no: chosen.rev_no,
                state: chosen.state.into(),
                item_count: u32::try_from(sku_ids.len()).unwrap_or(u32::MAX),
                sku_ids,
                created_by: row.created_by,
                created_by_name: None,
                book,
            })
        } else {
            None
        };
        let in_effect = plan::in_effect(&effective).map(|r| PricingPlanInEffect {
            revision_id: r.id,
            rev_no: r.rev_no,
            sku_ids: reading.skus.get(&r.id).cloned().unwrap_or_default(),
        });
        Ok(Self {
            id: m.id,
            tenant_id: m.tenant_id,
            code: m.code,
            name: m.name,
            published_rev: plan::published_rev(m.published_rev, &stored, m.id, today),
            version: m.version,
            created_by: m.created_by,
            created_by_name: None,
            created_at: m.created_at,
            updated_at: m.updated_at,
            last_activity_at: summary.last_activity_at,
            selling: crate::infra::plan_summary::selling(&summary, today),
            change: PricingPlanChange::stored(
                crate::infra::plan_summary::change(&summary, today).as_str(),
                &format_args!("plan {}", m.id),
            )?,
            revisions: revisions
                .iter()
                .zip(&effective)
                .map(|(r, e)| PricingPlanRevisionHeader::of(r, e, &reading.units, &reading.books))
                .collect::<Result<Vec<_>, _>>()?,
            current,
            in_effect,
        })
    }
}
/// The current revision's id of one plan's revisions as they read on `today` (D-460): the
/// revision whose items the plans list reads.
/// # Errors
/// `CorruptRow` for a state outside the closed set.
pub fn current_revision(
    revisions: &[entity::plan_revision::Model],
    today: time::Date,
) -> Result<Option<Uuid>, RepoError> {
    Ok(plan::current(&effective_revisions(revisions, today)?).map(|r| r.id))
}
/// The published revision in effect among one plan's revisions as they read on `today` (D-447,
/// D-480): the revision whose SKUs `in_effect.sku_ids` and a draft's `carried_sku_ids` name.
/// # Errors
/// `CorruptRow` for a state outside the closed set.
pub fn in_effect_revision(
    revisions: &[entity::plan_revision::Model],
    today: time::Date,
) -> Result<Option<Uuid>, RepoError> {
    Ok(plan::in_effect(&effective_revisions(revisions, today)?).map(|r| r.id))
}
/// The units `revisions` name (D-461): each one's pending or approving unit.
#[must_use]
pub fn named_units(revisions: &[entity::plan_revision::Model]) -> Vec<Uuid> {
    revisions
        .iter()
        .filter_map(|r| r.pending_unit_id.or(r.approved_by_unit_id))
        .collect()
}
#[toolkit_macros::api_dto(response)]
pub struct PricingPlanList {
    pub items: Vec<PricingPlanDto>,
    /// The toolkit pager's page (D-485). `limit` is the page size, 500 by default and at most 500.
    pub page_info: toolkit_odata::PageInfo,
}
/// `GET /plans/counts` (D-485): every plan the list's narrowing keeps, by the derived axes.
#[toolkit_macros::api_dto(response)]
pub struct PricingPlanCounts {
    pub by_selling: PricingPlanSellingCounts,
    pub by_change: PricingPlanChangeCounts,
    pub total: u64,
}
/// `selling` is never null, so the two buckets add up to `total`.
#[toolkit_macros::api_dto(response)]
#[derive(Default)]
pub struct PricingPlanSellingCounts {
    #[serde(rename = "true")]
    pub r#true: u64,
    #[serde(rename = "false")]
    pub r#false: u64,
}
/// Every change the list can name, 0 when none.
#[toolkit_macros::api_dto(response)]
#[derive(Default)]
pub struct PricingPlanChangeCounts {
    pub none: u64,
    pub draft: u64,
    pub pending: u64,
    pub scheduled: u64,
}
/// A pending revision's vote progress (D-462), counts only: its unit, the approve votes the quorum
/// counts (the current generation's, not stale: the approval library's `counted_approvals`, the
/// count the vote door judges by) and the quorum.
#[toolkit_macros::api_dto(response)]
pub struct PricingPlanApprovalProgress {
    pub unit_id: Uuid,
    pub approvals: u32,
    pub quorum_required: u32,
}
/// A revision with its items (D-407: items are a sub-resource, read with their revision).
#[toolkit_macros::api_dto(response)]
pub struct PricingPlanRevisionDto {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub plan_id: Uuid,
    pub rev_no: i32,
    pub book_id: Uuid,
    /// The state as it reads today (D-447); `scheduled` is approved and waiting for its sale date.
    pub state: PricingRevisionState,
    /// The sale date; null means "at publish".
    pub available_from: Option<String>,
    pub pending_unit_id: Option<Uuid>,
    pub approved_by_unit_id: Option<Uuid>,
    /// When it took effect (D-447): its approval's instant, or 00:00 UTC of its sale date for a
    /// revision that waited for it; null until then.
    #[serde(with = "time::serde::rfc3339::option")]
    pub published_at: Option<time::OffsetDateTime>,
    pub version: i64,
    /// The draft's author: the one principal who edits it and its items (D-404).
    pub created_by: Uuid,
    /// The current name of `created_by` (D-519): its display name, else first and last name, else
    /// username, from Account Management under the caller's rights; "System" for a system actor.
    /// Null when no name is available now, and on a write answer.
    pub created_by_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
    /// When it was submitted for approval (D-461): the submission of its pending unit, or of the
    /// unit that approved it; null for a draft.
    #[serde(with = "time::serde::rfc3339::option")]
    pub submitted_at: Option<time::OffsetDateTime>,
    /// When it was approved (D-461): the decision of the unit that approved it; null before.
    #[serde(with = "time::serde::rfc3339::option")]
    pub approved_at: Option<time::OffsetDateTime>,
    /// The vote progress of a pending revision (D-462), readable under plan read; null for any
    /// other state.
    pub approval: Option<PricingPlanApprovalProgress>,
    /// True when no item is `unreserved` or `confirmation_pending` (D-480). `lost` counts as
    /// settled, so settled is not a green check. Computed from the items in hand, on every answer.
    pub reservations_settled: bool,
    pub items: Vec<PricingPlanItemDto>,
}
impl PricingPlanRevisionDto {
    /// A read of `m`: as [`Self::of`], with the state and `published_at` it reads today among
    /// `siblings`, its plan's revisions (D-447).
    /// # Errors
    /// `CorruptRow` for a stored token outside its closed set (D-439).
    pub fn read(
        m: &entity::plan_revision::Model,
        siblings: &[entity::plan_revision::Model],
        items: Vec<entity::plan_item::Model>,
        today: time::Date,
    ) -> Result<Self, RepoError> {
        let mut dto = Self::of(m, items)?;
        if let Some(e) = effective_revisions(siblings, today)?
            .into_iter()
            .find(|e| e.id == m.id)
        {
            dto.state = e.state.into();
            dto.published_at = e.published_at;
        }
        Ok(dto)
    }
    /// The revision as stored: a write's answer, whose state is the one it just wrote. Its unit
    /// fields are null, as a draft's are; [`Self::with_units`] fills them for a revision that
    /// names a unit.
    /// # Errors
    /// `CorruptRow` for a stored token outside its closed set (D-439).
    pub fn of(
        m: &entity::plan_revision::Model,
        items: Vec<entity::plan_item::Model>,
    ) -> Result<Self, RepoError> {
        let items = items
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, _>>()?;
        let reservations_settled = reservations_settled(&items);
        Ok(Self {
            id: m.id,
            tenant_id: m.tenant_id,
            plan_id: m.plan_id,
            rev_no: m.rev_no,
            book_id: m.book_id,
            state: PricingRevisionState::stored(
                &m.state,
                &format_args!("revision {} state", m.id),
            )?,
            available_from: m.available_from.map(|d| d.to_string()),
            pending_unit_id: m.pending_unit_id,
            approved_by_unit_id: m.approved_by_unit_id,
            published_at: m.published_at,
            version: m.version,
            created_by: m.created_by,
            created_by_name: None,
            created_at: m.created_at,
            updated_at: m.updated_at,
            submitted_at: None,
            approved_at: None,
            approval: None,
            reservations_settled,
            items,
        })
    }
    /// The instants of the unit the revision names among `units` (D-461), and the vote progress
    /// `approval` of a pending revision (D-462): kept only while the revision reads `pending`.
    #[must_use]
    pub fn with_units(
        mut self,
        units: &BTreeMap<Uuid, UnitInstants>,
        approval: Option<PricingPlanApprovalProgress>,
    ) -> Self {
        let (submitted_at, approved_at) =
            unit_instants(self.pending_unit_id, self.approved_by_unit_id, units);
        self.submitted_at = submitted_at;
        self.approved_at = approved_at;
        self.approval = approval.filter(|_| self.state == PricingRevisionState::Pending);
        self
    }
}
/// True when no item is `unreserved` or `confirmation_pending` (D-480). `confirmed` and `lost`
/// are settled, so settled is not a green check. An empty item list is settled.
fn reservations_settled(items: &[PricingPlanItemDto]) -> bool {
    items.iter().all(|item| {
        !matches!(
            item.reference_state,
            PricingItemReferenceState::Unreserved | PricingItemReferenceState::ConfirmationPending
        )
    })
}
/// One distinct entry a revision's items name, with the default chain's price in force on the
/// revision's sale date (D-480).
#[toolkit_macros::api_dto(response)]
pub struct PricingPlanEntrySummary {
    pub price_book_entry_id: Uuid,
    pub book_id: Uuid,
    pub charge_kind: PricingChargeKind,
    pub period: Option<PricingPeriod>,
    pub model: PricingModel,
    pub dimension_key: Option<String>,
    /// The default chain's approved price in force on the revision's sale date, chosen by
    /// `price_book_entries::headline` (D-440, D-472). Null when only value chains price the
    /// entry, when none is in force, or when the caller's `price_book` read does not admit the
    /// entry's book (D-434).
    pub price_on_sale_date: Option<PricingPriceDto>,
}
impl PricingPlanEntrySummary {
    /// The entry as the revision read shows it, with `price` already judged for the book.
    /// # Errors
    /// `CorruptRow` for a stored token outside its closed set.
    pub fn of(
        m: entity::price_book_entry::Model,
        price: Option<PricingPriceDto>,
    ) -> Result<Self, RepoError> {
        let id = m.id;
        Ok(Self {
            price_book_entry_id: id,
            book_id: m.book_id,
            charge_kind: PricingChargeKind::stored(
                &m.charge_kind,
                &format_args!("entry {id} charge_kind"),
            )?,
            period: m
                .period
                .as_deref()
                .map(|p| PricingPeriod::stored(p, &format_args!("entry {id} period")))
                .transpose()?,
            model: PricingModel::stored(&m.model, &format_args!("entry {id} model"))?,
            dimension_key: m.dimension_key,
            price_on_sale_date: price,
        })
    }
}
/// `GET /plan-revisions/{id}` (D-480): the revision, its sale date, one summary per distinct
/// entry its items name, and, while it is draft or pending, the SKUs the plan sells today.
/// Write answers keep [`PricingPlanRevisionDto`].
#[toolkit_macros::api_dto(response)]
pub struct PricingPlanRevisionReadDto {
    #[serde(flatten)]
    pub revision: PricingPlanRevisionDto,
    /// The checks' sale date (`domain::plan::sale_date`): `available_from`, or today for "at
    /// publish". A past `available_from` answers that past date, as the checks do.
    pub sale_date: String,
    /// One per distinct entry the items name, in entry id order.
    pub entries: Vec<PricingPlanEntrySummary>,
    /// Draft or pending only (null otherwise): the SKU ids of the plan's published revision in
    /// effect today (D-447), `[]` when none is in effect. The set a re-add of a deprecated SKU
    /// judges (D-465).
    pub carried_sku_ids: Option<Vec<Uuid>>,
}
/// `GET /plan-revisions/{id}/reservations` (D-480): each item's reference, and whether they have
/// all settled.
#[toolkit_macros::api_dto(response)]
pub struct PricingPlanReservationsDto {
    pub items: Vec<PricingPlanReservationItemDto>,
    /// The same rule as the revision's `reservations_settled`: `lost` counts as settled.
    pub settled: bool,
}
/// One item on the reservations read (D-480).
#[toolkit_macros::api_dto(response)]
pub struct PricingPlanReservationItemDto {
    pub item_id: Uuid,
    pub reference_state: PricingItemReferenceState,
    pub reservation_id: Option<Uuid>,
}
impl TryFrom<&entity::plan_item::Model> for PricingPlanReservationItemDto {
    type Error = RepoError;
    fn try_from(m: &entity::plan_item::Model) -> Result<Self, RepoError> {
        Ok(Self {
            item_id: m.id,
            reference_state: PricingItemReferenceState::stored(
                &m.reference_state,
                &format_args!("plan item {} reference_state", m.id),
            )?,
            reservation_id: m.reservation_id,
        })
    }
}
/// `GET /approval-policy/{kind}/effective` (D-481): the quorum a submit of `kind` needs now.
#[toolkit_macros::api_dto(response)]
pub struct PricingEffectivePolicyDto {
    pub kind: PricingApprovalKind,
    pub quorum_required: u32,
}
/// `PATCH /plan-revisions/{id}`, draft only: the book and the sale date, never an item list
/// (D-407). A book change remaps each item to the new book's entry of the same (SKU, charge kind,
/// period, model, policy digest) and equal dimension key (D-502); an unmatched item keeps its
/// entry and the checks show it foreign.
#[toolkit_macros::api_dto(request)]
#[derive(Clone)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::option_option,
    reason = "PATCH distinguishes omission, null clearing and a new date"
)]
pub struct PricingPlanRevisionPatch {
    pub book_id: Option<Uuid>,
    #[serde(default, deserialize_with = "nullable_date")]
    pub available_from: Option<Option<String>>,
}
/// `PATCH /plan-items/{id}`, draft only: never a SKU change (the SKU is the item's reference).
/// It sets the item's entry (D-467, D-512), including on an item that has none. `treatment`,
/// `included_qty` and `qty_min` are refused (400 `BODY_UNEXPECTED`). A null entry is 400
/// `ITEM_ENTRY_MISSING`: a PATCH never clears an entry.
#[toolkit_macros::api_dto(request)]
#[derive(Clone)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::option_option,
    reason = "PATCH tells omission from an explicit null, which it refuses"
)]
pub struct PricingPlanItemPatch {
    #[serde(default, deserialize_with = "nullable")]
    pub price_book_entry_id: Option<Option<Uuid>>,
}
#[allow(
    clippy::option_option,
    reason = "PATCH distinguishes omission, null clearing and a new value"
)]
fn nullable<'de, D: serde::Deserializer<'de>, T: serde::Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    <Option<T> as serde::Deserialize>::deserialize(d).map(Some)
}
/// An item a check row is about (D-466): the item, its SKU and the entry it names.
#[toolkit_macros::api_dto(response)]
#[expect(
    clippy::struct_field_names,
    reason = "the wire names of ask 29, each an id of another aggregate (D-466)"
)]
pub struct PricingPlanCheckSubject {
    pub item_id: Uuid,
    pub sku_id: Uuid,
    /// Null for an item that names no entry.
    pub price_book_entry_id: Option<Uuid>,
}
/// A pending price that blocks a check row (D-466): its approval unit, the price and its entry.
#[toolkit_macros::api_dto(response)]
#[expect(
    clippy::struct_field_names,
    reason = "the wire names of ask 29, each an id of another aggregate (D-466)"
)]
pub struct PricingPlanCheckBlockingPrice {
    pub unit_id: Uuid,
    pub price_id: Uuid,
    pub price_book_entry_id: Uuid,
}
/// One row of a revision's checks (D-408). An `info` row is always ok and never blocks.
#[toolkit_macros::api_dto(response)]
pub struct PricingPlanCheckDto {
    pub code: String,
    pub ok: bool,
    pub label: String,
    pub detail: String,
    pub info: bool,
    /// The approval units whose pending prices would cover what is uncovered: computed on every
    /// read, never stored (spec §6).
    pub blocked_by: Vec<Uuid>,
    /// The items that turn the row red, in the revision's item order (D-466): empty for a green
    /// row and for a plan-wide one (the plan's name, its book, the book's validity, the item
    /// count, and the information rows).
    pub subjects: Vec<PricingPlanCheckSubject>,
    /// The pending prices behind `blocked_by`, one per price, ordered by unit then price (D-466):
    /// the units they name are exactly `blocked_by`.
    pub blocked_by_prices: Vec<PricingPlanCheckBlockingPrice>,
}
impl From<crate::domain::plan::Check> for PricingPlanCheckDto {
    fn from(c: crate::domain::plan::Check) -> Self {
        let blocked_by = c.blocked_by();
        Self {
            code: c.code.into(),
            ok: c.ok,
            label: c.label,
            detail: c.detail,
            info: c.info,
            blocked_by,
            subjects: c
                .subjects
                .into_iter()
                .map(|s| PricingPlanCheckSubject {
                    item_id: s.item,
                    sku_id: s.sku,
                    price_book_entry_id: s.entry,
                })
                .collect(),
            blocked_by_prices: c
                .blocked_by_prices
                .into_iter()
                .map(|p| PricingPlanCheckBlockingPrice {
                    unit_id: p.unit,
                    price_id: p.price,
                    price_book_entry_id: p.entry,
                })
                .collect(),
        }
    }
}
/// `GET /plan-revisions/{id}/checks`: every check on the sale date, from fresh SKU reads.
#[toolkit_macros::api_dto(response)]
pub struct PricingPlanChecksDto {
    pub checks: Vec<PricingPlanCheckDto>,
    /// Every check is ok: the revision may be submitted.
    pub ready: bool,
    /// `available_from`, or today for a revision sold from its publication.
    pub sale_date: String,
    /// The `plan_revision` quorum a submit of this revision needs (D-481): the tenant's effective
    /// policy, the number the APPROVAL info row already shows. The revision itself has no quorum.
    pub quorum_required: u32,
}
/// `GET /plan-revisions/checks` (D-482): one revision's checks, the single read's answer.
#[toolkit_macros::api_dto(response)]
pub struct PricingRevisionChecksDto {
    pub revision_id: Uuid,
    /// Byte-identical to `GET /plan-revisions/{id}/checks` for this revision.
    pub checks: PricingPlanChecksDto,
}
/// `GET /plan-revisions/checks` (D-482): the checks of the revisions the caller may read, and
/// the ids the tenant does not hold or the plan-read scope does not admit.
#[toolkit_macros::api_dto(response)]
pub struct PricingRevisionChecksBatchDto {
    pub items: Vec<PricingRevisionChecksDto>,
    pub missing: Vec<Uuid>,
}
#[toolkit_macros::api_dto(response)]
pub struct PricingReferenceOpDto {
    pub op_id: Uuid,
    pub kind: PricingReferenceOpKind,
    pub state: PricingReferenceOpState,
    /// The reference the op works for (D-407).
    pub ref_kind: PricingReferenceOpRefKind,
    pub ref_id: Uuid,
    pub sku_id: Uuid,
    pub reservation_id: Option<Uuid>,
    pub attempts: i32,
    #[serde(with = "time::serde::rfc3339")]
    pub next_attempt_at: time::OffsetDateTime,
    pub last_error: Option<String>,
    /// Why the op releases its reference: `book_archived` for a `release` (D-522); null for every
    /// other op.
    pub reason: Option<PricingReferenceOpReason>,
}
impl TryFrom<entity::reference_op::Model> for PricingReferenceOpDto {
    type Error = RepoError;
    fn try_from(op: entity::reference_op::Model) -> Result<Self, RepoError> {
        let id = op.op_id;
        // The work record as the drive reads it: one that does not decode is a corrupt row.
        let work = crate::infra::reference_work::Work::read(&op)
            .map_err(|_| RepoError::CorruptRow(format!("op {id} work")))?;
        let reason = work
            .reason
            .as_deref()
            .map(|token| PricingReferenceOpReason::stored(token, &format_args!("op {id} reason")))
            .transpose()?;
        Ok(Self {
            op_id: id,
            kind: PricingReferenceOpKind::stored(&op.kind, &format_args!("op {id} kind"))?,
            state: PricingReferenceOpState::stored(&op.state, &format_args!("op {id} state"))?,
            ref_kind: PricingReferenceOpRefKind::stored(
                &op.ref_kind,
                &format_args!("op {id} ref_kind"),
            )?,
            ref_id: op.ref_id,
            sku_id: op.sku_id,
            reservation_id: op.reservation_id,
            attempts: op.attempts,
            next_attempt_at: op.next_attempt_at,
            last_error: op.last_error,
            reason,
        })
    }
}
#[toolkit_macros::api_dto(response)]
pub struct PricingReferenceOpPage {
    pub items: Vec<PricingReferenceOpDto>,
    pub next_cursor: Option<Uuid>,
}
#[derive(Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PricingReferenceOpQuery {
    pub state: Option<String>,
    pub limit: Option<u64>,
    pub cursor: Option<Uuid>,
}

#[toolkit_macros::api_dto(request)]
#[derive(Clone, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct PricingPriceCreate {
    pub dim_value: Option<String>,
    /// Money in the entry's model (D-427); a price carries no model of its own.
    pub price: serde_json::Value,
    pub min_fee: Option<String>,
    pub eligibility: String,
    pub effective_from: String,
    pub temporary_until: Option<String>,
    pub note: Option<String>,
}
/// `POST /prices/{id}/end` (D-521).
#[toolkit_macros::api_dto(request)]
#[derive(Clone, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct PricingPriceEnd {
    /// The new end, `YYYY-MM-DD`, exclusive: after today, after the price's start, and no later
    /// than its current end.
    pub effective_to: String,
}
#[toolkit_macros::api_dto(request)]
#[derive(Clone)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::option_option,
    reason = "PATCH distinguishes omission, null clearing and a new value"
)]
pub struct PricingPricePatch {
    #[serde(default, deserialize_with = "nullable_date")]
    pub dim_value: Option<Option<String>>,
    pub price: Option<serde_json::Value>,
    #[serde(default, deserialize_with = "nullable_date")]
    pub min_fee: Option<Option<String>>,
    pub eligibility: Option<String>,
    /// A temporary draft's start moves with its end; a return's start is its pair's end.
    pub effective_from: Option<String>,
    /// The end of the temporary half of a draft; its pair is re-derived over the new dates. Any
    /// other price, and `null`, is 400 `TEMPORARY_PRICE_FIXED`.
    #[serde(default, deserialize_with = "nullable_date")]
    pub temporary_until: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable_date")]
    pub note: Option<Option<String>>,
}
/// The draft price, and its return partner when the request made a temporary pair.
#[toolkit_macros::api_dto(response)]
pub struct PricingPriceCreated {
    pub items: Vec<PricingPriceDto>,
}

/// One reviewer decision; decisions of earlier generations are kept and marked stale.
#[toolkit_macros::api_dto(response)]
pub struct PricingDecisionDto {
    pub actor: Uuid,
    /// The current name of `actor` (D-519): its display name, else first and last name, else
    /// username, from Account Management under the caller's rights; "System" for a system actor.
    /// Null when no name is available now, and on a write answer.
    pub actor_name: Option<String>,
    pub generation: i32,
    pub decision: PricingDecisionKind,
    pub note: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub at: time::OffsetDateTime,
    pub stale: bool,
}
impl From<bss_approval::Decision> for PricingDecisionDto {
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
/// An approval unit with its snapshot, decisions and, on the card, the live impact; and whether
/// its reader may approve it (D-471).
#[expect(
    clippy::struct_excessive_bools,
    reason = "the wire carries three independent caller flags (D-497)"
)]
#[toolkit_macros::api_dto(response)]
pub struct PricingApprovalUnitDto {
    pub id: Uuid,
    /// The unit's kind, one pricing records: a stored unit of another kind is a corrupt row (500),
    /// never served.
    pub kind: PricingApprovalKind,
    pub ref_type: String,
    pub ref_id: Uuid,
    pub state: PricingUnitState,
    pub generation: i32,
    pub quorum_required: u32,
    pub common_effective_date: Option<String>,
    pub submitted_by: Uuid,
    /// The current name of `submitted_by` (D-519): its display name, else first and last name, else
    /// username, from Account Management under the caller's rights; "System" for a system actor.
    /// Null when no name is available now, and on a write answer.
    pub submitted_by_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub submitted_at: time::OffsetDateTime,
    /// The submitter's note (D-445), the unit shape products shares (P-D-219): the note a plan
    /// revision's submit or publish-changes sent (D-464), or null. A single price's submit takes
    /// none.
    pub submit_note: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub decided_at: Option<time::OffsetDateTime>,
    pub decided_note: Option<String>,
    pub snapshot: serde_json::Value,
    pub decisions: Vec<PricingDecisionDto>,
    pub impact: Option<serde_json::Value>,
    /// Whether the caller may Approve this unit now (D-471): the approval engine's own rule
    /// (`bss_approval::approve_eligibility`, D-459) over the unit's stored items and its decisions,
    /// with the caller as the voter. It is false for a decided unit, for its submitter and every
    /// author of its items (separation of duties) and for a caller who already voted in its current
    /// generation. It means Approve only: a reject judges no separation of duties. It also
    /// requires the caller's `approval_unit:approve` grant on this unit (D-497). Without that
    /// grant the vote door is 403.
    pub caller_can_approve: bool,
    /// Whether the caller may reject this unit now (D-497): the approve grant, the unit pending,
    /// and no vote by the caller in this generation.
    pub caller_can_reject: bool,
    /// Whether the caller may withdraw this unit now (D-497): the caller submitted it, the unit
    /// is pending, and the caller holds the submit grant the withdraw door asks.
    pub caller_can_withdraw: bool,
}
impl PricingApprovalUnitDto {
    /// The unit as `reader` reads it: its decisions of every generation, and whether `reader` may
    /// approve it, judged by the engine's own predicate over the `authors` of the unit's stored
    /// (current generation) items and its `decisions` (D-459, D-471). `impact` is the caller's to
    /// fill.
    /// # Errors
    /// `CorruptRow` for a kind pricing does not record.
    pub fn of(
        u: bss_approval::Unit,
        authors: &[Uuid],
        decisions: Vec<bss_approval::Decision>,
        reader: Uuid,
        approve_scope: &toolkit_db::secure::AccessScope,
        submit_scope: &toolkit_db::secure::AccessScope,
    ) -> Result<Self, RepoError> {
        let grant_approve = crate::authz::scope_holds(approve_scope, u.tenant_id, u.id);
        let grant_submit = crate::authz::scope_holds(submit_scope, u.tenant_id, u.id);
        let engine =
            bss_approval::approve_eligibility(&u, authors.iter().copied(), &decisions, reader)
                .refusal
                .is_none();
        let pending = u.state == bss_approval::UnitState::Pending;
        let voted = bss_approval::already_voted(&u, &decisions, reader);
        Ok(Self {
            id: u.id,
            kind: PricingApprovalKind::stored(&u.kind, &format_args!("approval unit {}", u.id))?,
            ref_type: u.ref_type,
            ref_id: u.ref_id,
            state: u.state.into(),
            generation: u.generation,
            quorum_required: u.quorum_required,
            common_effective_date: u.common_effective_date.map(|d| d.to_string()),
            submitted_by: u.submitted_by,
            submitted_by_name: None,
            submitted_at: u.submitted_at,
            submit_note: u.submit_note,
            decided_at: u.decided_at,
            decided_note: u.decided_note,
            snapshot: u.snapshot,
            decisions: decisions.into_iter().map(Into::into).collect(),
            impact: None,
            caller_can_approve: engine && grant_approve,
            caller_can_reject: grant_approve && pending && !voted,
            caller_can_withdraw: grant_submit && pending && u.submitted_by == reader,
        })
    }
}
/// `GET /approval-units/counts` (D-470): the units the list's narrowing keeps, by state and by
/// kind, every state and kind named (0 when none), and their total.
#[toolkit_macros::api_dto(response)]
pub struct PricingApprovalUnitCounts {
    pub by_state: PricingApprovalUnitStateCounts,
    pub by_kind: PricingApprovalUnitKindCounts,
    pub total: u64,
}
/// The units in each state (D-470).
#[toolkit_macros::api_dto(response)]
#[derive(Default)]
pub struct PricingApprovalUnitStateCounts {
    pub pending: u64,
    pub approved: u64,
    pub rejected: u64,
    pub withdrawn: u64,
}
/// The units of each kind pricing records (D-470).
#[toolkit_macros::api_dto(response)]
#[derive(Default)]
pub struct PricingApprovalUnitKindCounts {
    pub prices: u64,
    pub plan_revision: u64,
}
/// One page of the unit list (D-458): its units, and the toolkit pager's `page_info`, whose
/// `next_cursor` continues it.
#[toolkit_macros::api_dto(response)]
pub struct PricingApprovalUnitList {
    pub items: Vec<PricingApprovalUnitDto>,
    pub page_info: toolkit_odata::PageInfo,
}
/// A vote names the generation its reviewer saw; a reject needs a note.
#[toolkit_macros::api_dto(request)]
#[derive(Clone)]
#[serde(deny_unknown_fields)]
pub struct PricingVoteRequest {
    pub generation: i32,
    pub note: Option<String>,
}
#[toolkit_macros::api_dto(response)]
pub struct PricingVoteReceipt {
    pub have: Option<u32>,
    pub need: Option<u32>,
    pub outcome: PricingVoteOutcome,
    pub unit: PricingApprovalUnitDto,
}
/// The unit a submission recorded and its prices after the transaction.
#[toolkit_macros::api_dto(response)]
pub struct PricingSubmitReceipt {
    pub applied: bool,
    pub unit: PricingApprovalUnitDto,
    pub prices: Vec<PricingPriceDto>,
}
/// The `plan_revision` unit a submission recorded and the revision after the transaction:
/// pending under the unit, or published when quorum zero applied it at once.
#[toolkit_macros::api_dto(response)]
pub struct PricingPlanRevisionSubmitReceipt {
    pub applied: bool,
    pub unit: PricingApprovalUnitDto,
    pub revision: PricingPlanRevisionDto,
}
#[toolkit_macros::api_dto(request)]
#[derive(Clone)]
#[serde(deny_unknown_fields)]
pub struct PricingPublishChangesRequest {
    pub price_ids: Option<Vec<Uuid>>,
    pub common_effective_date: Option<String>,
    /// The submitter's note for the approver (D-464), stored on the unit as `submit_note`; at most
    /// 2000 characters (400 `NOTE_TOO_LONG`). Omitted or null, none.
    #[serde(default)]
    pub note: Option<String>,
}
/// `POST /plan-revisions/{id}/submit`: an optional body with the submitter's note (D-464). No
/// body, `{}` and `note: null` carry none; any other key is 400 `BODY_UNEXPECTED`.
#[toolkit_macros::api_dto(request)]
#[derive(Clone)]
#[serde(deny_unknown_fields)]
pub struct PricingPlanRevisionSubmitRequest {
    /// The submitter's note for the approver, stored on the unit as `submit_note`; at most 2000
    /// characters (400 `NOTE_TOO_LONG`).
    #[serde(default)]
    pub note: Option<String>,
}
/// One draft price as the operator sees it before publishing: the entry key, the chain,
/// the approved predecessor it follows, its pair partner and the default selection.
#[toolkit_macros::api_dto(response)]
pub struct PricingProposedPrice {
    pub price: PricingPriceDto,
    pub entry: PricingPriceBookEntryDto,
    pub chain: String,
    pub before: Option<PricingPriceDto>,
    pub pair_partner_id: Option<Uuid>,
    pub selected: bool,
}
#[toolkit_macros::api_dto(response)]
pub struct PricingPublishChanges {
    pub book: PriceBookDto,
    pub prices: Vec<PricingProposedPrice>,
    /// Prices and entries the listed drafts touch, the plan revisions naming those entries, and
    /// subscriptions (unavailable until the Subscriptions integration).
    pub impact: serde_json::Value,
}
#[toolkit_macros::api_dto(request)]
#[derive(Clone)]
#[serde(deny_unknown_fields)]
pub struct PricingApprovalPolicyPut {
    pub kind: Option<String>,
    pub quorum: u32,
}
#[toolkit_macros::api_dto(response)]
pub struct PricingApprovalPolicyDto {
    pub default_quorum: u32,
    pub overrides: std::collections::BTreeMap<String, u32>,
}
impl From<bss_approval::Policy> for PricingApprovalPolicyDto {
    fn from(p: bss_approval::Policy) -> Self {
        Self {
            default_quorum: p.default_quorum,
            overrides: p.overrides,
        }
    }
}
// `$orderby` is the served query name.
#[allow(unknown_lints, de0803_api_snake_case)]
#[derive(Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PricingApprovalUnitQuery {
    pub state: Option<String>,
    pub kind: Option<String>,
    pub ref_id: Option<Uuid>,
    pub book_id: Option<Uuid>,
    /// Page size (D-458): 200 by default, clamped at 500.
    pub limit: Option<u64>,
    /// The opaque continuation of a page's `page_info.next_cursor`.
    pub cursor: Option<String>,
    /// `submitted_at asc` (the default, D-458) or `submitted_at desc` (D-470); the id breaks a
    /// tie in the same direction. A cursor carries its order, so a continuation sends none.
    #[serde(rename = "$orderby")]
    pub orderby: Option<String>,
    /// `false` skips the live impact read: every unit answers `impact: null` (D-470).
    pub impact: Option<bool>,
}
/// `GET /approval-units/counts` (D-470): the list's narrowing, and nothing else.
#[derive(Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PricingApprovalUnitCountsQuery {
    pub state: Option<String>,
    pub kind: Option<String>,
    pub ref_id: Option<Uuid>,
    pub book_id: Option<Uuid>,
}
