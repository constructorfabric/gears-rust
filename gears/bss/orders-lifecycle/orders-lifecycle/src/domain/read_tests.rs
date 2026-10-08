#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use bss_orders_lifecycle_sdk::catalog::OrderState;
use bss_orders_lifecycle_sdk::reads::CURSOR_MAX_LEN;
use toolkit_security::access_scope::ScopeConstraint;

fn u(n: u128) -> Uuid {
    Uuid::from_u128(n)
}
fn binding(hash: &str) -> CursorBinding<'_> {
    CursorBinding {
        collection: Collection::Orders,
        parent: None,
        subject_id: u(1),
        subject_tenant_id: u(10),
        filters_hash: hash,
    }
}
fn position() -> Position {
    Position {
        at_micros: 1_791_288_000_123_456,
        id: u(7),
    }
}

#[test]
fn page_size_is_bounded_and_defaults_to_fifty() {
    assert_eq!(PageSize::default().get(), 50);
    assert_eq!(PageSize::try_from(1).unwrap().get(), 1);
    assert_eq!(PageSize::try_from(200).unwrap().get(), 200);
    assert!(PageSize::try_from(0).is_err());
    assert!(PageSize::try_from(201).is_err());
    assert!(PageSize::try_from(u64::MAX).is_err());
    assert_eq!(fetch_limit(PageSize::try_from(200).unwrap()), 201);
    let (rows, more) = split_page(
        (0..201).collect::<Vec<i32>>(),
        PageSize::try_from(200).unwrap(),
    );
    assert_eq!((rows.len(), more), (200, true));
    let (rows, more) = split_page(
        (0..200).collect::<Vec<i32>>(),
        PageSize::try_from(200).unwrap(),
    );
    assert_eq!((rows.len(), more), (200, false));
    let (rows, more) = split_page(Vec::<i32>::new(), PageSize::try_from(1).unwrap());
    assert_eq!((rows.len(), more), (0, false));
}

#[test]
fn cursor_round_trips_microseconds_and_binary_identity() {
    let hash = filters_hash(&OrderFilters::default());
    let token = encode_cursor(binding(&hash), position()).unwrap();
    let decoded = decode_cursor(&token, binding(&hash)).unwrap();
    assert_eq!(decoded, position());
    // The instant keeps its microseconds exactly, including a negative epoch offset.
    let before = Position::of(
        time::OffsetDateTime::from_unix_timestamp_nanos(-1_000_000_001_500).unwrap(),
        u(1),
    )
    .unwrap();
    assert_eq!(before.at_micros, -1_000_000_002);
    let at = Position::of(
        time::OffsetDateTime::from_unix_timestamp_nanos(1_791_288_000_123_456_789).unwrap(),
        u(1),
    )
    .unwrap();
    assert_eq!(at.at_micros, 1_791_288_000_123_456);
    assert_eq!(at.instant().unwrap().microsecond(), 123_456);
}

#[test]
fn a_cursor_is_bound_to_endpoint_parent_principal_filters_and_sort() {
    let hash = filters_hash(&OrderFilters::default());
    let token = encode_cursor(binding(&hash), position()).unwrap();
    let other_hash = filters_hash(&OrderFilters {
        state: Some(OrderState::Draft),
        ..OrderFilters::default()
    });
    assert_ne!(hash, other_hash);
    let mismatches = [
        CursorBinding {
            collection: Collection::Lines,
            ..binding(&hash)
        },
        CursorBinding {
            parent: Some(u(9)),
            ..binding(&hash)
        },
        CursorBinding {
            subject_id: u(2),
            ..binding(&hash)
        },
        CursorBinding {
            subject_tenant_id: u(11),
            ..binding(&hash)
        },
        binding(&other_hash),
    ];
    for wrong in mismatches {
        assert_eq!(
            decode_cursor(&token, wrong).unwrap_err(),
            ReadRuleError::CursorInvalid
        );
    }
    // A lines token binds its parent and cannot be replayed against another order.
    let lines = CursorBinding {
        collection: Collection::Lines,
        parent: Some(u(9)),
        ..binding(&hash)
    };
    let token = encode_cursor(lines, position()).unwrap();
    assert!(decode_cursor(&token, lines).is_ok());
    assert!(
        decode_cursor(
            &token,
            CursorBinding {
                parent: Some(u(10)),
                ..lines
            }
        )
        .is_err()
    );
}

#[test]
fn malformed_tokens_are_cursor_invalid() {
    let hash = filters_hash(&OrderFilters::default());
    let bad = |text: &str| {
        decode_cursor(&Cursor::try_from(text.to_owned()).unwrap(), binding(&hash)).unwrap_err()
    };
    assert_eq!(bad("not-base64!"), ReadRuleError::CursorInvalid);
    assert_eq!(
        bad(&URL_SAFE_NO_PAD.encode(b"{}")),
        ReadRuleError::CursorInvalid
    );
    assert_eq!(
        bad(&URL_SAFE_NO_PAD.encode(b"[1,2,3]")),
        ReadRuleError::CursorInvalid
    );
    // Wrong version, extra field, missing key, wrong sort.
    let valid = Token {
        v: CURSOR_VERSION,
        c: Collection::Orders,
        p: None,
        s: u(1),
        t: u(10),
        f: hash.clone(),
        o: Collection::Orders.sort().to_owned(),
        k: position(),
    };
    let mut json = serde_json::to_value(&valid).unwrap();
    let encode = |v: &serde_json::Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(v).unwrap());
    assert!(decode_cursor(&Cursor::try_from(encode(&json)).unwrap(), binding(&hash)).is_ok());
    json["v"] = serde_json::json!(2);
    assert_eq!(bad(&encode(&json)), ReadRuleError::CursorInvalid);
    json["v"] = serde_json::json!(1);
    json["o"] = serde_json::json!("state_entered_at");
    assert_eq!(bad(&encode(&json)), ReadRuleError::CursorInvalid);
    json["o"] = serde_json::json!(Collection::Orders.sort());
    json["extra"] = serde_json::json!(true);
    assert_eq!(bad(&encode(&json)), ReadRuleError::CursorInvalid);
    json.as_object_mut().unwrap().remove("extra");
    json.as_object_mut().unwrap().remove("k");
    assert_eq!(bad(&encode(&json)), ReadRuleError::CursorInvalid);
    // A millisecond-rounded or textual timestamp cannot be smuggled in: the key is integer micros.
    let mut fractional = serde_json::to_value(&valid).unwrap();
    fractional["k"]["at_micros"] = serde_json::json!(1.5);
    assert_eq!(bad(&encode(&fractional)), ReadRuleError::CursorInvalid);
    // Empty and oversized tokens fail at the boundary type.
    assert!(Cursor::try_from(String::new()).is_err());
    assert!(Cursor::try_from("a".repeat(CURSOR_MAX_LEN + 1)).is_err());
    assert!(Cursor::try_from("a b".to_owned()).is_err());
}

#[test]
fn filters_hash_is_canonical_over_typed_values() {
    let t = time::OffsetDateTime::from_unix_timestamp_nanos(1_791_288_000_123_456_000).unwrap();
    let a = OrderFilters {
        state: Some(OrderState::Draft),
        created_from: Some(t),
        created_to: None,
        state_entered_before: None,
        contract_id: Some(u(5)),
    };
    assert_eq!(filters_hash(&a), filters_hash(&a.clone()));
    assert_ne!(filters_hash(&a), filters_hash(&OrderFilters::default()));
    assert_eq!(no_filters_hash(), filters_hash(&OrderFilters::default()));
    // Same instant expressed in another offset is the same normalized filter.
    let shifted = OrderFilters {
        created_from: Some(t.to_offset(time::UtcOffset::from_hms(2, 0, 0).unwrap())),
        ..a
    };
    assert_eq!(filters_hash(&shifted), filters_hash(&a));
    // Nanosecond-distinct instants are distinct filters (microseconds are the stored precision,
    // but the token binds exactly what the caller normalized to).
    let later = OrderFilters {
        created_from: Some(t + time::Duration::microseconds(1)),
        ..a
    };
    assert_ne!(filters_hash(&later), filters_hash(&a));
}

#[test]
fn confinement_requires_every_path_to_name_only_the_subject_resource_tenant() {
    let own = u(10);
    let eq = |p: &str, v: Uuid| ScopeFilter::eq(p, v);
    let path = |filters: Vec<ScopeFilter>| ScopeConstraint::new(filters);
    assert!(!confined_to_subject(&AccessScope::deny_all(), own));
    #[allow(clippy::disallowed_methods)]
    let unconstrained = AccessScope::from_constraints(vec![]);
    assert!(!confined_to_subject(&unconstrained, own));
    assert!(confined_to_subject(
        &AccessScope::single(path(vec![eq(properties::RESOURCE_TENANT_ID, own)])),
        own
    ));
    assert!(confined_to_subject(
        &AccessScope::single(path(vec![ScopeFilter::in_uuids(
            properties::RESOURCE_TENANT_ID,
            vec![own, own]
        )])),
        own
    ));
    // A foreign tenant, an empty IN, a seller/payer-only path, or one broad OR path all log.
    assert!(!confined_to_subject(
        &AccessScope::single(path(vec![eq(properties::RESOURCE_TENANT_ID, u(11))])),
        own
    ));
    assert!(!confined_to_subject(
        &AccessScope::single(path(vec![ScopeFilter::in_uuids(
            properties::RESOURCE_TENANT_ID,
            vec![own, u(11)]
        )])),
        own
    ));
    assert!(!confined_to_subject(
        &AccessScope::single(path(vec![ScopeFilter::in_uuids(
            properties::RESOURCE_TENANT_ID,
            vec![]
        )])),
        own
    ));
    assert!(!confined_to_subject(
        &AccessScope::single(path(vec![eq(properties::SELLER_TENANT_ID, own)])),
        own
    ));
    assert!(!confined_to_subject(
        &AccessScope::from_constraints(vec![
            path(vec![eq(properties::RESOURCE_TENANT_ID, own)]),
            path(vec![eq(properties::SELLER_TENANT_ID, u(20))]),
        ]),
        own
    ));
    // Own tenant AND a finite ID restriction is still confined (the ID only narrows).
    assert!(confined_to_subject(
        &AccessScope::single(path(vec![
            eq(properties::RESOURCE_TENANT_ID, own),
            eq(properties::ID, u(99))
        ])),
        own
    ));
}

#[test]
fn the_logging_decision_table_follows_section_4_4() {
    assert_eq!(point_read_log(false, u(10), u(10)), ServedLog::NotRequired);
    assert_eq!(point_read_log(true, u(10), u(10)), ServedLog::Required);
    assert_eq!(point_read_log(false, u(20), u(10)), ServedLog::Required);
    assert_eq!(collection_log(false, true), ServedLog::NotRequired);
    assert_eq!(collection_log(true, true), ServedLog::Required);
    assert_eq!(collection_log(false, false), ServedLog::Required);
    let detail = |reason, proof| Refusal { reason, proof }.internal_detail();
    assert_eq!(
        detail(Reason::OrderNotFound, Some(ProofDenial::Required)),
        Some("delegation-proof-required")
    );
    assert_eq!(
        detail(Reason::OrderNotFound, Some(ProofDenial::Invalid)),
        Some("delegation-proof-invalid")
    );
    assert_eq!(detail(Reason::OrderNotFound, None), None);
    // Untargeted proof denials disclose their reason and keep no hidden detail.
    assert_eq!(
        detail(Reason::DelegationProofRequired, Some(ProofDenial::Required)),
        None
    );
    assert_eq!(
        Target {
            requested_order_ref: u(3),
            exists: false
        }
        .order_id(),
        None
    );
    assert_eq!(
        Target {
            requested_order_ref: u(3),
            exists: true
        }
        .order_id(),
        Some(u(3))
    );
    assert_eq!(Collection::Orders.operation(), "list");
    assert_eq!(Collection::Lines.operation(), "list_lines");
    assert_eq!(AccessOutcome::Served.token(), "served");
    assert_eq!(AccessOutcome::Refused.token(), "refused");
}
