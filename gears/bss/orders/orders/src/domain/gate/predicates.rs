//! The declared predicate order (03 §3.4 step 2 "predicate order"): the eight adopted catalog
//! predicates (DESIGN `#contract-03-4-1`), Orders' nine (F03 §4.2) and the per-line pin outcome
//! (F03 step 10). Declaration order is persistence, replay and primary-selection order.
use super::ports::GatePort;
use bss_orders_sdk::catalog::Reason;
use toolkit_macros::domain_model;

/// One predicate of the declared order; serializes as its stored token (`PredicateSpec::token`).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PredicateId {
    /// Adopted 1: the requested revision is the one in effect on the assessment date (DESIGN `#contract-03-4-1`).
    CatalogRevisionCurrent,
    /// Adopted 2: the revision is available on the assessment date; an absent availability field never produces a pass (DESIGN #contract-03-4-1).
    CatalogRevisionAvailable,
    /// Adopted 3: the selected item is a member of the requested revision (DESIGN `#contract-03-4-1`).
    CatalogItemMembership,
    /// Adopted 4: every selected charged slot exists and is covered by a binding; unselected uncovered cells do not fail (DESIGN #contract-03-4-1).
    CatalogSlotCoverage,
    /// Adopted 5: the SKU version and descriptors of each bound slot are those for the assessment date, from the accepted binding (DESIGN #contract-03-4-1).
    CatalogSkuVersionDated,
    /// Adopted 6: the SKU is sellable (DESIGN `#contract-03-4-1`).
    CatalogSkuSellable,
    /// Adopted 7: the selected dimension value applies to the payer's market; unevaluable until the D-189 assessment exists (DESIGN #contract-03-4-1).
    CatalogDimensionMarket,
    /// Adopted 8: any further purchase-eligibility rule the catalog owns; unevaluable until the D-189 assessment exists (DESIGN #contract-03-4-1).
    CatalogEligibilityRules,
    /// Orders 1: all three tenant axes resolve against IdP/Account Management, and the payer relationship is confirmed (F03 §4.2; D-215).
    AxisValidity,
    /// Orders 2: the referenced contract is active and its party eligible (F03 §4.2).
    ContractActive,
    /// Orders 3: each selected item's quantity is a positive exact decimal; there is no line-level multiplier (F03 §4.2; D-170).
    QuantityFloor,
    /// Orders 4: the book currency and the producer-declared market applicability agree with the payer profile's market (F03 §4.2).
    MarketConsistency,
    /// Orders 5: line references resolve uniquely (F03 §4.2).
    ReferenceResolution,
    /// Orders 6: the order prices in a single currency (F03 §4.2).
    SingleCurrency,
    /// Orders 7: activeCount plus proposed lines stays within maxConcurrentActive per (payer, resource, overlap key), applied as Subscriptions answers it (F03 §4.2; D-179, D-180).
    OverlapUniqueness,
    /// Orders 8: the date cascade resolves (F03 §4.2).
    DatesResolvable,
    /// Orders 9: no other in-flight order holds the same overlap key for the same payer and resource tenant; the requesting order is excluded (F03 §4.2; D-179).
    InFlightPerKey,
    /// Pin: the per-line pin outcome composing resolve, billing terms and acceptance policy (F03 step 10).
    PinComposition,
}
/// Where a predicate comes from; declaration order is `Adopted` < `Delta` < `Pin`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    /// Adopted from the catalog predicates (DESIGN `#contract-03-4-1`).
    Adopted,
    /// Orders' own additional predicate (F03 §4.2).
    Delta,
    /// The per-line pin outcome (F03 step 10).
    Pin,
}
impl Origin {
    /// The `origin` label value of the gate's predicate metrics.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Adopted => "adopted",
            Self::Delta => "delta",
            Self::Pin => "pin",
        }
    }
}
/// The granularity at which a predicate is evaluated.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Once per order (no line in the identity).
    Order,
    /// Once per line.
    Line,
    /// Once per catalog slot selection of a line.
    Slot,
}

/// The static description of one predicate.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct PredicateSpec {
    /// The predicate this spec describes.
    pub id: PredicateId,
    /// Stored `bss_orders__gate_outcome.predicate` token.
    pub token: &'static str,
    /// Evaluation granularity.
    pub scope: Scope,
    /// Adopted, Orders' own, or pin.
    pub origin: Origin,
    /// The ports whose answers the predicate reads; any unavailable one makes it unevaluable.
    pub ports: &'static [GatePort],
    /// The reasons this predicate may report when it fails or is unevaluable.
    pub failure_reasons: &'static [Reason],
    /// D-230: a failure of this predicate is a definitive local business refusal, final whatever
    /// any provider answers (axis validity, reference resolution, single currency, quantity
    /// floor, dates). In primary selection its first failed outcome outranks every unevaluable
    /// one; every other predicate keeps the D-195 order.
    pub definitive_local: bool,
}
use GatePort as G;
use PredicateId as P;
use Reason as R;
/// Every predicate in declared order (persistence, replay and primary-selection order);
/// the position equals `PredicateId::index`.
pub const PREDICATES: [PredicateSpec; 18] = [
    PredicateSpec {
        id: P::CatalogRevisionCurrent,
        token: "catalog.revision-current",
        scope: Scope::Line,
        origin: Origin::Adopted,
        ports: &[G::CurrentRevision, G::Resolve],
        failure_reasons: &[R::CatalogPredicateFailed, R::PricingRevisionAbsent],
        definitive_local: false,
    },
    PredicateSpec {
        id: P::CatalogRevisionAvailable,
        token: "catalog.revision-available",
        scope: Scope::Line,
        origin: Origin::Adopted,
        ports: &[G::Assessment],
        failure_reasons: &[R::CatalogPredicateFailed],
        definitive_local: false,
    },
    PredicateSpec {
        id: P::CatalogItemMembership,
        token: "catalog.item-membership",
        scope: Scope::Slot,
        origin: Origin::Adopted,
        ports: &[G::Resolve],
        failure_reasons: &[R::CatalogPredicateFailed],
        definitive_local: false,
    },
    PredicateSpec {
        id: P::CatalogSlotCoverage,
        token: "catalog.slot-coverage",
        scope: Scope::Slot,
        origin: Origin::Adopted,
        ports: &[G::Resolve],
        failure_reasons: &[R::CatalogPredicateFailed],
        definitive_local: false,
    },
    PredicateSpec {
        id: P::CatalogSkuVersionDated,
        token: "catalog.sku-version-dated",
        scope: Scope::Slot,
        origin: Origin::Adopted,
        ports: &[G::Resolve],
        failure_reasons: &[R::CatalogPredicateFailed],
        definitive_local: false,
    },
    PredicateSpec {
        id: P::CatalogSkuSellable,
        token: "catalog.sku-sellable",
        scope: Scope::Slot,
        origin: Origin::Adopted,
        ports: &[G::SkuRead],
        failure_reasons: &[R::CatalogPredicateFailed],
        definitive_local: false,
    },
    PredicateSpec {
        id: P::CatalogDimensionMarket,
        token: "catalog.dimension-market",
        scope: Scope::Slot,
        origin: Origin::Adopted,
        ports: &[G::Assessment],
        failure_reasons: &[R::CatalogPredicateFailed],
        definitive_local: false,
    },
    PredicateSpec {
        id: P::CatalogEligibilityRules,
        token: "catalog.eligibility-rules",
        scope: Scope::Line,
        origin: Origin::Adopted,
        ports: &[G::Assessment],
        failure_reasons: &[R::CatalogPredicateFailed],
        definitive_local: false,
    },
    PredicateSpec {
        id: P::AxisValidity,
        token: "orders.axis-validity",
        scope: Scope::Order,
        origin: Origin::Delta,
        ports: &[G::IdentityProfile],
        failure_reasons: &[R::AxisInvalid, R::PayerRelationshipUnconfirmed],
        definitive_local: true,
    },
    PredicateSpec {
        id: P::ContractActive,
        token: "orders.contract-active",
        scope: Scope::Order,
        origin: Origin::Delta,
        ports: &[G::ContractResolution],
        failure_reasons: &[R::ContractNotActive, R::ContractPartyIneligible],
        definitive_local: false,
    },
    PredicateSpec {
        id: P::QuantityFloor,
        token: "orders.quantity-floor",
        scope: Scope::Line,
        origin: Origin::Delta,
        ports: &[],
        failure_reasons: &[R::QuantityBelowFloor],
        definitive_local: true,
    },
    PredicateSpec {
        id: P::MarketConsistency,
        token: "orders.market-consistency",
        scope: Scope::Line,
        origin: Origin::Delta,
        ports: &[G::IdentityProfile, G::Assessment],
        failure_reasons: &[R::MarketInconsistent],
        definitive_local: false,
    },
    PredicateSpec {
        id: P::ReferenceResolution,
        token: "orders.reference-resolution",
        scope: Scope::Line,
        origin: Origin::Delta,
        ports: &[G::Resolve],
        failure_reasons: &[R::ReferenceUnresolvable, R::ReferenceDuplicated],
        definitive_local: true,
    },
    PredicateSpec {
        id: P::SingleCurrency,
        token: "orders.single-currency",
        scope: Scope::Order,
        origin: Origin::Delta,
        ports: &[],
        failure_reasons: &[R::CurrencyMixed],
        definitive_local: true,
    },
    PredicateSpec {
        id: P::OverlapUniqueness,
        token: "orders.overlap-uniqueness",
        scope: Scope::Line,
        origin: Origin::Delta,
        ports: &[G::OverlapKey, G::Occupancy],
        failure_reasons: &[R::OverlapCardinalityExceeded],
        definitive_local: false,
    },
    PredicateSpec {
        id: P::DatesResolvable,
        token: "orders.dates-resolvable",
        scope: Scope::Line,
        origin: Origin::Delta,
        ports: &[],
        failure_reasons: &[R::DateCascadeInvalid],
        definitive_local: true,
    },
    PredicateSpec {
        id: P::InFlightPerKey,
        token: "orders.in-flight-per-key",
        scope: Scope::Line,
        origin: Origin::Delta,
        ports: &[G::OverlapKey],
        failure_reasons: &[R::OrderInFlightForKey],
        definitive_local: false,
    },
    PredicateSpec {
        id: P::PinComposition,
        token: "orders.pin-composition",
        scope: Scope::Line,
        origin: Origin::Pin,
        ports: &[G::Resolve, G::BillingTerms, G::AcceptancePolicy],
        failure_reasons: &[
            R::PinUnresolvable,
            R::OrderBindingPolicyMissing,
            R::BillingTermsUnsupported,
        ],
        definitive_local: false,
    },
];
impl PredicateId {
    /// The stored token (`bss_orders__gate_outcome.predicate`).
    #[must_use]
    pub fn token(self) -> &'static str {
        self.spec().token
    }
    /// The predicate with this stored token, or `None` when unknown.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        PREDICATES.iter().find(|p| p.token == token).map(|p| p.id)
    }
    /// Adopted, Orders' own, or pin.
    #[must_use]
    pub fn origin(self) -> Origin {
        self.spec().origin
    }
    /// The static spec for this predicate.
    #[must_use]
    pub fn spec(self) -> &'static PredicateSpec {
        &PREDICATES[self as usize]
    }
    /// The declared position, `0..18`.
    #[must_use]
    pub fn index(self) -> usize {
        self as usize
    }
}

impl serde::Serialize for PredicateId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.token())
    }
}
impl<'de> serde::Deserialize<'de> for PredicateId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let t = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        Self::from_token(&t)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown predicate token `{t}`")))
    }
}
