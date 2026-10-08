#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;

fn u(n: u128) -> Uuid {
    Uuid::from_u128(n)
}
fn key(k: &str) -> OverlapScopeKey {
    OverlapScopeKey::try_from(k.to_owned()).unwrap()
}
fn tuple(payer: u128, resource: u128, k: &str) -> ClaimTuple {
    ClaimTuple {
        payer_tenant_id: u(payer),
        resource_tenant_id: u(resource),
        overlap_scope_key: key(k),
    }
}
fn held(id: u128, payer: u128, resource: u128, k: &str) -> HeldClaim {
    HeldClaim {
        claim_id: u(id),
        tuple: tuple(payer, resource, k),
    }
}
fn proposal(payer: u128, keys: &[&str]) -> ProposedClaims {
    ProposedClaims::new(u(payer), u(10), keys.iter().map(|k| key(k))).unwrap()
}

#[test]
fn keys_are_stored_as_received_and_only_unstorable_or_empty_text_is_refused() {
    for bad in ["", "a\0b"] {
        assert_eq!(
            OverlapScopeKey::try_from(bad.to_owned()),
            Err(ClaimRuleError::InvalidKey)
        );
    }
    // No trimming, case folding or normalization: these are three distinct owner keys.
    let keys = [" sku-1", "SKU-1", "sku-1"].map(key);
    assert_eq!(keys[0].as_str(), " sku-1");
    assert_eq!(keys.iter().collect::<BTreeSet<_>>().len(), 3);
}

#[test]
fn total_order_is_payer_bytes_then_resource_bytes_then_key_bytes() {
    // Byte order, not numeric or locale order: 0x01.. sorts before 0xff.. in the first byte.
    let low = Uuid::from_bytes([1; 16]);
    let high = Uuid::from_bytes([0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(low.as_bytes() < high.as_bytes());
    let t = |payer, resource, k: &str| ClaimTuple {
        payer_tenant_id: payer,
        resource_tenant_id: resource,
        overlap_scope_key: key(k),
    };
    let mut set = vec![
        t(high, low, "a"),
        t(low, high, "a"),
        t(low, low, "b"),
        t(low, low, "B"),
        t(low, low, "\u{e9}"),
        t(low, low, "z"),
    ];
    set.sort();
    // "B" (0x42) < "b" (0x62) < "z" (0x7a) < "\u{e9}" (0xc3 0xa9): UTF-8 bytes, not collation.
    let expected = vec![
        t(low, low, "B"),
        t(low, low, "b"),
        t(low, low, "z"),
        t(low, low, "\u{e9}"),
        t(low, high, "a"),
        t(high, low, "a"),
    ];
    assert_eq!(set, expected);
    for pair in expected.windows(2) {
        let bytes = |x: &ClaimTuple| {
            let mut v = x.payer_tenant_id.as_bytes().to_vec();
            v.extend_from_slice(x.resource_tenant_id.as_bytes());
            v.extend_from_slice(x.overlap_scope_key.as_str().as_bytes());
            v
        };
        assert!(bytes(&pair[0]) < bytes(&pair[1]));
    }
}

#[test]
fn duplicate_line_keys_offer_one_tuple_in_the_same_order_whatever_the_caller_order() {
    let forward = proposal(30, &["k2", "k1", "k2", "k3", "k1"]);
    let reversed = proposal(30, &["k1", "k3", "k2", "k1", "k2"]);
    assert_eq!(forward, reversed);
    let offered: Vec<_> = forward
        .tuples()
        .map(|t| t.overlap_scope_key.as_str())
        .collect();
    assert_eq!(offered, ["k1", "k2", "k3"]);
    assert!(
        forward
            .tuples()
            .all(|t| t.payer_tenant_id == u(30) && t.resource_tenant_id == u(10))
    );
    assert_eq!(
        ProposedClaims::new(u(30), u(10), []),
        Err(ClaimRuleError::EmptyProposal)
    );
}

#[test]
fn partition_compares_full_tuples_and_never_reoffers_a_held_tuple() {
    // Unchanged tuples: nothing missing, nothing superseded.
    let p = plan(&[held(1, 30, 10, "k1")], &proposal(30, &["k1"])).unwrap();
    assert_eq!(
        p,
        ClaimPlan {
            retained: vec![u(1)],
            missing: vec![],
            superseded: vec![]
        }
    );
    // Payer-only change: the same key under a new payer is a replacement, not retention.
    let p = plan(&[held(1, 30, 10, "k1")], &proposal(31, &["k1"])).unwrap();
    assert_eq!(p.retained, Vec::<Uuid>::new());
    assert_eq!(p.missing, vec![tuple(31, 10, "k1")]);
    assert_eq!(p.superseded, vec![u(1)]);
    // Payer-plus-key change.
    let p = plan(&[held(1, 30, 10, "k1")], &proposal(31, &["k2"])).unwrap();
    assert_eq!(p.missing, vec![tuple(31, 10, "k2")]);
    assert_eq!(p.superseded, vec![u(1)]);
    // Mixed: retain the intersection, acquire the rest in order, supersede the remainder.
    let p = plan(
        &[held(1, 30, 10, "k3"), held(2, 30, 10, "k1")],
        &proposal(30, &["k4", "k1", "k2"]),
    )
    .unwrap();
    assert_eq!(p.retained, vec![u(2)]);
    assert_eq!(p.missing, vec![tuple(30, 10, "k2"), tuple(30, 10, "k4")]);
    assert_eq!(p.superseded, vec![u(1)]);
    // The same key under another resource tenant is a distinct tuple (D-179).
    let p = plan(&[held(1, 30, 11, "k1")], &proposal(30, &["k1"])).unwrap();
    assert_eq!(p.missing, vec![tuple(30, 10, "k1")]);
    assert_eq!(p.superseded, vec![u(1)]);
    // A repeated live tuple is an integrity failure, never silently deduplicated.
    assert_eq!(
        plan(
            &[held(1, 30, 10, "k1"), held(2, 30, 10, "k1")],
            &proposal(30, &["k1"])
        ),
        Err(ClaimRuleError::DuplicateHeldTuple)
    );
}

#[test]
fn every_terminal_target_releases_all_and_only_submit_and_amendment_acquire() {
    for &trigger in Trigger::ALL {
        for &target in OrderState::ALL {
            let with_keys =
                ClaimDirective::for_transition(trigger, target, Some(proposal(30, &["k"])));
            let without = ClaimDirective::for_transition(trigger, target, None);
            if is_terminal(target) {
                assert_eq!(
                    without,
                    Ok(ClaimDirective::ReleaseAll),
                    "{trigger:?}->{target:?}"
                );
                assert_eq!(with_keys, Err(ClaimRuleError::TerminalWithKeys));
            } else if matches!(trigger, Trigger::Submit | Trigger::Amendment) {
                assert_eq!(with_keys, Ok(ClaimDirective::Replace(proposal(30, &["k"]))));
                assert_eq!(without, Err(ClaimRuleError::AcquiringRowWithoutKeys));
            } else {
                assert_eq!(
                    without,
                    Ok(ClaimDirective::Retain),
                    "{trigger:?}->{target:?}"
                );
                assert_eq!(with_keys, Err(ClaimRuleError::KeysOnNonAcquiringRow));
            }
        }
    }
    assert_eq!(
        TERMINAL_STATES.to_vec(),
        OrderState::ALL
            .iter()
            .copied()
            .filter(|s| matches!(
                s,
                OrderState::Completed
                    | OrderState::Rejected
                    | OrderState::Cancelled
                    | OrderState::FulfillmentFailed
                    | OrderState::Expired
            ))
            .collect::<Vec<_>>()
    );
    // D-182: the forced terminal releases Orders claims through the ordinary terminal rule;
    // the directive has no receiver-capacity variant at all.
    assert_eq!(
        ClaimDirective::for_transition(
            Trigger::ForceFailUnreconciled,
            OrderState::FulfillmentFailed,
            None
        ),
        Ok(ClaimDirective::ReleaseAll)
    );
    // In-flight non-terminal rows keep their claims (hold/resume/begin-fulfillment).
    for (trigger, target) in [
        (Trigger::Hold, OrderState::OnHold),
        (Trigger::Resume, OrderState::Approved),
        (Trigger::BeginFulfillment, OrderState::InFulfillment),
    ] {
        assert_eq!(
            ClaimDirective::for_transition(trigger, target, None),
            Ok(ClaimDirective::Retain)
        );
    }
}

#[test]
fn conflict_is_order_in_flight_for_key_and_replaces_predicate_nine() {
    let conflict = OverlapConflict {
        blocked: vec![BlockedTuple {
            tuple: tuple(30, 10, "k"),
            visible_holder: Some(u(2)),
        }],
        provisional_released: vec![u(5)],
    };
    assert_eq!(OverlapConflict::REASON, Reason::OrderInFlightForKey);
    let replacement = conflict.predicate_nine();
    assert_eq!(replacement.reason, Reason::OrderInFlightForKey);
    assert_eq!(replacement.blocked, conflict.blocked.as_slice());
}
