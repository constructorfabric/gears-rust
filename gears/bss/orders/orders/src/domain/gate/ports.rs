//! The outbound inventory (D-227 item 1) and typed unavailability (item 2).
use crate::domain::guards::InputPort;
use bss_orders_sdk::catalog::Reason;
use std::time::Duration;
use toolkit_macros::domain_model;

/// One logical outbound operation of the assessment (F03 §3.3; D-227 table order).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GatePort {
    /// Pricing: the current pricing revision.
    CurrentRevision,
    /// Pricing: catalog resolve.
    Resolve,
    /// Products: SKU read.
    SkuRead,
    /// Pricing: purchase assessment.
    Assessment,
    /// Subscriptions: pre-subscription billing terms.
    BillingTerms,
    /// Pricing: acceptance policy.
    AcceptancePolicy,
    /// Account management: payer commercial profile.
    IdentityProfile,
    /// Contracts: party eligibility resolution.
    ContractResolution,
    /// Subscriptions: catalog subscription product key.
    OverlapKey,
    /// Subscriptions: overlap presence read.
    Occupancy,
    /// Rating: evaluation.
    Evaluation,
    /// Unowned: indicative tax read (preview only).
    IndicativeTax,
}

/// Whether a real provider exists on 2026-10-22 (D-227 item 1, "Status").
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// A real provider exists.
    Available,
    /// The implementing package and the upstream ask that closes it. Only the first ask is
    /// carried; the rest (the assessment row's `…-upreq-pricing-bundle-sellability`, the grant
    /// upreqs of the three available rows) live in the D-227 table.
    Missing {
        /// The Orders work package that implements the port once the provider lands.
        package: &'static str,
        /// The upstream requirement ID that closes the gap.
        upreq: &'static str,
    },
}

/// Why a port did not answer (operator diagnostics; never public). `Missing` is the production
/// answer of an undelivered provider: fail closed, never pass.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    /// The provider failed or was unreachable.
    Outage,
    /// The provider refused the call.
    Denied,
    /// The per-run deadline elapsed.
    Deadline,
    /// The provider answered with a value that does not decode.
    Malformed,
    /// No provider is delivered.
    Missing,
}

impl Unavailable {
    /// The `kind` label value of the gate's unavailable-port metric.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Outage => "outage",
            Self::Denied => "denied",
            Self::Deadline => "deadline",
            Self::Malformed => "malformed",
            Self::Missing => "missing",
        }
    }
}

/// A port's answer: the typed value, or unavailability that maps to the port's one reason.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortAnswer<T> {
    /// The typed answer.
    Answered(T),
    /// A dependent call was skipped; retain its prerequisite reason.
    Skipped(Reason),
    /// The port did not answer.
    Unavailable(Unavailable),
}

impl<T> PortAnswer<T> {
    /// The registered reason an unavailable answer carries for `port`; `None` when answered.
    #[must_use]
    pub fn reason_for(&self, port: GatePort) -> Option<Reason> {
        match self {
            Self::Answered(_) => None,
            Self::Unavailable(_) => Some(port.unavailable_reason()),
            Self::Skipped(reason) => Some(*reason),
        }
    }

    /// The per-input reasons an unavailable answer carries for `port` (D-227 item 1 rule);
    /// empty when answered.
    #[must_use]
    pub fn input_reasons_for(&self, port: GatePort) -> Vec<(InputPort, Reason)> {
        match self {
            Self::Answered(_) => Vec::new(),
            Self::Unavailable(_) => port.input_reasons(),
            Self::Skipped(reason) => port.feeds().iter().map(|p| (*p, *reason)).collect(),
        }
    }

    /// The answered value, if any.
    #[must_use]
    pub fn answered(&self) -> Option<&T> {
        match self {
            Self::Answered(v) => Some(v),
            Self::Unavailable(_) | Self::Skipped(_) => None,
        }
    }
}

impl GatePort {
    /// Every port, in D-227 table order.
    pub const ALL: [Self; 12] = [
        Self::CurrentRevision,
        Self::Resolve,
        Self::SkuRead,
        Self::Assessment,
        Self::BillingTerms,
        Self::AcceptancePolicy,
        Self::IdentityProfile,
        Self::ContractResolution,
        Self::OverlapKey,
        Self::Occupancy,
        Self::Evaluation,
        Self::IndicativeTax,
    ];

    /// The engine input ports this operation feeds (D-227 item 1, "Feeds").
    #[must_use]
    pub fn feeds(self) -> &'static [InputPort] {
        use InputPort as P;
        match self {
            Self::CurrentRevision => &[P::PricingRevision],
            Self::Resolve => &[P::CatalogPredicates, P::PinComposition],
            Self::SkuRead | Self::Assessment => &[P::CatalogPredicates],
            Self::BillingTerms | Self::AcceptancePolicy => &[P::PinComposition],
            Self::IdentityProfile => &[P::Identity],
            Self::ContractResolution => &[P::ContractResolution],
            Self::OverlapKey => &[P::OverlapKey],
            Self::Occupancy => &[P::Occupancy],
            Self::Evaluation => &[P::Evaluation],
            Self::IndicativeTax => &[],
        }
    }

    /// The port's primary unavailable reason, for logs and readiness (DESIGN
    /// `#contract-03-3-3`). A port feeding two engine inputs (`resolve`) reports one reason per
    /// input; [`Self::input_reasons`] is the form the assessment uses to mark engine inputs
    /// unresolvable (D-227 item 1 rule).
    #[must_use]
    pub fn unavailable_reason(self) -> Reason {
        match self {
            Self::CurrentRevision => Reason::PricingRevisionUnavailable,
            Self::Resolve | Self::SkuRead | Self::Assessment => {
                Reason::CatalogPredicatesUnavailable
            }
            Self::BillingTerms | Self::AcceptancePolicy => Reason::CatalogPinCompositionUnavailable,
            Self::IdentityProfile => Reason::IdentityPartyUnavailable,
            Self::ContractResolution => Reason::ContractResolutionUnavailable,
            Self::OverlapKey => Reason::OverlapKeyUnavailable,
            Self::Occupancy => Reason::OverlapPresenceUnevaluable,
            Self::Evaluation => Reason::EvaluationUnavailable,
            Self::IndicativeTax => Reason::IndicativeTaxUnavailable,
        }
    }

    /// One `(input, reason)` per engine input this port feeds, the reason being that input's
    /// registered unevaluable reason (D-227 item 1 rule); empty for `IndicativeTax`.
    #[must_use]
    pub fn input_reasons(self) -> Vec<(InputPort, Reason)> {
        self.feeds()
            .iter()
            .map(|p| (*p, p.unevaluable_reasons()[0]))
            .collect()
    }

    /// The owning gear, for logs and the readiness manifest (S3-14).
    #[must_use]
    pub fn owner(self) -> &'static str {
        match self {
            Self::CurrentRevision | Self::Resolve | Self::Assessment | Self::AcceptancePolicy => {
                "pricing"
            }
            Self::SkuRead => "products",
            Self::BillingTerms | Self::OverlapKey | Self::Occupancy => "subscriptions",
            Self::IdentityProfile => "account-management",
            Self::ContractResolution => "contracts",
            Self::Evaluation => "rating",
            Self::IndicativeTax => "unowned",
        }
    }

    /// Whether a real provider exists, and when not, the package and upstream ask that close it.
    #[must_use]
    pub fn delivery(self) -> Delivery {
        let missing = |package, upreq| Delivery::Missing { package, upreq };
        match self {
            Self::CurrentRevision | Self::Resolve | Self::SkuRead => Delivery::Available,
            Self::Assessment => missing(
                "S3-03",
                "cpt-cf-bss-orders-upreq-pricing-purchase-assessment",
            ),
            Self::BillingTerms => missing(
                "S3-04",
                "cpt-cf-bss-orders-upreq-pre-subscription-billing-terms",
            ),
            Self::AcceptancePolicy => {
                missing("S3-05", "cpt-cf-bss-orders-upreq-pricing-acceptance-policy")
            }
            Self::IdentityProfile => {
                missing("S4-02", "cpt-cf-bss-orders-upreq-payer-commercial-profile")
            }
            Self::ContractResolution => missing(
                "S4-02",
                "cpt-cf-bss-orders-upreq-contract-party-eligibility",
            ),
            Self::OverlapKey => missing(
                "S5-06a",
                "cpt-cf-bss-orders-upreq-catalog-subscription-product-key",
            ),
            Self::Occupancy => missing("S5-06a", "cpt-cf-bss-orders-upreq-overlap-presence-read"),
            Self::Evaluation => missing("S3-07", "cpt-cf-bss-orders-upreq-rating-evaluation"),
            Self::IndicativeTax => missing("S3-08", "cpt-cf-bss-orders-upreq-indicative-tax-read"),
        }
    }

    /// The port a stable operator token names; `None` for any other spelling.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.token() == token)
    }

    /// The stable operator token naming this port in configuration and logs (kebab-case, unique).
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            Self::CurrentRevision => "current-revision",
            Self::Resolve => "resolve",
            Self::SkuRead => "sku-read",
            Self::Assessment => "assessment",
            Self::BillingTerms => "billing-terms",
            Self::AcceptancePolicy => "acceptance-policy",
            Self::IdentityProfile => "identity-profile",
            Self::ContractResolution => "contract-resolution",
            Self::OverlapKey => "overlap-key",
            Self::Occupancy => "occupancy",
            Self::Evaluation => "evaluation",
            Self::IndicativeTax => "indicative-tax",
        }
    }

    /// F03 §3.3 baseline per-run deadline (unratified, D-227 item 5); config may override.
    #[must_use]
    pub fn baseline_deadline(self) -> Duration {
        match self {
            Self::Evaluation => Duration::from_millis(500),
            Self::CurrentRevision
            | Self::Resolve
            | Self::SkuRead
            | Self::Assessment
            | Self::BillingTerms
            | Self::AcceptancePolicy
            | Self::IdentityProfile
            | Self::ContractResolution
            | Self::OverlapKey
            | Self::Occupancy
            | Self::IndicativeTax => Duration::from_millis(250),
        }
    }
}

/// A port is configured by its token (`gate.deadline_ms` keys); an unknown token fails at parse
/// and lists the valid ones.
impl<'de> serde::Deserialize<'de> for GatePort {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let token = String::deserialize(deserializer)?;
        Self::from_token(&token).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "unknown port `{token}`; valid: {}",
                Self::ALL.map(Self::token).join(", ")
            ))
        })
    }
}
