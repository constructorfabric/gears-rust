//! The closed sets the RESPONSE schemas publish as an `enum` (P-D-217, twin of pricing D-439),
//! with exactly the tokens the columns store and the wire has always carried. Requests keep
//! `string`, so every door keeps its own refusal for a value outside a set.
//!
//! A set with an SDK or approval enum maps from it one to one (a variant added on either side is a
//! compile error); a stored token is read back with `stored`, where a token outside the set — its
//! column's CHECK forbids one — is `CorruptRow` naming the row, a 500, never a panic.
use crate::domain::approvals::ApprovalKind;
use crate::infra::storage::RepoError;
use bss_approval::{UnitState, Verdict};
use bss_products_sdk::models::{BillingTiming, Lifecycle, ReferenceKind, ReferenceState, SkuType};

macro_rules! closed_set {
    ($(#[$meta:meta])* $name:ident from $domain:ident { $($variant:ident => $token:literal),+ $(,)? }) => {
        closed_set!($(#[$meta])* $name { $($variant => $token),+ });
        impl From<$domain> for $name {
            fn from(value: $domain) -> Self {
                match value {
                    $($domain::$variant => Self::$variant),+
                }
            }
        }
        impl From<$name> for $domain {
            fn from(value: $name) -> Self {
                match value {
                    $($name::$variant => Self::$variant),+
                }
            }
        }
    };
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $token:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[toolkit_macros::api_dto(response)]
        pub enum $name {
            $(#[serde(rename = $token)] $variant),+
        }
        impl $name {
            /// Every value, in the schema's order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            /// The token stored and carried on the wire.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $token),+
                }
            }
            /// A stored token read back; `row` names where it was read.
            /// # Errors
            /// `CorruptRow` for a token outside the set.
            pub fn stored(token: &str, row: &dyn std::fmt::Display) -> Result<Self, RepoError> {
                match token {
                    $($token => Ok(Self::$variant),)+
                    other => Err(RepoError::CorruptRow(format!(
                        "{row}: {other:?} is not a {}",
                        stringify!($name)
                    ))),
                }
            }
        }
    };
}

closed_set!(
    /// A SKU's type.
    ProductsSkuType from SkuType {
        Recurring => "recurring",
        Usage => "usage",
        OneTime => "one_time",
        Bundle => "bundle",
    }
);
closed_set!(
    /// A SKU's lifecycle. A retire under review is `retire_pending`, not a lifecycle (P-D-248).
    ProductsLifecycle from Lifecycle {
        Draft => "draft",
        Published => "published",
        Deprecated => "deprecated",
        Retired => "retired",
    }
);
closed_set!(
    /// When a SKU's charge is billed: in advance of its period or in arrears.
    ProductsBillingTiming from BillingTiming { Advance => "advance", Arrears => "arrears" }
);
closed_set!(
    /// A category's status.
    ProductsCategoryStatus { Active => "active", Retired => "retired" }
);
closed_set!(
    /// An approval unit's state.
    ProductsUnitState from UnitState {
        Pending => "pending",
        Approved => "approved",
        Rejected => "rejected",
        Withdrawn => "withdrawn",
    }
);
closed_set!(
    /// A kind of approval unit products records (P-D-227). No CHECK holds the stored column: the
    /// repository reads it through this set, so a unit of another kind is a corrupt row (500).
    #[expect(
        clippy::enum_variant_names,
        reason = "each variant is its stored token in its own case: the schema publishes a \
                  variant's snake case, so `Publish` would publish `publish`"
    )]
    ProductsApprovalKind from ApprovalKind {
        SkuPublish => "sku_publish",
        SkuChange => "sku_change",
        SkuRetire => "sku_retire",
    }
);
closed_set!(
    /// One reviewer's decision.
    ProductsDecisionKind from Verdict { Approve => "approve", Reject => "reject" }
);
closed_set!(
    /// What a vote did: counted toward the quorum, or applied, rejected or withdrew the unit.
    ProductsVoteOutcome {
        Pending => "pending",
        Applied => "applied",
        Rejected => "rejected",
        Withdrawn => "withdrawn",
    }
);
closed_set!(
    /// The kind of owner object a SKU reference protects.
    ProductsReferenceKind from ReferenceKind {
        PriceBookEntry => "price_book_entry",
        PlanItem => "plan_item",
        SoldAs => "sold_as",
    }
);
closed_set!(
    /// A SKU reference's state; a released one is a tombstone.
    ProductsReferenceState from ReferenceState {
        Reserved => "reserved",
        Confirmed => "confirmed",
        Released => "released",
    }
);

#[cfg(test)]
#[path = "closed_sets_tests.rs"]
mod closed_sets_tests;
