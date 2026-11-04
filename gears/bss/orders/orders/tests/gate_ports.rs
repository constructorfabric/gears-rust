//! D-231: the port inventory is the code. Every row of the decision's table is asserted here.
use InputPort as I;
use Reason as R;
use bss_orders::domain::gate::ports::{Delivery, GatePort, PortAnswer, Unavailable};
use bss_orders::domain::guards::{ALL_PORTS, InputPort};
use bss_orders_sdk::catalog::Reason;
use std::collections::BTreeSet;
use std::time::Duration;

#[test]
fn every_port_has_one_registered_unavailable_reason_owned_by_an_input_port_or_preview() {
    for port in GatePort::ALL {
        let reason = port.unavailable_reason();
        let owned_by_input = ALL_PORTS
            .iter()
            .any(|p| p.unevaluable_reasons().contains(&reason));
        let preview_only = reason == Reason::IndicativeTaxUnavailable;
        assert!(
            owned_by_input || preview_only,
            "{port:?} → {reason:?} has no owner"
        );
    }
}

#[test]
fn feeds_cover_every_engine_input_port_exactly_as_d227_says() {
    let fed: BTreeSet<InputPort> = GatePort::ALL
        .iter()
        .flat_map(|p| p.feeds().iter().copied())
        .collect();
    let expected: BTreeSet<InputPort> = ALL_PORTS
        .iter()
        .copied()
        .filter(|p| *p != InputPort::AcceptanceRequirement)
        .collect();
    assert_eq!(
        fed, expected,
        "AcceptanceRequirement is 05's port, every other port is fed by the gate"
    );
    assert_eq!(
        GatePort::Resolve.feeds(),
        &[InputPort::CatalogPredicates, InputPort::PinComposition]
    );
    assert_eq!(GatePort::IndicativeTax.feeds(), &[]);
}

#[test]
fn delivery_status_matches_the_2026_10_22_inventory() {
    let available: BTreeSet<GatePort> = GatePort::ALL
        .iter()
        .copied()
        .filter(|p| p.delivery() == Delivery::Available)
        .collect();
    assert_eq!(
        available,
        [
            GatePort::CurrentRevision,
            GatePort::Resolve,
            GatePort::SkuRead
        ]
        .into_iter()
        .collect()
    );
    match GatePort::Evaluation.delivery() {
        Delivery::Missing { package, upreq } => {
            assert_eq!(package, "S3-07");
            assert_eq!(upreq, "cpt-cf-bss-orders-upreq-rating-evaluation");
        }
        Delivery::Available => panic!("Rating has no SDK"),
    }
}

#[test]
fn an_unavailable_answer_names_its_kind_but_resolves_to_the_port_reason() {
    let answer: PortAnswer<u8> = PortAnswer::Unavailable(Unavailable::Missing);
    assert_eq!(
        answer.reason_for(GatePort::Occupancy),
        Some(Reason::OverlapPresenceUnevaluable)
    );
    assert_eq!(
        PortAnswer::Answered(1u8).reason_for(GatePort::Occupancy),
        None
    );
}

#[test]
fn all_has_twelve_distinct_ports() {
    let set: BTreeSet<GatePort> = GatePort::ALL.iter().copied().collect();
    assert_eq!(set.len(), 12);
    assert_eq!(GatePort::ALL.len(), 12);
}

#[test]
fn input_reasons_are_per_input_for_dual_feed_ports() {
    assert_eq!(
        GatePort::Resolve.input_reasons(),
        vec![
            (I::CatalogPredicates, R::CatalogPredicatesUnavailable),
            (I::PinComposition, R::CatalogPinCompositionUnavailable),
        ]
    );
    assert!(GatePort::IndicativeTax.input_reasons().is_empty());
    for port in GatePort::ALL {
        let expected: Vec<(InputPort, Reason)> = port
            .feeds()
            .iter()
            .map(|p| (*p, p.unevaluable_reasons()[0]))
            .collect();
        assert_eq!(port.input_reasons(), expected, "{port:?}");
    }
    let unavailable: PortAnswer<u8> = PortAnswer::Unavailable(Unavailable::Outage);
    assert_eq!(
        unavailable.input_reasons_for(GatePort::Resolve),
        GatePort::Resolve.input_reasons()
    );
    assert!(
        PortAnswer::Answered(1u8)
            .input_reasons_for(GatePort::Resolve)
            .is_empty()
    );
}

type Row = (
    GatePort,
    &'static [InputPort],
    Reason,
    &'static str,
    Delivery,
    u64,
);

const fn missing(package: &'static str, upreq: &'static str) -> Delivery {
    Delivery::Missing { package, upreq }
}

/// The D-231 item 1 table, row for row.
const TABLE: &[Row] = &[
    (
        GatePort::CurrentRevision,
        &[I::PricingRevision],
        R::PricingRevisionUnavailable,
        "pricing",
        Delivery::Available,
        250,
    ),
    (
        GatePort::Resolve,
        &[I::CatalogPredicates, I::PinComposition],
        R::CatalogPredicatesUnavailable,
        "pricing",
        Delivery::Available,
        250,
    ),
    (
        GatePort::SkuRead,
        &[I::CatalogPredicates],
        R::CatalogPredicatesUnavailable,
        "products",
        Delivery::Available,
        250,
    ),
    (
        GatePort::Assessment,
        &[I::CatalogPredicates],
        R::CatalogPredicatesUnavailable,
        "pricing",
        missing(
            "S3-03",
            "cpt-cf-bss-orders-upreq-pricing-purchase-assessment",
        ),
        250,
    ),
    (
        GatePort::BillingTerms,
        &[I::PinComposition],
        R::CatalogPinCompositionUnavailable,
        "subscriptions",
        missing(
            "S3-04",
            "cpt-cf-bss-orders-upreq-pre-subscription-billing-terms",
        ),
        250,
    ),
    (
        GatePort::AcceptancePolicy,
        &[I::PinComposition],
        R::CatalogPinCompositionUnavailable,
        "pricing",
        missing("S3-05", "cpt-cf-bss-orders-upreq-pricing-acceptance-policy"),
        250,
    ),
    (
        GatePort::IdentityProfile,
        &[I::Identity],
        R::IdentityPartyUnavailable,
        "account-management",
        missing("S4-02", "cpt-cf-bss-orders-upreq-payer-commercial-profile"),
        250,
    ),
    (
        GatePort::ContractResolution,
        &[I::ContractResolution],
        R::ContractResolutionUnavailable,
        "contracts",
        missing(
            "S4-02",
            "cpt-cf-bss-orders-upreq-contract-party-eligibility",
        ),
        250,
    ),
    (
        GatePort::OverlapKey,
        &[I::OverlapKey],
        R::OverlapKeyUnavailable,
        "subscriptions",
        missing(
            "S5-06a",
            "cpt-cf-bss-orders-upreq-catalog-subscription-product-key",
        ),
        250,
    ),
    (
        GatePort::Occupancy,
        &[I::Occupancy],
        R::OverlapPresenceUnevaluable,
        "subscriptions",
        missing("S5-06a", "cpt-cf-bss-orders-upreq-overlap-presence-read"),
        250,
    ),
    (
        GatePort::Evaluation,
        &[I::Evaluation],
        R::EvaluationUnavailable,
        "rating",
        missing("S3-07", "cpt-cf-bss-orders-upreq-rating-evaluation"),
        500,
    ),
    (
        GatePort::IndicativeTax,
        &[],
        R::IndicativeTaxUnavailable,
        "unowned",
        missing("S3-08", "cpt-cf-bss-orders-upreq-indicative-tax-read"),
        250,
    ),
];

#[test]
fn the_whole_d227_table_is_pinned_row_by_row() {
    let ports: Vec<GatePort> = TABLE.iter().map(|r| r.0).collect();
    assert_eq!(ports, GatePort::ALL.to_vec(), "table order");
    for (port, feeds, reason, owner, delivery, ms) in TABLE {
        let port = *port;
        assert_eq!(port.feeds(), *feeds, "{port:?} feeds");
        assert_eq!(port.unavailable_reason(), *reason, "{port:?} reason");
        assert_eq!(port.owner(), *owner, "{port:?} owner");
        assert_eq!(port.delivery(), *delivery, "{port:?} delivery");
        assert_eq!(
            port.baseline_deadline(),
            Duration::from_millis(*ms),
            "{port:?} deadline"
        );
    }
}

#[test]
fn port_tokens_are_unique_and_kebab_case() {
    let tokens: BTreeSet<&str> = GatePort::ALL.iter().map(|p| p.token()).collect();
    assert_eq!(tokens.len(), GatePort::ALL.len());
    assert!(
        tokens
            .iter()
            .all(|t| { !t.is_empty() && t.bytes().all(|b| b.is_ascii_lowercase() || b == b'-') })
    );
}

#[test]
fn all_is_in_declaration_order_so_discriminants_index_per_port_tables() {
    for (i, port) in GatePort::ALL.into_iter().enumerate() {
        assert_eq!(port as usize, i, "{port:?}");
    }
}

#[test]
fn every_port_has_a_listed_baseline_deadline() {
    for port in GatePort::ALL {
        let expected = if port == GatePort::Evaluation {
            500
        } else {
            250
        };
        assert_eq!(
            port.baseline_deadline(),
            Duration::from_millis(expected),
            "{port:?} baseline deadline"
        );
    }
}
