//! Local registry vocabulary and reservation/fence eligibility (decision 17).
use crate::domain::error::DomainError;
use bss_products_sdk::models::Lifecycle;
use toolkit_macros::domain_model;
/// The object in an owner gear that depends on a SKU.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    PriceBookEntry,
    PlanItem,
    SoldAs,
}
impl RefKind {
    /// Stable storage token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PriceBookEntry => "price_book_entry",
            Self::PlanItem => "plan_item",
            Self::SoldAs => "sold_as",
        }
    }
}
/// Released attempts remain as history and no longer block a fence.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefState {
    Reserved,
    Confirmed,
    Released,
}
impl RefState {
    /// Stable storage token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Confirmed => "confirmed",
            Self::Released => "released",
        }
    }
}
/// Live counts by owner object, with the reserved subset reported separately.
/// `price_book_entries` and `plans` include reserved and confirmed rows; `plans` includes
/// both plan items and sold-as references. `reserved` is not an extra total.
#[domain_model]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReferenceSummary {
    pub price_book_entries: u32,
    pub plans: u32,
    pub reserved: u32,
    /// Live counts by owner and kind, including each owner’s reserved subset.
    pub by_owner: std::collections::BTreeMap<String, std::collections::BTreeMap<String, u32>>,
}
/// Check local eligibility before a reservation is written in the transaction.
/// @cpt-cf-bss-products-fr-reference-registry
///
/// # Errors
/// `SKU_FENCED` for a fence (`retire_pending` or `type_change_pending`) or an inactive head.
/// A retire under review is fenced by the flag (P-D-248); it is not a lifecycle.
pub fn reservation_allowed(lifecycle: Lifecycle, fenced: bool) -> Result<(), DomainError> {
    if fenced {
        return Err(DomainError::Conflict {
            code: "SKU_FENCED",
            detail: "the SKU is fenced".into(),
        });
    }
    if fenced || !matches!(lifecycle, Lifecycle::Published | Lifecycle::Deprecated) {
        return Err(DomainError::Conflict {
            code: "SKU_FENCED",
            detail: "the SKU does not admit new references".into(),
        });
    }
    Ok(())
}
/// A reserved row counts until explicitly confirmed or released; time never expires it.
/// @cpt-cf-bss-products-fr-reference-registry
///
/// # Errors
/// Returns the caller's fence refusal code when any live reference remains.
pub fn fence_allowed(live_references: u32, code: &'static str) -> Result<(), DomainError> {
    if live_references > 0 {
        return Err(DomainError::Conflict {
            code,
            detail: format!("{live_references} live reference(s) block this fence"),
        });
    }
    Ok(())
}
#[cfg(test)]
#[path = "references_tests.rs"]
mod references_tests;
