//! The usage-type catalog as this registry sees it — one narrow port for one
//! external dependency (P-D-184).
//!
//! # Why the port is here and not bound to the collector's own client
//!
//! The gear used to take `dyn UsageCollectorClientV1` from `ClientHub`, so a
//! deployment whose usage types come from something other than that collector
//! had to implement the whole of it — records, batch writes, aggregation,
//! deactivation, deletion — to supply a catalog of type ids. This trait is the
//! part products actually needs, and a module that registers one is preferred
//! over the collector adapter.
//!
//! # Why both methods sit on one trait
//!
//! A supplier that fills the authoring pick-list also answers the publish gate.
//! Split them and a deployment lists from one catalog while gating against
//! another; the disagreement reaches an operator as *"I picked it from your
//! list and publish says it does not exist"*.
//!
//! # No serde here
//!
//! This crate's own rule (see the crate doc): the gear's REST DTOs own serde
//! and map onto these types, so a wire concern stays out of the contract.

use async_trait::async_trait;
use toolkit_canonical_errors::{CanonicalError, resource_error};
use toolkit_security::SecurityContext;

#[resource_error(gts_id!("cf.bss.products.recognized_set.v1~"))]
struct UsageTypeCatalogResource;

/// The canonical error every implementation owes when no catalog is configured
/// — a **501**, never an empty page.
///
/// Public, and public for pricing's reason one gear over: every implementation
/// of the port owes the same answer to the same fact, and a second spelling of
/// it is a second thing a caller has to recognise.
#[must_use]
pub fn unconfigured_usage_type_catalog() -> CanonicalError {
    UsageTypeCatalogResource::unimplemented("no usage-type catalog is configured").create()
}

/// The canonical error when the catalog refused the caller — a **403**.
///
/// **Carries no detail.** The collector SDK's own `PermissionDenied` says its
/// PDP reason is "kept for operator logs; the host lift drops it from the
/// public wire body", so an implementation logs it and tells the caller only
/// that authorization was refused. Distinct from the 503 below because an
/// operator retries an outage and cannot retry a denial.
#[must_use]
pub fn usage_type_catalog_denied() -> CanonicalError {
    UsageTypeCatalogResource::permission_denied()
        .with_reason("the usage-type catalog refused this caller")
        .create()
}

/// The canonical error when the catalog rejected the query itself — a **400**.
///
/// A malformed filter or an unusable page size is the caller's, not an outage,
/// and reporting it as one sends an operator to retry something that will never
/// succeed.
#[must_use]
pub fn usage_type_catalog_rejected_the_query() -> CanonicalError {
    UsageTypeCatalogResource::invalid_argument()
        .with_field_violation(
            "q",
            "the usage-type catalog rejected this query",
            "invalid_filter",
        )
        .create()
}

/// The canonical error for a continuation token this walk did not mint — a
/// **400**, because it arrives on a public query string.
#[must_use]
pub fn invalid_usage_type_cursor() -> CanonicalError {
    UsageTypeCatalogResource::invalid_argument()
        .with_field_violation(
            "cursor",
            "not a continuation token this walk minted",
            "invalid_cursor",
        )
        .create()
}

/// The canonical error when a configured catalog did not answer — a **503**,
/// and distinct from the 501 above because the operator's next act differs:
/// configure one, versus retry or go and look at the one that is configured.
///
/// **This one carries no resource identity, and the other three do**, because
/// the canonical `service_unavailable` builder takes none — it is an outage of
/// the transport rather than a verdict about a resource. Recorded rather than
/// smoothed over: a client keying off the resource type sees
/// `recognized_set.v1~` on three failures of this port and nothing on the
/// fourth. Giving all four a matching identity needs a resource of this port's
/// own, which is a catalog pair no single slice mints.
#[must_use]
pub fn usage_type_catalog_unreachable(detail: impl Into<String>) -> CanonicalError {
    CanonicalError::service_unavailable()
        .with_detail(detail)
        .create()
}

/// One usage type, in the three fields this registry reads.
///
/// Nothing else the catalog knows is carried: **P-D-184 is resolvability
/// only**, so a lifecycle state or a dimension set would be a fact this gear
/// promises to judge and does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageTypeBinding {
    /// The GTS id a meter declaration names — a derived type of the usage
    /// record base, `…usage_record.v1~…`.
    pub gts_id: String,
    /// `counter` or `gauge`, as the catalog spells it.
    pub kind: String,
    /// The metadata keys the catalog declares for this type, in its own order.
    pub metadata_fields: Vec<String>,
}

/// What the catalog answers about one id.
///
/// **Four values, not two**: a catalog that says *no*, a catalog that says
/// *nothing* and a catalog that refuses *the caller* are different facts, and
/// the publish gate treats them differently — the first is a refusal the
/// author can fix, the second is fail-closed and retryable, the third is a
/// permission the caller lacks (P-D-207: the catalog is read as the caller, so
/// a denial is an honest 403, never an outage).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsageTypeAnswer {
    /// The catalog knows this id.
    Resolved(UsageTypeBinding),
    /// The catalog answered, and the answer is no — **or** the id is not a
    /// valid GTS id at all, which cannot name anything anywhere and so needs
    /// no round trip to refuse.
    Unresolved,
    /// The catalog did not answer: absent, unreachable, or past the caller's
    /// deadline.
    Unavailable,
    /// The catalog refused the caller: it answered, and the answer is that
    /// this caller may not read usage types. Retrying does not help; a grant
    /// does.
    Forbidden,
}

/// One page of the authoring pick-list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageTypePage {
    /// The page's types, in the catalog's own order.
    pub items: Vec<UsageTypeBinding>,
    /// The cursor that walks forward, absent on the last page.
    pub next_cursor: Option<String>,
    /// The cursor that walks back, absent on the first.
    pub prev_cursor: Option<String>,
    /// The page size the catalog **actually applied**, which need not be the
    /// one the caller asked for: a catalog may have a ceiling of its own, and
    /// a screen that sizes its pager off the requested number would size it
    /// off something nobody honoured.
    pub limit: u32,
}

/// The catalog a deployment binds this registry to.
///
/// Registered on `ClientHub` as `dyn UsageTypeCatalog`; the gear prefers a
/// registered implementation over its own adapter over the usage collector,
/// and over any configured fallback.
#[async_trait]
pub trait UsageTypeCatalog: Send + Sync + 'static {
    /// Does this id name a usage type?
    ///
    /// **Resolvability and nothing else** (P-D-184): no lifecycle check, no
    /// dimension check. An implementation that cannot answer must say
    /// [`UsageTypeAnswer::Unavailable`] rather than guess, because the publish
    /// gate fails closed on it and would otherwise freeze a meter nobody
    /// confirmed.
    async fn resolve(&self, ctx: &SecurityContext, usage_type_ref: &str) -> UsageTypeAnswer;

    /// The authoring pick-list, paged.
    ///
    /// `q` narrows by case-insensitive substring of the id, `kind` by
    /// equality. An implementation that cannot narrow may ignore either, but
    /// must not answer a page it did not narrow as though it had.
    ///
    /// # Errors
    ///
    /// [`CanonicalError`] when the catalog is unconfigured, unreachable or
    /// unusable. **None of those is an empty page.** A caller must be able to
    /// tell *"this deployment has no usage types"* from *"nobody could be
    /// asked"*, and an implementation that collapses them hands an operator
    /// silence to read as a clean answer.
    async fn list(
        &self,
        ctx: &SecurityContext,
        q: Option<&str>,
        kind: Option<&str>,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<UsageTypePage, CanonicalError>;
}
