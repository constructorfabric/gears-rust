//! The upstream fact families that enter the inbox. A leaf module: the error type and the
//! metrics port name a source without depending on the inbox itself.

use std::fmt;

use toolkit_macros::domain_model;

/// The upstream fact families that enter the inbox (DESIGN §3.7 `bss_rating__inbox.source`).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum InboxSource {
    /// A Subscriptions commercial fact version.
    Fact,
    /// A Subscriptions attribution segment version.
    Segment,
    /// A Subscriptions usage scope version.
    Scope,
    /// A usage emitter coverage declaration.
    Coverage,
    /// A Subscriptions subscription version (drives derived coverage, T-D-82).
    SubscriptionVersion,
    /// A Billing period hint (observational).
    BillingHint,
}

impl InboxSource {
    /// Every source, in a stable order.
    pub const ALL: [Self; 6] = [
        Self::Fact,
        Self::Segment,
        Self::Scope,
        Self::Coverage,
        Self::SubscriptionVersion,
        Self::BillingHint,
    ];

    /// The stored and wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Segment => "segment",
            Self::Scope => "scope",
            Self::Coverage => "coverage",
            Self::SubscriptionVersion => "subscription_version",
            Self::BillingHint => "billing_hint",
        }
    }

    /// The source a stored or configured name denotes.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == name)
    }
}

impl fmt::Display for InboxSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
