//! The closed status sets of the idempotency registry, commercial-attempt and
//! fulfillment-control rows, with the stored token of each member.
//!
//! The tokens are exactly what the database CHECK constraints admit
//! (`ck_bss_orders__idempotency__0`, `ck_bss_orders__commercial_attempt__1`,
//! `ck_bss_orders__fulfillment_control__1`), and each `UNRESOLVED` set is the nonterminal half of
//! the lease/terminal CHECK (`ck_bss_orders__commercial_attempt__6`,
//! `ck_bss_orders__fulfillment_control__3`). `orders/tests/storage_status.rs` checks both against
//! `schema-inventory.json`. The entity columns stay `String`: a stored token outside the set is
//! an invariant violation each reader reports, not a decode failure.

/// Status of an idempotency registry row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryStatus {
    /// Claimed and not yet settled; carries a lease.
    InFlight,
    /// Settled with a replayable response.
    Settled,
}

impl RegistryStatus {
    /// Every member, in the CHECK constraint's order.
    pub const ALL: [Self; 2] = [Self::InFlight, Self::Settled];

    /// The stored token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::InFlight => "in_flight",
            Self::Settled => "settled",
        }
    }

    /// The member a stored token names, if any.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.token() == token)
    }
}

/// Status of a D-188 commercial attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptStatus {
    /// Intent persisted; no receipt yet.
    Prepared,
    /// At least one receipt recorded.
    Running,
    /// Terminal: the candidate version committed.
    Committed,
    /// Terminal: business refusal.
    Refused,
    /// Terminal: conditionally abandoned.
    Abandoned,
}

impl AttemptStatus {
    /// Every member, in the CHECK constraint's order.
    pub const ALL: [Self; 5] = [
        Self::Prepared,
        Self::Running,
        Self::Committed,
        Self::Refused,
        Self::Abandoned,
    ];
    /// The nonterminal members: an attempt in one of these still owns its execution.
    pub const UNRESOLVED: [Self; 2] = [Self::Prepared, Self::Running];

    /// The stored token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Running => "running",
            Self::Committed => "committed",
            Self::Refused => "refused",
            Self::Abandoned => "abandoned",
        }
    }

    /// The member a stored token names, if any.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.token() == token)
    }

    /// Whether the attempt is still unresolved.
    #[must_use]
    pub fn is_unresolved(self) -> bool {
        Self::UNRESOLVED.contains(&self)
    }

    /// The stored tokens of [`Self::UNRESOLVED`], for a status filter.
    #[must_use]
    pub fn unresolved_tokens() -> [&'static str; 2] {
        Self::UNRESOLVED.map(Self::token)
    }
}

/// Status of a D-198 fulfillment control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlStatus {
    /// Intent persisted; receivers not yet paused.
    Prepared,
    /// Waiting for receiver evidence.
    Awaiting,
    /// Every receiver reached the barrier.
    BarrierReady,
    /// Terminal: the control settled.
    Settled,
    /// Terminal: business refusal.
    Refused,
    /// Terminal: conditionally abandoned.
    Abandoned,
}

impl ControlStatus {
    /// Every member, in the CHECK constraint's order.
    pub const ALL: [Self; 6] = [
        Self::Prepared,
        Self::Awaiting,
        Self::BarrierReady,
        Self::Settled,
        Self::Refused,
        Self::Abandoned,
    ];
    /// The nonterminal members, in progress order: a control in one of these still owns its
    /// execution and blocks a new one on the order.
    pub const UNRESOLVED: [Self; 3] = [Self::Prepared, Self::Awaiting, Self::BarrierReady];

    /// The stored token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Awaiting => "awaiting",
            Self::BarrierReady => "barrier_ready",
            Self::Settled => "settled",
            Self::Refused => "refused",
            Self::Abandoned => "abandoned",
        }
    }

    /// The member a stored token names, if any.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.token() == token)
    }

    /// Whether the control is still unresolved.
    #[must_use]
    pub fn is_unresolved(self) -> bool {
        Self::UNRESOLVED.contains(&self)
    }

    /// Position in the nonterminal progress order; `None` for a terminal status, which only
    /// final settlement writes. Nonterminal progress never moves to a lower rank.
    #[must_use]
    pub fn rank(self) -> Option<usize> {
        Self::UNRESOLVED.iter().position(|s| *s == self)
    }

    /// The stored tokens of [`Self::UNRESOLVED`], for a status filter.
    #[must_use]
    pub fn unresolved_tokens() -> [&'static str; 3] {
        Self::UNRESOLVED.map(Self::token)
    }
}
