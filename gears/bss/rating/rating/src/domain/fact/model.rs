//! A commercial fact version as Rating stores it (DESIGN §3.7 `bss_rating__fact`; DECISIONS
//! T-D-87): Subscriptions' `subscriptions-rating/1` profile, already checked by
//! [`super::profile::read_fact`].

use std::fmt;

use bss_rating_core::Interval;
use bss_rating_core::engine::current::input::{CoreBinding, TenantAxes};
use serde_json::Value;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::digest::Sha256Hex;

/// What a fact bills (DESIGN §3.1 child kinds).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FactKind {
    /// A recurring charge: one `period_line` child.
    Recurring,
    /// A usage parent: one schedule, its windows from the binding's `UsageRatingPolicy`.
    Usage,
    /// A one-time charge: stored, not rated, while R-19 is open (T-D-18).
    OneTime,
}

impl FactKind {
    /// The stored and wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Recurring => "recurring",
            Self::Usage => "usage",
            Self::OneTime => "one_time",
        }
    }

    /// The kind a stored or wire name denotes.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "recurring" => Some(Self::Recurring),
            "usage" => Some(Self::Usage),
            "one_time" => Some(Self::OneTime),
            _ => None,
        }
    }
}

impl fmt::Display for FactKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether the version still carries the obligation (Subscriptions SUB-D-32).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Obligation {
    /// The obligation stands: the version's slices are rated.
    Active,
    /// The obligation was removed: the version expects no child and its result is a complete
    /// empty replacement (SUB-D-45).
    Zeroed,
}

impl Obligation {
    /// The stored and wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Zeroed => "zeroed",
        }
    }

    /// The obligation a stored or wire name denotes.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "active" => Some(Self::Active),
            "zeroed" => Some(Self::Zeroed),
            _ => None,
        }
    }
}

/// When the line is billed: the fact's own timing, which equals its bindings' invoice timing.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Timing {
    /// Billed at acceptance.
    Advance,
    /// Billed once the period has ended.
    Arrears,
}

impl Timing {
    /// The stored and wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Advance => "advance",
            Self::Arrears => "arrears",
        }
    }

    /// The timing a stored or wire name denotes.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "advance" => Some(Self::Advance),
            "arrears" => Some(Self::Arrears),
            _ => None,
        }
    }
}

/// What a slice of the timeline is (Subscriptions SUB-D-42).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Coverage {
    /// Priced by the accepted terms the slice names.
    Chargeable,
    /// Served but not charged (a trial or a grant), with the producer's reason.
    Nonchargeable {
        /// Why the interval is not charged.
        reason: String,
    },
    /// Served, but Subscriptions has no authority to continue the terms: the children over it
    /// wait `pending(terms_unresolved)` and are never priced at zero.
    Unresolved {
        /// Why the terms are unresolved.
        reason: String,
    },
}

impl Coverage {
    /// The stored and wire name.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Chargeable => "chargeable",
            Self::Nonchargeable { .. } => "nonchargeable",
            Self::Unresolved { .. } => "unresolved",
        }
    }

    /// Whether a usage window over this slice is expected: chargeable and unresolved time is
    /// served and must be accounted for, nonchargeable time is not.
    #[must_use]
    pub const fn is_rated(&self) -> bool {
        matches!(self, Self::Chargeable | Self::Unresolved { .. })
    }
}

/// One purchase lot of a quantity-bearing component inside a slice (Subscriptions SUB-D-46).
#[expect(
    clippy::struct_field_names,
    reason = "`lot_id` is the name Subscriptions' profile gives it"
)]
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Lot {
    /// The stable purchase-lot id.
    pub lot_id: String,
    /// The lot's quantity, canonical decimal text.
    pub quantity: String,
    /// The quantity's unit.
    pub unit: String,
    /// The accepted term the lot is priced by; absent on an `unresolved` slice.
    pub terms_ref: Option<String>,
    /// The purchase the lot came from.
    pub purchase_origin: String,
}

/// One slice of the timeline: half-open, with one coverage, the accepted terms in force and the
/// lots of a quantity-bearing component.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slice {
    /// `[from, to)`.
    pub span: Interval,
    /// What the slice is.
    pub coverage: Coverage,
    /// The accepted terms in force, by `terms_ref`; at most one per dimension value.
    pub terms: Vec<String>,
    /// The complete lot allocation of the interval.
    pub lots: Vec<Lot>,
}

/// One accepted term the fact carries: a frozen `AcceptedBinding` with the plan revision it was
/// accepted under, and its digest.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedTerm {
    /// The reference slices and lots use.
    pub terms_ref: String,
    /// The catalog tenant the binding was resolved in (pricing D-523).
    pub catalog_tenant_id: Uuid,
    /// The plan.
    pub plan_id: Uuid,
    /// The plan revision.
    pub revision_id: Uuid,
    /// The binding, as the core reads it.
    pub binding: CoreBinding,
    /// Pricing's `selected_bindings_digest` of the one cell: the binding of record's digest.
    pub binding_digest: Sha256Hex,
}

/// The suspension ranges of a recurring line and how they bill.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suspensions {
    /// The ranges, ordered and disjoint, inside the period.
    pub ranges: Vec<Interval>,
    /// `pause_recurring` or `continue`.
    pub posture: String,
}

/// The identity of a fact version.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FactRef {
    /// The seller tenant: fact rows are seller-tenant rows (DESIGN §3.7).
    pub tenant_id: Uuid,
    /// Subscriptions' fact id.
    pub fact_id: Uuid,
    /// Its monotonic version.
    pub fact_version: i64,
}

/// A fact version the profile accepted.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct FactVersion {
    /// Tenant, fact and version.
    pub fact: FactRef,
    /// The version this one replaces.
    pub predecessor_version: Option<i64>,
    /// Subscriptions' payload digest, kept until its codec can be verified (S-U13).
    pub producer_digest: String,
    /// What it bills.
    pub kind: FactKind,
    /// Whether the obligation stands.
    pub obligation: Obligation,
    /// When it is billed.
    pub timing: Timing,
    /// The billing group.
    pub billing_group_id: Uuid,
    /// The subscription.
    pub subscription_id: Uuid,
    /// Seller, payer and resource tenants.
    pub axes: TenantAxes,
    /// The billing currency code.
    pub currency: String,
    /// The component interval alias (`plan#n`, `addon:{id}#n`).
    pub sub_line_key: String,
    /// The one `PriceBook` item of the fact.
    pub item_id: Uuid,
    /// The billing period.
    pub period: Interval,
    /// The served extent, inside the period.
    pub served: Interval,
    /// The Subscriptions-owned `BillingTerms`, verbatim.
    pub billing_terms: Value,
    /// The suspension ranges and posture.
    pub suspensions: Suspensions,
    /// A one-time occurrence id.
    pub occurrence_id: Option<String>,
    /// The slices, ordered, partitioning the served extent; none for a zeroed version.
    pub slices: Vec<Slice>,
    /// The accepted terms the slices name.
    pub terms: Vec<AcceptedTerm>,
}

impl FactVersion {
    /// The accepted term `terms_ref` names.
    #[must_use]
    pub fn term(&self, terms_ref: &str) -> Option<&AcceptedTerm> {
        self.terms.iter().find(|term| term.terms_ref == terms_ref)
    }

    /// The accepted terms the chargeable slices name, each once, in the order first named.
    #[must_use]
    pub fn chargeable_terms(&self) -> Vec<&AcceptedTerm> {
        let mut found: Vec<&AcceptedTerm> = Vec::new();
        for slice in self
            .slices
            .iter()
            .filter(|slice| slice.coverage == Coverage::Chargeable)
        {
            for terms_ref in &slice.terms {
                if let Some(term) = self.term(terms_ref)
                    && !found.iter().any(|seen| seen.terms_ref == term.terms_ref)
                {
                    found.push(term);
                }
            }
        }
        found
    }
}
