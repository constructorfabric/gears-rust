//! Typed pricing read contracts.

use uuid::Uuid;

use rust_decimal::Decimal;

use time::Date;

use crate::{
    Digest,
    meter_semantics::MeterRef,
    terms::{BillingCycle, InvoiceInputs, UsageRatingPolicy},
};

use toolkit_security::SecurityContext;

use toolkit_canonical_errors::CanonicalError;

/// `CatalogRef` value in the versioned pricing read contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogRef {
    /// Tenant id.
    pub tenant_id: Uuid,
}

/// `Tier` value in the versioned pricing read contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tier {
    /// Exclusive upper bound; the last band has no bound.
    pub up_to: Option<Decimal>,
    /// Rate.
    pub rate: Decimal,
}

/// `PriceModel` value in the versioned pricing read contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriceModel {
    /// One flat amount.
    Flat {
        /// Exact amount in the price currency.
        amount: Decimal,
    },
    /// Rate per dated SKU unit, supplied on the binding.
    PerUnit {
        /// Exact amount per unit.
        unit_amount: Decimal,
    },
    /// The selected volume band applies to the entire quantity.
    Volume {
        /// Ordered half-open bands ending in one open band.
        tiers: Vec<Tier>,
    },
    /// Each band prices its own portion of the quantity.
    Graduated {
        /// Ordered half-open bands ending in one open band.
        tiers: Vec<Tier>,
    },
    /// Historical package money, retained for reads.
    Package {
        /// Positive quantity in one package.
        package_size: Decimal,
        /// Exact amount for a package.
        package_price: Decimal,
    }, // Read-only in this slice.
}

/// Where a price read by id stands (D-520). A cancelled price was cancelled before it started: it
/// was never in force, so no resolve, pin or binding returns it, and only the price read by id
/// serves it. More states may be added: a consumer's match keeps a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PriceState {
    /// Approved, whatever its window: open, closed or followed by a later price.
    Approved,
    /// Cancelled through its book's prices unit before it started (D-520).
    Cancelled,
}

/// `ImmutablePrice` value in the versioned pricing read contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImmutablePrice {
    /// Price id.
    pub price_id: Uuid,

    /// Price book entry id.
    pub price_book_entry_id: Uuid,

    /// Financial-content SHA-256, excluding identity and mutable closing metadata.
    pub money_digest: Digest,

    /// Currency.
    pub currency: String,

    /// Model.
    pub model: PriceModel,

    /// Minimum fee.
    pub minimum_fee: Option<Decimal>,

    /// Effective from.
    pub effective_from: Date,

    /// Observed own end of the binding, excluded from the money digest.
    pub ends_on: Option<Date>, // Eligibility observation, excluded from money_digest.

    /// Stored state: `Approved`, or `Cancelled` for a price cancelled before it started (D-520).
    /// A binding's price is always `Approved`. Excluded from both digests.
    pub state: PriceState,
}

/// `ChargeKind` value in the versioned pricing read contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChargeKind {
    Recurring,
    Usage,
    OneTime,
}

/// `AcceptedBinding` value in the versioned pricing read contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedBinding {
    /// Item id.
    pub item_id: Uuid,

    /// Price book entry id.
    pub price_book_entry_id: Uuid,

    /// Dimension key.
    pub dimension_key: Option<String>,

    /// `Dimension` value.
    pub dimension_value: Option<String>,

    /// Sku id.
    pub sku_id: Uuid,

    /// Sku version.
    pub sku_version: i64,

    /// Sku code.
    pub sku_code: String,

    /// Sku name.
    pub sku_name: String,

    /// Unit.
    pub unit: Option<String>, // Dated SKU unit.

    /// Dated SKU usage type. `None` for a non-usage binding. A projection, not policy storage.
    pub meter: Option<MeterRef>,

    /// Price.
    pub price: ImmutablePrice,

    /// Kind.
    pub kind: ChargeKind,

    /// Recurring period.
    pub recurring_period: Option<BillingCycle>,

    /// The requested value was covered by the default chain.
    pub via_default: bool,

    /// Usage rating policy.
    pub usage_rating_policy: Option<UsageRatingPolicy>,

    /// Invoice.
    pub invoice: InvoiceInputs,
}

/// `BindingSelection` value in the versioned pricing read contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingSelection {
    /// Item id.
    pub item_id: Uuid,
    /// `Dimension` value.
    pub dimension_value: Option<String>,
}

/// `PricePin` value in the versioned pricing read contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PricePin {
    /// Item id.
    pub item_id: Uuid,
    /// `Dimension` value.
    pub dimension_value: Option<String>,
    /// Price id.
    pub price_id: Uuid,
}

/// `ResolveQuery` value in the versioned pricing read contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveQuery {
    /// Catalog.
    pub catalog: CatalogRef,

    /// Revision id.
    pub revision_id: Uuid,

    /// Date.
    pub date: Date,

    /// Item id.
    pub item_id: Option<Uuid>,

    /// Pins.
    pub pins: Vec<PricePin>,
}

/// `ResolvedCell` value in the versioned pricing read contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCell {
    /// Selection.
    pub selection: BindingSelection,

    /// Binding.
    pub binding: Option<AcceptedBinding>, // None means uncovered, never free.
}

/// `ResolvedBindings` value in the versioned pricing read contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBindings {
    /// Plan id.
    pub plan_id: Uuid,

    /// Revision id.
    pub revision_id: Uuid,

    /// Cells.
    pub cells: Vec<ResolvedCell>,
}

/// `RevisionRef` value in the versioned pricing read contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionRef {
    /// Plan id.
    pub plan_id: Uuid,
    /// Revision id.
    pub revision_id: Uuid,
    /// Revision no.
    pub revision_no: i32,
}

/// `PriceQuery` value in the versioned pricing read contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceQuery {
    /// Catalog.
    pub catalog: CatalogRef,
    /// Price id.
    pub price_id: Uuid,
}

/// `PlanQuery` value in the versioned pricing read contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanQuery {
    /// Catalog.
    pub catalog: CatalogRef,
    /// Plan id.
    pub plan_id: Uuid,
}

/// Authorized read capability. Every method is a `SafeRead` with no idempotency key.
#[async_trait::async_trait]
pub trait PricingReadV1: Send + Sync {
    /// Resolve the revision's full dimension matrix on a date.
    /// # Errors
    /// Authorization, invalid pins, unavailable dependencies, or incomplete commercial inputs.
    async fn resolve(
        &self,
        ctx: &SecurityContext,
        query: ResolveQuery,
    ) -> Result<ResolvedBindings, CanonicalError>;
    /// Read approved immutable money, including closed historical prices, and a cancelled price
    /// with its money as approved and `state` `Cancelled` (D-520).
    /// # Errors
    /// Authorization or a tenant-scoped missing price, or one that is neither approved nor
    /// cancelled.
    async fn price(
        &self,
        ctx: &SecurityContext,
        query: PriceQuery,
    ) -> Result<ImmutablePrice, CanonicalError>;
    /// Promote due scheduled revisions and return the published revision in effect.
    /// # Errors
    /// Authorization, missing plan/published revision, or unavailable storage.
    async fn current_revision(
        &self,
        ctx: &SecurityContext,
        query: PlanQuery,
    ) -> Result<RevisionRef, CanonicalError>;
}

#[toolkit_canonical_errors::resource_error("gts.cf.bss.pricing.plan.v1~")]
struct PlanResource;
/// A priced cell cannot be projected into a complete commercial binding.
#[derive(Debug, thiserror::Error)]
#[error("incomplete commercial inputs: {field}")]
pub struct IncompleteCommercialInputs {
    /// The absent or unsupported input.
    pub field: &'static str,
}
impl From<IncompleteCommercialInputs> for CanonicalError {
    fn from(value: IncompleteCommercialInputs) -> Self {
        PlanResource::failed_precondition()
            .with_precondition_violation(
                value.field,
                format!("incomplete commercial inputs: {}", value.field),
                "INCOMPLETE_COMMERCIAL_INPUTS",
            )
            .create()
    }
}

/// Explicit IR preserves the established V1 name and classifies all methods as safe reads.
#[must_use]
pub fn pricing_read_v1_ir() -> toolkit_contract::ir::contract::ContractIr {
    crate::acceptance::commercial_ir(
        "PricingReadV1",
        &[
            ("resolve", "ResolveQuery", "ResolvedBindings", false),
            ("price", "PriceQuery", "ImmutablePrice", false),
            ("current_revision", "PlanQuery", "RevisionRef", false),
        ],
    )
}
