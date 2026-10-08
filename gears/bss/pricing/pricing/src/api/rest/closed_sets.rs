//! The closed sets the RESPONSE schemas publish as an `enum` (D-439), with exactly the tokens the
//! columns store and the wire has always carried. Requests keep `string`, so every door keeps its
//! own refusal code for a value outside a set (D-403).
//!
//! A set with a domain enum maps from it one to one (a variant added on either side is a compile
//! error); a stored token is read back with `stored`, where a token outside the set — its column's
//! CHECK forbids one — is `CorruptRow` naming the row, a 500, never a panic.
use crate::domain::{
    plan::{ReferenceState as ItemReference, RevisionState},
    price::{ChangeKind, DisplayStatus, Eligibility, PriceState},
    price_book_entry::{ChargeKind, Model, OpState, ReferenceState as EntryReference},
    reference_op::OpKind,
    resolve::Source,
};
use crate::infra::{approval_kinds::Kind as ApprovalKind, storage::RepoError};
use bss_approval::{UnitState, Verdict};

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
    /// An entry's charge kind.
    PricingChargeKind from ChargeKind { Recurring => "recurring", Usage => "usage", OneTime => "one_time" }
);
closed_set!(
    /// A recurring entry's period; a usage or one-time entry has none.
    PricingPeriod { Month => "month", Year => "year" }
);
closed_set!(
    /// An entry's model (D-427), the model of every price of the entry.
    PricingModel from Model {
        Flat => "flat",
        PerUnit => "per_unit",
        Graduated => "graduated",
        Volume => "volume",
        Package => "package",
    }
);
closed_set!(
    /// Where an entry's SKU reference stands with Products; `released` once its book is archived
    /// (D-522).
    PricingEntryReferenceState from EntryReference {
        ConfirmationPending => "confirmation_pending",
        Confirmed => "confirmed",
        Lost => "lost",
        Released => "released",
    }
);
closed_set!(
    /// Who may take a price: every subscription, or new ones only.
    PricingEligibility from Eligibility { All => "all", New => "new" }
);
closed_set!(
    /// A price's stored state.
    PricingPriceState from PriceState {
        Draft => "draft",
        Pending => "pending",
        Approved => "approved",
        Rejected => "rejected",
        Cancelled => "cancelled",
    }
);
closed_set!(
    /// What a price row asks the prices unit to do (D-520, D-521). `set` is a price.
    PricingChangeKind from ChangeKind {
        Set => "set",
        Cancel => "cancel",
        End => "end",
    }
);
closed_set!(
    /// A price's display state (matrix row 10): an approved price shows where its window stands.
    /// A cancelled price shows `cancelled` (D-520).
    PricingPriceStatus from DisplayStatus {
        Draft => "draft",
        Pending => "pending",
        Rejected => "rejected",
        Scheduled => "scheduled",
        Active => "active",
        Superseded => "superseded",
        Cancelled => "cancelled",
    }
);
// No `From`: the read serves only a cancelled price's status (D-520), never a display status
// computed from the day (D-422).
closed_set!(
    /// A pinned price's status (D-520): `cancelled` for a price cancelled before it started, the
    /// token the entry's price list shows. The only status `GET /prices/{id}` serves.
    PricingPinnedPriceStatus { Cancelled => "cancelled" }
);
closed_set!(
    /// Where a plan item's SKU reference stands with Products.
    PricingItemReferenceState from ItemReference {
        Unreserved => "unreserved",
        ConfirmationPending => "confirmation_pending",
        Confirmed => "confirmed",
        Lost => "lost",
    }
);
closed_set!(
    /// A plan revision's state.
    PricingRevisionState from RevisionState {
        Draft => "draft",
        Pending => "pending",
        Scheduled => "scheduled",
        Published => "published",
        Superseded => "superseded",
    }
);
closed_set!(
    /// How a plan's list row is changing (D-485): a draft or pending revision, a scheduled one
    /// still waiting for its date, or none.
    PricingPlanChange {
        None => "none",
        Draft => "draft",
        Pending => "pending",
        Scheduled => "scheduled",
    }
);
closed_set!(
    /// The state of a revision that resolves, as it reads today (D-447): a published or
    /// superseded one on every date (D-419), a scheduled one from its sale date on (D-454). No
    /// other resolves.
    PricingResolvedRevisionState {
        Published => "published",
        Superseded => "superseded",
        Scheduled => "scheduled",
    }
);
closed_set!(
    /// What a reference op does (D-413); `release` lets an archived book's entry go (D-522).
    PricingReferenceOpKind from OpKind {
        Create => "create",
        Delete => "delete",
        Rereserve => "rereserve",
        Attach => "attach",
        Release => "release",
    }
);
closed_set!(
    /// A reference op's protocol state.
    PricingReferenceOpState from OpState {
        Reserving => "reserving",
        Written => "written",
        Cancelling => "cancelling",
        Releasing => "releasing",
        Done => "done",
    }
);
// Read from its stored token, with no `From`: the work record stores the reason as text
// (`reference_work::BOOK_ARCHIVED_REASON`).
closed_set!(
    /// Why a reference op releases its reference (D-522): its entry's book was archived.
    PricingReferenceOpReason { BookArchived => "book_archived" }
);
// Read from its stored token, with no `From`: the domain's `RefKind::Entry` is spelled
// `price_book_entry`, and the schema publishes a variant's own snake case.
closed_set!(
    /// The reference a reference op works for (D-407).
    PricingReferenceOpRefKind { PriceBookEntry => "price_book_entry", PlanItem => "plan_item" }
);
closed_set!(
    /// An approval unit's state.
    PricingUnitState from UnitState {
        Pending => "pending",
        Approved => "approved",
        Rejected => "rejected",
        Withdrawn => "withdrawn",
    }
);
closed_set!(
    /// A kind of approval unit pricing records (spec §6). No CHECK holds the stored column: the
    /// repository reads it through this set, so a unit of another kind is a corrupt row (500).
    PricingApprovalKind from ApprovalKind { Prices => "prices", PlanRevision => "plan_revision" }
);
closed_set!(
    /// One reviewer's decision.
    PricingDecisionKind from Verdict { Approve => "approve", Reject => "reject" }
);
closed_set!(
    /// What a vote did: counted toward the quorum, or applied, rejected or withdrew the unit.
    PricingVoteOutcome {
        Pending => "pending",
        Applied => "applied",
        Rejected => "rejected",
        Withdrawn => "withdrawn",
    }
);
closed_set!(
    /// When a charge is billed: in advance of its period or in arrears.
    PricingBillingTiming { Advance => "advance", Arrears => "arrears" }
);
closed_set!(
    /// Where a resolved invoice input came from (D-421).
    PricingResolveSource from Source { Entry => "entry", Sku => "sku", Tenant => "tenant" }
);
closed_set!(
    /// Where a SKU's entry stands today (D-486): an approved price in force, else one that
    /// starts later, else neither. Not a price's display status.
    PricingSkuEntryStatus {
        Priced => "priced",
        Scheduled => "scheduled",
        Unpriced => "unpriced",
    }
);

#[cfg(test)]
#[path = "closed_sets_tests.rs"]
mod closed_sets_tests;
