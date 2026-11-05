//! The S3-14 composition root without the gear: an empty hub binds every port fail-closed and
//! the manifest names each missing provider.
//! In-crate: runs in the `orders-pg` CI lane with `gate-dev-stubs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once"
)]
use crate::config::GateConfig;
use crate::domain::gate::breaker::BreakerState;
use crate::domain::gate::ports::{Delivery, GatePort};
use crate::infra::gate::GatePorts;
use crate::infra::gate::budget::Budget;
use crate::infra::gate::readiness::{AuthorityState, GateReadiness, PortState};
use crate::infra::gate::signals::NoGateSignals;
use std::sync::Arc;

fn bind(config: &GateConfig) -> anyhow::Result<GatePorts> {
    let budget = Arc::new(Budget::from_config(config, Arc::new(NoGateSignals)));
    GatePorts::from_hub(&toolkit::ClientHub::new(), config, budget)
}

/// The health message blocklist of the toolkit (`SUSPICIOUS_SUBSTRINGS`,
/// `libs/toolkit/src/healthcheck.rs`; private there, so copied): a message containing any of
/// these, case-insensitively, collapses to "health check failed".
const HEALTH_BLOCKLIST: [&str; 23] = [
    "password",
    "passwd",
    "secret",
    "token",
    "bearer",
    "authorization",
    "api_key",
    "apikey",
    "private_key",
    "credential",
    "access_key",
    "aws_",
    "client_secret",
    "postgres://",
    "postgresql://",
    "mysql://",
    "sqlite://",
    "mongodb://",
    "redis://",
    "amqp://",
    "jdbc:",
    "panic",
    "stack backtrace",
];

#[test]
fn an_empty_hub_binds_every_port_fail_closed_and_names_each_missing_provider() {
    let readiness = bind(&GateConfig::default()).unwrap().readiness();
    assert_eq!(readiness.authority, AuthorityState::NotProvisioned);
    assert_eq!(readiness.ports.len(), GatePort::ALL.len());
    for (entry, port) in readiness.ports.iter().zip(GatePort::ALL) {
        assert_eq!(entry.token, port.token());
        assert_eq!(entry.owner, port.owner());
        match port.delivery() {
            Delivery::Available => assert_eq!(entry.state, PortState::MissingClient),
            Delivery::Missing { package, upreq } => {
                assert_eq!(entry.state, PortState::ProviderMissing { package, upreq });
            }
        }
    }
    let manifest = serde_json::to_value(&readiness).unwrap();
    assert_eq!(manifest["authority"], "not provisioned");
    assert_eq!(manifest["ports"][0]["token"], "current-revision");
    assert_eq!(
        manifest["ports"][0]["state"],
        "unavailable (missing client)"
    );
    let evaluation = manifest["ports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["token"] == "evaluation")
        .expect("the manifest has the evaluation port");
    assert_eq!(
        evaluation["state"],
        "unavailable (provider missing: S3-07, cpt-cf-bss-orders-upreq-rating-evaluation)"
    );
    assert_eq!(
        readiness.summary(),
        "gate: catalog unavailable (missing client), authority not provisioned, 9 of 12 ports \
         provider-missing"
    );
}

/// The widest summary stays under the health message cap (256) with the ready prefix, and
/// carries none of the toolkit sanitizer's collapse words.
#[test]
fn the_gate_summary_fits_the_health_message() {
    // Development stubs (Task 8b) stub every undelivered port: the widest stubbed summary.
    let undelivered: Vec<GatePort> = GatePort::ALL
        .into_iter()
        .filter(|p| p.delivery() != Delivery::Available)
        .collect();
    // Every breaker half-open (the widest token) on every port: the breaker section's widest.
    let all_half_open: Vec<(GatePort, BreakerState)> = GatePort::ALL
        .into_iter()
        .map(|p| (p, BreakerState::HalfOpen))
        .collect();
    let one_half_open = [(GatePort::CurrentRevision, BreakerState::HalfOpen)];
    // The widest *named* breaker section: `, resolve: breaker half-open` (28 of the 30-character
    // budget), so it is named, not counted.
    let resolve_half_open = [(GatePort::Resolve, BreakerState::HalfOpen)];
    for (catalog, authority, stubbed, breakers) in [
        (false, AuthorityState::Bound, &[][..], &[][..]),
        (true, AuthorityState::Bound, &[][..], &[][..]),
        (false, AuthorityState::NotProvisioned, &[][..], &[][..]),
        (false, AuthorityState::Bound, &undelivered[..], &[][..]),
        (
            false,
            AuthorityState::Bound,
            &undelivered[..],
            &all_half_open[..],
        ),
        (
            false,
            AuthorityState::Bound,
            &undelivered[..],
            &one_half_open[..],
        ),
        // The widest base (catalog unbound, authority not provisioned, every undelivered port
        // stubbed) with the widest named breaker section.
        (
            false,
            AuthorityState::NotProvisioned,
            &undelivered[..],
            &resolve_half_open[..],
        ),
    ] {
        let message = format!(
            "draft capture and draft reads are ready; later packages answer unavailable; {}",
            GateReadiness::new(catalog, authority)
                .with_stubbed(stubbed)
                .with_breakers(breakers)
                .summary()
        );
        assert!(message.len() <= 256, "{message}");
        if breakers == &resolve_half_open[..] {
            assert!(
                message.ends_with(", resolve: breaker half-open"),
                "{message}"
            );
        }
        let lower = message.to_lowercase();
        for word in HEALTH_BLOCKLIST {
            assert!(!lower.contains(word), "{word}: {message}");
        }
    }
}

#[test]
fn configured_identities_without_a_resolver_refuse_to_bind() {
    let config: GateConfig = serde_json::from_value(serde_json::json!({
        "commercial_identities": [{
            "seller_tenant_id": "00000000-0000-0000-0000-000000005e11",
            "client_id": "orders-gate",
            "client_secret": "fixture"
        }]
    }))
    .unwrap();
    let error = bind(&config).unwrap_err().to_string();
    assert!(error.contains("gate.commercial_identities"), "{error}");
}

/// The catalog binds late: unbound at init over an empty hub, bound once both clients are
/// published and `bind_from_hub` runs, and the manifest reads the live binding.
#[test]
fn the_catalog_binds_when_both_clients_are_published() {
    use crate::infra::gate::tests::doubles::{FakePricing, FakeProducts};
    let hub = toolkit::ClientHub::new();
    let config = GateConfig::default();
    let ports = GatePorts::from_hub(
        &hub,
        &config,
        Arc::new(Budget::from_config(&config, Arc::new(NoGateSignals))),
    )
    .unwrap();
    assert!(!ports.catalog.bind_from_hub(&hub));
    hub.register::<dyn bss_pricing_sdk::read::PricingReadV1>(Arc::new(FakePricing::new(
        bss_pricing_sdk::read::RevisionRef {
            plan_id: uuid::Uuid::from_u128(1),
            revision_id: uuid::Uuid::from_u128(2),
            revision_no: 1,
        },
        Vec::new(),
    )));
    assert!(
        !ports.catalog.bind_from_hub(&hub),
        "Products is still missing"
    );
    assert_eq!(
        ports.readiness().port(GatePort::Resolve).map(|p| p.state),
        Some(PortState::MissingClient)
    );
    hub.register::<dyn bss_products_sdk::api::ProductsClient>(Arc::new(FakeProducts::default()));
    assert!(ports.catalog.bind_from_hub(&hub));
    for port in [
        GatePort::CurrentRevision,
        GatePort::Resolve,
        GatePort::SkuRead,
    ] {
        assert_eq!(
            ports.readiness().port(port).map(|p| p.state),
            Some(PortState::Bound)
        );
    }
}

#[test]
fn the_manifest_shows_each_enabled_breaker_state() {
    let readiness = GateReadiness::new(true, AuthorityState::Bound)
        .with_breakers(&[(GatePort::Resolve, BreakerState::Open)]);
    assert_eq!(
        readiness.port(GatePort::Resolve).and_then(|p| p.breaker),
        Some("open")
    );
    for port in GatePort::ALL
        .into_iter()
        .filter(|p| *p != GatePort::Resolve)
    {
        assert_eq!(
            readiness.port(port).and_then(|p| p.breaker),
            None,
            "{port:?}"
        );
    }
    let summary = readiness.summary();
    assert!(summary.ends_with(", resolve: breaker open"), "{summary}");
    let manifest = serde_json::to_value(&readiness).unwrap();
    let ports = manifest["ports"].as_array().unwrap();
    let by_token = |token: &str| {
        ports
            .iter()
            .find(|p| p["token"] == token)
            .unwrap_or_else(|| panic!("the manifest has the {token} port"))
    };
    assert_eq!(by_token("resolve")["breaker"], "open");
    assert!(by_token("current-revision").get("breaker").is_none());
}
