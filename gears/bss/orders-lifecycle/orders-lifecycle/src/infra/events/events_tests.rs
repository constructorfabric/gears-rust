#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::payload::*;
use super::test_support::*;
use super::*;
use bss_orders_lifecycle_sdk::catalog::OrderState;
use event_broker_sdk::TypedEvent;
use serde_json::{Value, json};

fn rooted<K: EventKind>(summary: OrderSummary, detail: K) -> OrderEvent<K> {
    OrderEvent::rooted(RootTenant(u(1)), summary, detail)
}

/// The real registry admits the whole Orders contract in the startup commit and resolves each
/// final type with the declared topic, subject type and `/subject` routing (DESIGN §4.7).
#[tokio::test]
async fn the_real_registry_commits_and_resolves_the_event_contract() {
    let client = registry_client();
    let topic = client.get_instance(TOPIC).await.unwrap();
    assert_eq!(topic.id.as_ref(), TOPIC);
    let base = client.get_type_schema(EVENT_BASE).await.unwrap();
    assert!(
        base.effective_traits().get("topic").is_none(),
        "the abstract family base declares no publishable traits"
    );
    let subject = client.get_type_schema(ORDER_SUBJECT_TYPE).await.unwrap();
    assert_eq!(subject.type_id.as_ref(), ORDER_SUBJECT_TYPE);
    let listed = client
        .list_type_schemas(
            types_registry_sdk::TypeSchemaQuery::new().with_pattern(format!("{EVENT_BASE}*")),
        )
        .await
        .unwrap();
    let mut finals: Vec<&str> = listed
        .iter()
        .map(|s| s.type_id.as_ref())
        .filter(|id| *id != EVENT_BASE)
        .collect();
    finals.sort_unstable();
    let mut expected = EVENT_TYPE_IDS.to_vec();
    expected.sort_unstable();
    assert_eq!(finals, expected, "exactly the eleven final event types");
    for id in EVENT_TYPE_IDS {
        let schema = client.get_type_schema(id).await.unwrap();
        let traits = schema.effective_traits();
        assert_eq!(traits["topic"], json!(TOPIC), "{id}");
        assert_eq!(traits["partition_key"], json!(PARTITION_KEY), "{id}");
        assert_eq!(
            traits["allowed_subject_types"],
            json!([ORDER_SUBJECT_TYPE]),
            "{id}"
        );
        let parent = schema
            .parent
            .as_ref()
            .expect("derived from the family base");
        assert_eq!(parent.type_id.as_ref(), EVENT_BASE, "{id}");
    }
}

/// The envelope mapping of §4.4: type, root tenant, source, canonical order-UUID subject.
#[test]
fn typed_events_map_the_envelope_exactly() {
    let order = Uuid::parse_str("0b6f0c52-6b0e-4ad8-9f5d-0c2b0d6a1e9f").unwrap();
    let event = rooted(
        summary(order, 2, OrderState::Submitted),
        OrderSubmitted {
            lines: vec![LineRef { line_id: u(5) }],
            acceptance: None,
        },
    );
    assert_eq!(
        OrderEvent::<OrderSubmitted>::TYPE_ID,
        "gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.submitted.v1~"
    );
    assert_eq!(OrderEvent::<OrderSubmitted>::SOURCE, "bss-orders-lifecycle");
    assert_eq!(
        OrderEvent::<OrderSubmitted>::SUBJECT_TYPE,
        "gts.cf.bss.orders.order.v1~"
    );
    assert_eq!(event.subject(), "0b6f0c52-6b0e-4ad8-9f5d-0c2b0d6a1e9f");
    assert_eq!(
        event.tenant_id(),
        Some(u(1)),
        "root, never a business tenant axis"
    );
    let data = serde_json::to_value(&event).unwrap();
    assert_eq!(data["orderId"], json!(event.subject()));
    assert!(
        data.get("tenant_id").is_none() && data.get("tenant").is_none(),
        "envelope tenancy is not business data"
    );
    assert_eq!(data["orderVersion"], json!(2));
    assert_eq!(data["state"], json!("submitted"));
    assert_eq!(data["correlationId"], json!(u(77)));
    assert!(data.get("contractId").is_none() && data.get("externalReference").is_none());
    for (id, name) in EVENT_TYPE_IDS.iter().zip([
        "submitted",
        "approved",
        "rejected",
        "amended",
        "held",
        "resumed",
        "cancelled",
        "expired",
        "completed",
        "fulfillment_failed",
        "acceptance_recorded",
    ]) {
        assert_eq!(*id, format!("{EVENT_BASE}cf.bss.orders.{name}.v1~"));
        assert!(id.starts_with(&EVENT_TYPE_WILDCARD[..EVENT_TYPE_WILDCARD.len() - 1]));
    }
    assert!(RootTenant::resolved(Uuid::nil()).is_none());
}

/// Every event's `data` names exactly the members its schema closes over.
#[test]
fn schemas_close_over_exactly_the_serialized_members() {
    let documents = schema::final_types();
    assert_eq!(documents.len(), 11);
    let common = &schema::abstract_base()["allOf"][1]["properties"]["data"];
    // The ordinary and forced failure maxima both validate against the failure document.
    let document_of = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 9, 10];
    for (index, (name, data)) in document_of.iter().zip(encoded_max_events(u(9)).iter()) {
        let document = &documents[*index];
        let data: Value = serde_json::from_slice(data).unwrap();
        let narrowing = &document["allOf"][1]["properties"]["data"];
        assert_eq!(narrowing["additionalProperties"], json!(false), "{name}");
        let declared: std::collections::BTreeSet<&str> = narrowing["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        for key in data.as_object().unwrap().keys() {
            assert!(
                declared.contains(key.as_str()),
                "{name} serializes undeclared {key}"
            );
        }
        for key in common["properties"].as_object().unwrap().keys() {
            assert!(
                declared.contains(key.as_str()),
                "{name} closes off common {key}"
            );
        }
        assert_eq!(document["x-gts-final"], json!(true));
    }
    // Ordinary or forced (D-182) evidence; the reason coupling is Orders' own rule.
    assert_eq!(
        documents[9]["allOf"][1]["properties"]["data"]["properties"]["compensationEvidence"]
            ["oneOf"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
}

/// §4.4/§6: the largest event of every type at the 200-line cap, with worst-case JSON text,
/// fits the data budget that leaves the envelope reserve inside 64 KiB.
#[test]
fn maximal_events_at_the_line_cap_fit_the_envelope_budget() {
    for (name, data) in encoded_max_events(u(9)) {
        assert!(
            data.len() <= max_data_bytes(),
            "{name}: {} > {}",
            data.len(),
            max_data_bytes()
        );
        admit_data_len(data.len()).unwrap();
    }
    assert!(admit_data_len(max_data_bytes()).is_ok());
    assert!(matches!(
        admit_data_len(max_data_bytes() + 1),
        Err(EventEnqueueError::TooLarge { .. })
    ));
}

/// `MAX_ENVELOPE_BYTES` is the toolkit outbox's own payload limit.
#[test]
fn the_envelope_limit_is_the_toolkit_payload_limit() {
    let record = |n| {
        toolkit_db::outbox::Record::to(QUEUE, 0)
            .payload(vec![b'x'; n], "application/json")
            .build()
    };
    assert!(record(MAX_ENVELOPE_BYTES).is_ok());
    assert!(record(MAX_ENVELOPE_BYTES + 1).is_err());
}

/// Each §4.4 row rule refuses before enqueue.
#[test]
fn event_contract_rules_refuse_malformed_events() {
    let bad = |event: Result<(), EventContractError>| assert!(event.is_err());
    let ok = |event: Result<(), EventContractError>| event.unwrap();
    let s = |state| summary(u(9), 3, state);
    // Summary rules.
    let mut nil = s(OrderState::Submitted);
    nil.payer_tenant_id = Uuid::nil();
    let lines = || OrderSubmitted {
        lines: vec![LineRef { line_id: u(5) }],
        acceptance: None,
    };
    bad(rooted(nil, lines()).validate());
    let mut foreign = s(OrderState::Submitted);
    foreign.category = "gts.cf.bss.other.category.v1~x".into();
    bad(rooted(foreign, lines()).validate());
    let mut long = s(OrderState::Submitted);
    long.external_reference = Some("x".repeat(MAX_EXTERNAL_REFERENCE_CHARS + 1));
    bad(rooted(long, lines()).validate());
    ok(rooted(s(OrderState::Submitted), lines()).validate());
    // Resulting state must be the row's.
    bad(rooted(s(OrderState::Approved), lines()).validate());
    // Lines: none, duplicated, over the cap.
    bad(rooted(
        s(OrderState::Submitted),
        OrderSubmitted {
            lines: vec![],
            acceptance: None,
        },
    )
    .validate());
    bad(rooted(
        s(OrderState::Submitted),
        OrderSubmitted {
            lines: vec![LineRef { line_id: u(5) }, LineRef { line_id: u(5) }],
            acceptance: None,
        },
    )
    .validate());
    bad(rooted(
        s(OrderState::Submitted),
        OrderSubmitted {
            lines: (0..=MAX_LINES as u128)
                .map(|i| LineRef {
                    line_id: u(100 + i),
                })
                .collect(),
            acceptance: None,
        },
    )
    .validate());
    bad(rooted(
        s(OrderState::Submitted),
        OrderSubmitted {
            lines: vec![LineRef { line_id: u(5) }],
            acceptance: Some(SubmitAcceptance {
                accepted_version: 2,
                accepted_at: at(),
            }),
        },
    )
    .validate());
    // Approval and rejection.
    bad(rooted(
        s(OrderState::Approved),
        OrderApproved {
            deciding_authority: "a".into(),
            approved_version: 2,
        },
    )
    .validate());
    bad(rooted(
        s(OrderState::Approved),
        OrderApproved {
            deciding_authority: "\u{e9}".into(),
            approved_version: 3,
        },
    )
    .validate());
    bad(rooted(
        s(OrderState::Rejected),
        OrderRejected {
            deciding_authority: "a".into(),
            denial_reason: "bad\u{7}".into(),
        },
    )
    .validate());
    // Amendment ordering.
    bad(rooted(
        s(OrderState::Submitted),
        OrderAmended {
            supersedes_version: 3,
        },
    )
    .validate());
    ok(rooted(
        s(OrderState::Submitted),
        OrderAmended {
            supersedes_version: 1,
        },
    )
    .validate());
    // Rows 18-20 all end in `submitted`.
    for state in [
        OrderState::PendingApproval,
        OrderState::Approved,
        OrderState::Draft,
    ] {
        bad(rooted(
            s(state),
            OrderAmended {
                supersedes_version: 2,
            },
        )
        .validate());
    }
    // Hold/resume states.
    bad(rooted(
        s(OrderState::OnHold),
        OrderHeld {
            previous_state: OrderState::Completed,
            hold_reason: None,
        },
    )
    .validate());
    ok(rooted(
        s(OrderState::OnHold),
        OrderHeld {
            previous_state: OrderState::InFulfillment,
            hold_reason: None,
        },
    )
    .validate());
    bad(rooted(
        s(OrderState::Approved),
        OrderResumed {
            restored_state: OrderState::Submitted,
        },
    )
    .validate());
    // Row 21 holds only submitted/pending_approval/approved/in_fulfillment, so a draft is
    // never held and never restored (01 §4.3 rows 21-22).
    bad(rooted(
        s(OrderState::OnHold),
        OrderHeld {
            previous_state: OrderState::Draft,
            hold_reason: None,
        },
    )
    .validate());
    bad(rooted(
        s(OrderState::Draft),
        OrderResumed {
            restored_state: OrderState::Draft,
        },
    )
    .validate());
    for state in crate::infra::events::payload::HOLDABLE_STATES {
        ok(rooted(
            s(OrderState::OnHold),
            OrderHeld {
                previous_state: state,
                hold_reason: None,
            },
        )
        .validate());
        ok(rooted(
            s(state),
            OrderResumed {
                restored_state: state,
            },
        )
        .validate());
    }
}

/// The cancellation, expiry, completion, failure and acceptance row rules.
#[test]
fn terminal_and_acceptance_rules_refuse_malformed_events() {
    let bad = |event: Result<(), EventContractError>| assert!(event.is_err());
    let ok = |event: Result<(), EventContractError>| event.unwrap();
    let s = |state| summary(u(9), 3, state);
    // Cancel: reason mandatory; forced evidence never.
    bad(rooted(
        s(OrderState::Cancelled),
        OrderCancelled {
            cancelling_actor: "a".into(),
            cancel_reason: String::new(),
            compensation_evidence: None,
        },
    )
    .validate());
    bad(rooted(
        s(OrderState::Cancelled),
        OrderCancelled {
            cancelling_actor: "a".into(),
            cancel_reason: "r".into(),
            compensation_evidence: Some(forced_evidence(1)),
        },
    )
    .validate());
    // Expiry policy evidence.
    bad(rooted(
        s(OrderState::Expired),
        OrderExpired {
            expired_state: OrderState::InFulfillment,
            ttl: "P1D".into(),
            ttl_policy_id: u(1),
            ttl_policy_revision: 1,
            platform_policy_revision: 1,
        },
    )
    .validate());
    bad(rooted(
        s(OrderState::Expired),
        OrderExpired {
            expired_state: OrderState::Draft,
            ttl: "1 day".into(),
            ttl_policy_id: u(1),
            ttl_policy_revision: 1,
            platform_policy_revision: 1,
        },
    )
    .validate());
    // Completion mapping distinctness.
    bad(rooted(
        s(OrderState::Completed),
        OrderCompleted {
            lines: vec![
                LineSubscription {
                    line_id: u(5),
                    subscription_id: u(7),
                },
                LineSubscription {
                    line_id: u(6),
                    subscription_id: u(7),
                },
            ],
        },
    )
    .validate());
    // Failure: forced reason/evidence only with operator-forced-unreconciled (D-182).
    let failed = |reason, forced: Option<&str>, evidence| OrderFulfillmentFailed {
        failure_reason: reason,
        forced_reason: forced.map(str::to_owned),
        compensation_evidence: evidence,
    };
    ok(rooted(
        s(OrderState::FulfillmentFailed),
        failed(FailureReason::MarketDivergence, None, ordinary_evidence(1)),
    )
    .validate());
    bad(rooted(
        s(OrderState::FulfillmentFailed),
        failed(
            FailureReason::MarketDivergence,
            Some("x"),
            ordinary_evidence(1),
        ),
    )
    .validate());
    bad(rooted(
        s(OrderState::FulfillmentFailed),
        failed(FailureReason::MarketDivergence, None, forced_evidence(1)),
    )
    .validate());
    bad(rooted(
        s(OrderState::FulfillmentFailed),
        failed(
            FailureReason::OperatorForcedUnreconciled,
            None,
            forced_evidence(1),
        ),
    )
    .validate());
    let mut same_operator = forced_evidence(1);
    same_operator
        .operator_attestation
        .as_mut()
        .unwrap()
        .approved_by = u(91);
    bad(rooted(
        s(OrderState::FulfillmentFailed),
        failed(
            FailureReason::OperatorForcedUnreconciled,
            Some("x"),
            same_operator,
        ),
    )
    .validate());
    let mut remains = ordinary_evidence(1);
    remains.no_active_subscription_remains = Assertion::Known(false);
    bad(rooted(
        s(OrderState::FulfillmentFailed),
        failed(FailureReason::LineExecutionFailed, None, remains),
    )
    .validate());
    // Acceptance: post-draft version, live state.
    bad(rooted(
        s(OrderState::Approved),
        OrderAcceptanceRecorded {
            accepted_version: 1,
            accepted_at: at(),
            recording_actor: "a".into(),
            requirement_source: RequirementSource::Seller,
        },
    )
    .validate());
    bad(rooted(
        s(OrderState::Completed),
        OrderAcceptanceRecorded {
            accepted_version: 2,
            accepted_at: at(),
            recording_actor: "a".into(),
            requirement_source: RequirementSource::Seller,
        },
    )
    .validate());
    // Row 25 records the current immutable version only (`accepted_version =
    // expected_version` under the final version check); a predecessor is refused.
    let acceptance = |accepted_version| OrderAcceptanceRecorded {
        accepted_version,
        accepted_at: at(),
        recording_actor: "a".into(),
        requirement_source: RequirementSource::Contract,
    };
    bad(rooted(s(OrderState::Approved), acceptance(2)).validate());
    bad(rooted(s(OrderState::Approved), acceptance(4)).validate());
    ok(rooted(s(OrderState::Approved), acceptance(3)).validate());
}

/// Wire tokens are the registered ones (closed enumerations), and evidence keeps stored names.
#[test]
fn closed_enumerations_and_evidence_use_registered_tokens() {
    for reason in FailureReason::ALL {
        assert_eq!(serde_json::to_value(reason).unwrap(), json!(reason.token()));
    }
    assert_eq!(
        serde_json::to_value(RequirementSource::PlatformDefault).unwrap(),
        json!("platform_default")
    );
    let forced = serde_json::to_value(forced_evidence(1)).unwrap();
    assert_eq!(forced["at_sale_facts_emitted"], json!("unknown"));
    assert_eq!(forced["no_active_subscription_remains"], json!("unknown"));
    assert_eq!(forced["operator_attestation"]["requested_by"], json!(u(91)));
    let ordinary = serde_json::to_value(ordinary_evidence(0)).unwrap();
    assert!(ordinary.get("operator_attestation").is_none());
    assert_eq!(ordinary["no_active_subscription_remains"], json!(true));
}
