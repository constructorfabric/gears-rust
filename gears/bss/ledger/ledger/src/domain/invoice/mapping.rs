//! Account-mapping resolver (architecture §5.2). Resolves the GL target of one
//! invoice item to a `(account_class, gl_code, mapping_status)` triple, in a
//! fixed precedence: a Catalog-supplied class, overridden by a Contract-supplied
//! class when present; on a miss the line routes to `SUSPENSE` with status
//! `PENDING` (never a silent wrong-revenue mapping — an unmapped item must be
//! visibly parked, not booked to an arbitrary revenue account).
//!
//! Pure: a `match` on the two optional inputs carried by the item. No chart /
//! DB access here — the real `account_id` for the resolved class+stream+currency
//! is bound later by the posting glue from the provisioned chart of accounts.

use bss_ledger_sdk::{AccountClass, MappingStatus};
use toolkit_macros::domain_model;

use crate::domain::invoice::builder::InvoiceItem;

/// The resolved GL target of one invoice item. Carries no `account_id`: the
/// concrete chart row is resolved by the posting glue from
/// `(account_class, currency, revenue_stream)` against the provisioned chart.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MappedLine {
    /// The resolved revenue (or `SUSPENSE`) account class.
    pub account_class: AccountClass,
    /// The Catalog GL code carried through to the posted line, if any.
    pub gl_code: Option<String>,
    /// `RESOLVED` when a class was found; `PENDING` when the item routed to
    /// `SUSPENSE` (an operator must reclassify before the period can close).
    pub mapping_status: MappingStatus,
}

/// Resolve one item's GL target.
///
/// Precedence: `contract_class` (Contract override) wins when present, else
/// `catalog_class` (Catalog default). On a miss (neither present) the line
/// routes to [`AccountClass::Suspense`] with [`MappingStatus::Pending`].
#[must_use]
pub fn resolve(item: &InvoiceItem) -> MappedLine {
    // Contract override beats the Catalog default; either resolves RESOLVED.
    match item.contract_class.or(item.catalog_class) {
        Some(account_class) => MappedLine {
            account_class,
            gl_code: item.gl_code.clone(),
            mapping_status: MappingStatus::Resolved,
        },
        // Miss: park on SUSPENSE/PENDING rather than guess a revenue account.
        None => MappedLine {
            account_class: AccountClass::Suspense,
            gl_code: item.gl_code.clone(),
            mapping_status: MappingStatus::Pending,
        },
    }
}

#[cfg(test)]
#[path = "mapping_tests.rs"]
mod tests;
