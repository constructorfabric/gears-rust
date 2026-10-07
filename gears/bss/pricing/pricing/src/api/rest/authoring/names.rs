//! D-519: the actor ids every read shows, and their `*_name` siblings.
//!
//! A read builds its answer in its transaction, then names the actors of the whole answer with
//! one `ActorNames::fill` after the transaction has ended: no statement is added, and no
//! transaction waits on Account Management. A POST answer is stored as its key's receipt, and a
//! name is never stored, so no write answer names anyone: its `*_name` fields stay null.
use super::dto::{
    PriceBookDto, PriceBookExport, PricingApprovalUnitDto, PricingApprovalUnitList,
    PricingDecisionDto, PricingEntryPriceList, PricingExportEntry, PricingPlanCurrent,
    PricingPlanDto, PricingPlanEntrySummary, PricingPlanItemDto, PricingPlanItemReadDto,
    PricingPlanList, PricingPlanRevisionDto, PricingPlanRevisionHeader, PricingPlanRevisionReadDto,
    PricingPriceBookEntryList, PricingPriceBookEntryReadDto, PricingPriceBookReadDto,
    PricingPriceBookUnarchiveDto, PricingPriceDto, PricingProposedPrice, PricingPublishChanges,
    PricingSettingsDto, PricingSkuEntryDto, PricingSkuEntryList,
};
use super::{AuthoringState, support};
use axum::{http::StatusCode, response::Response};
use bss_rest::actor_names::{ActorFields, Names, label};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// A read's 200: `body` with its actors named in one lookup, and its `ETag` when `version` is
/// given.
/// # Errors
/// An `ETag` that is not a header value.
pub(super) async fn named<T: ActorFields + serde::Serialize>(
    state: &AuthoringState,
    ctx: &SecurityContext,
    mut body: T,
    version: Option<u64>,
) -> Result<Response, CanonicalError> {
    state.actor_names.fill(ctx, &mut body).await;
    support::response(StatusCode::OK, &body, version)
}

/// The settings' writer is null before the first write.
impl ActorFields for PricingSettingsDto {
    fn actor_ids(&self, ids: &mut Vec<Uuid>) {
        ids.extend(self.updated_by);
    }
    fn fill_names(&mut self, names: &Names) {
        self.updated_by_name = self.updated_by.and_then(|id| label(names, id));
    }
}

bss_rest::actor_fields!(PricingPriceDto { created_by => created_by_name } []);
bss_rest::actor_fields!(PricingPlanItemDto { created_by => created_by_name } []);
bss_rest::actor_fields!(PricingPlanItemReadDto {}[item]);
bss_rest::actor_fields!(PricingPlanRevisionHeader { created_by => created_by_name } []);
bss_rest::actor_fields!(PricingPlanCurrent { created_by => created_by_name } []);
bss_rest::actor_fields!(PricingPlanDto { created_by => created_by_name } [revisions, current]);
bss_rest::actor_fields!(PricingPlanList {}[items]);
bss_rest::actor_fields!(PricingPlanRevisionDto { created_by => created_by_name } [items]);
bss_rest::actor_fields!(PricingPlanEntrySummary {}[price_on_sale_date]);
bss_rest::actor_fields!(PricingPlanRevisionReadDto {} [revision, entries]);
bss_rest::actor_fields!(PricingDecisionDto { actor => actor_name } []);
bss_rest::actor_fields!(PricingApprovalUnitDto { submitted_by => submitted_by_name } [decisions]);
bss_rest::actor_fields!(PricingApprovalUnitList {}[items]);
bss_rest::actor_fields!(PricingPriceBookEntryReadDto {} [current_price, next_price]);
bss_rest::actor_fields!(PricingPriceBookEntryList {}[items]);
bss_rest::actor_fields!(PricingSkuEntryDto {} [current_price, next_price]);
bss_rest::actor_fields!(PricingSkuEntryList {}[items]);
bss_rest::actor_fields!(PricingEntryPriceList {}[items]);
bss_rest::actor_fields!(PricingExportEntry {}[prices]);
bss_rest::actor_fields!(PriceBookExport {}[book, entries]);
bss_rest::actor_fields!(PricingPriceBookReadDto {}[book]);
bss_rest::actor_fields!(PricingPriceBookUnarchiveDto {}[book]);

/// A book names the actor who archived it, while it is archived (D-522).
impl ActorFields for PriceBookDto {
    fn actor_ids(&self, ids: &mut Vec<Uuid>) {
        ids.extend(self.archived_by);
    }
    fn fill_names(&mut self, names: &Names) {
        self.archived_by_name = self.archived_by.and_then(|id| label(names, id));
    }
}
bss_rest::actor_fields!(PricingProposedPrice {} [price, before]);
bss_rest::actor_fields!(PricingPublishChanges {}[book, prices]);
