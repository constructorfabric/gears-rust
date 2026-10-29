//! The gate readiness manifest (S3-14): one entry per [`GatePort`] naming how the port is bound,
//! and the commercial authority's state. It is a diagnostic, never a readiness condition: the
//! gear is ready with every port unavailable, because missing providers are the designed state
//! on 2026-10-22 (D-227 item 1).
use crate::domain::gate::breaker::BreakerState;
use crate::domain::gate::ports::{Delivery, GatePort};
use serde::{Serialize, Serializer};

/// The most characters the breaker section may add to [`GateReadiness::summary`]: the widest
/// summary without breakers plus the health prefix is 226 of the health endpoint's 256.
const BREAKER_SUMMARY_BUDGET: usize = 30;

/// How one port is bound at init.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortState {
    /// A real client answers the port.
    Bound,
    /// The provider is delivered, but its client is not published in the hub.
    MissingClient,
    /// No provider is delivered: the D-227 package and upstream ask that close the gap.
    ProviderMissing {
        /// The Orders work package that implements the port once the provider lands.
        package: &'static str,
        /// The upstream requirement ID that closes the gap.
        upreq: &'static str,
    },
    /// A development stub answers the port from a fixture (`gate-dev-stubs` feature and
    /// `gate.dev_stubs`): never readiness evidence, never a production binding.
    Stubbed,
}

impl std::fmt::Display for PortState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bound => f.write_str("bound"),
            Self::MissingClient => f.write_str("unavailable (missing client)"),
            Self::ProviderMissing { package, upreq } => {
                write!(f, "unavailable (provider missing: {package}, {upreq})")
            }
            Self::Stubbed => f.write_str("stubbed (development only)"),
        }
    }
}

impl Serialize for PortState {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// The commercial service authority (D-194) the catalog port presents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityState {
    /// `gate.commercial_identities` is configured and the `AuthN` resolver is bound. Every read
    /// exchanges client credentials: the SDK carries no token expiry, so nothing is cached.
    Bound,
    /// No commercial identity is configured: every seller is not provisioned.
    NotProvisioned,
}

impl std::fmt::Display for AuthorityState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Bound => "bound (uncached exchange per read)",
            Self::NotProvisioned => "not provisioned",
        })
    }
}

impl Serialize for AuthorityState {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// One port's manifest entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PortReadiness {
    /// The port's operator token (`current-revision`, `resolve`, …).
    pub token: &'static str,
    /// The owning gear.
    pub owner: &'static str,
    /// How the port is bound.
    pub state: PortState,
    /// The port's breaker state token (`closed`, `open`, `half-open`; D-233); absent when
    /// `gate.breaker` is disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breaker: Option<&'static str>,
}

/// The gate manifest: every port in D-227 table order, and the authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GateReadiness {
    /// One entry per [`GatePort::ALL`] member, in that order.
    pub ports: Vec<PortReadiness>,
    /// The commercial service authority.
    pub authority: AuthorityState,
    /// Development stubs are bound (`gate-dev-stubs`); serialized only when true.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub dev_stubs: bool,
}

impl GateReadiness {
    /// The manifest for a catalog binding (`catalog_bound`: the Pricing and Products clients
    /// are both published) and an authority state. Every undelivered port is provider-missing
    /// with its D-227 package; the delivered ports are the catalog's.
    #[must_use]
    pub fn new(catalog_bound: bool, authority: AuthorityState) -> Self {
        let ports = GatePort::ALL
            .iter()
            .map(|port| PortReadiness {
                token: port.token(),
                owner: port.owner(),
                state: match port.delivery() {
                    Delivery::Available if catalog_bound => PortState::Bound,
                    Delivery::Available => PortState::MissingClient,
                    Delivery::Missing { package, upreq } => {
                        PortState::ProviderMissing { package, upreq }
                    }
                },
                breaker: None,
            })
            .collect();
        Self {
            ports,
            authority,
            dev_stubs: false,
        }
    }

    /// The manifest with each of `stubbed` reported as [`PortState::Stubbed`] and `dev_stubs`
    /// set when any port is stubbed.
    #[must_use]
    pub fn with_stubbed(mut self, stubbed: &[GatePort]) -> Self {
        for entry in &mut self.ports {
            if stubbed.iter().any(|p| p.token() == entry.token) {
                entry.state = PortState::Stubbed;
                self.dev_stubs = true;
            }
        }
        self
    }

    /// The manifest with each of `states` as its port's breaker state; a port not listed has
    /// no breaker (disabled).
    #[must_use]
    pub fn with_breakers(mut self, states: &[(GatePort, BreakerState)]) -> Self {
        for entry in &mut self.ports {
            entry.breaker = states
                .iter()
                .find(|(p, _)| p.token() == entry.token)
                .map(|(_, s)| s.token());
        }
        self
    }

    /// The entry for `port`.
    #[must_use]
    pub fn port(&self, port: GatePort) -> Option<&PortReadiness> {
        self.ports.iter().find(|p| p.token == port.token())
    }

    /// A one-line summary short enough for the readiness message (the health endpoint caps a
    /// message at 256 characters): the catalog state, the authority, the provider-missing count
    /// and, when development stubs are bound, the stubbed count; then each port whose breaker is
    /// not closed (`resolve: breaker open`), or only their count when the names would pass
    /// [`BREAKER_SUMMARY_BUDGET`]. Closed breakers are in the manifest only. The full manifest
    /// is logged at init on `orders.gate`.
    #[must_use]
    pub fn summary(&self) -> String {
        let catalog = self
            .port(GatePort::CurrentRevision)
            .map_or_else(|| "unknown".to_owned(), |p| p.state.to_string());
        let missing = self
            .ports
            .iter()
            .filter(|p| matches!(p.state, PortState::ProviderMissing { .. }))
            .count();
        let stubbed = self
            .ports
            .iter()
            .filter(|p| p.state == PortState::Stubbed)
            .count();
        let stubbed = if stubbed == 0 {
            String::new()
        } else {
            format!(", {stubbed} {}", PortState::Stubbed)
        };
        format!(
            "gate: catalog {catalog}, authority {}, {missing} of {} ports provider-missing{stubbed}{}",
            self.authority,
            self.ports.len(),
            self.breaker_summary()
        )
    }

    /// The summary's breaker section: every port whose breaker is not closed, by name within
    /// [`BREAKER_SUMMARY_BUDGET`], else by count; empty when every breaker is closed or disabled.
    fn breaker_summary(&self) -> String {
        let closed = BreakerState::Closed.token();
        let tripped: Vec<_> = self
            .ports
            .iter()
            .filter_map(|p| p.breaker.filter(|b| *b != closed).map(|b| (p.token, b)))
            .collect();
        let mut named = String::new();
        for (token, state) in &tripped {
            for part in [", ", token, ": breaker ", state] {
                named.push_str(part);
            }
        }
        if named.len() <= BREAKER_SUMMARY_BUDGET {
            named
        } else {
            format!(", breakers not closed: {}", tripped.len())
        }
    }
}
