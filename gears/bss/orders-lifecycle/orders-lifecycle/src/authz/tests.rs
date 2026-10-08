//! PEP census, request shape and denial mapping (recording PDP; no database).
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::test_pdp::{
    ScriptedPdp, allow, deny, eq, is_in, path, pep, property, service, u, user, with_proof,
};
use super::*;
use crate::gts::permissions::labels;
use bss_orders_lifecycle_sdk::models::{CallMeta, IdempotencyKey, OrderVersion};

const RT: &str = properties::RESOURCE_TENANT_ID;
const ST: &str = properties::SELLER_TENANT_ID;
const PT: &str = properties::PAYER_TENANT_ID;
const ID: &str = properties::ID;

fn arrangement(r: u128, s: u128, p: u128) -> Arrangement {
    Arrangement {
        resource_tenant_id: u(r),
        seller_tenant_id: u(s),
        payer_tenant_id: u(p),
    }
}
fn found(id: u128, a: Arrangement) -> Prefetch {
    Prefetch::Found(AuthorizationFacts::observed(u(id), a))
}
/// A complete current-order path naming the target and all three axes.
fn exact(id: u128, a: Arrangement) -> Constraint {
    path(vec![
        eq(ID, u(id)),
        eq(RT, a.resource_tenant_id),
        eq(ST, a.seller_tenant_id),
        eq(PT, a.payer_tenant_id),
    ])
}
use authz_resolver_sdk::Constraint;

type Case = (
    Option<authz_resolver_sdk::EvaluationResponse>,
    Result<(), AuthzFailure>,
);
type Row = (Option<&'static str>, bool, Reason, Option<ProofDenial>);

fn refused(result: Result<impl std::fmt::Debug, AuthzFailure>) -> (Reason, Option<ProofDenial>) {
    match result {
        Err(AuthzFailure::Refused { reason, proof }) => (reason, proof),
        other => panic!("expected refusal, got {other:?}"),
    }
}

/// The complete operation census against the reviewed 08 §4.3 catalog.
#[test]
fn every_operation_maps_to_its_exact_registered_action() {
    use Action as A;
    let expected: &[(&str, Option<PatchClass>, Action)] = &[
        ("create", None, A::OrderCreate),
        ("list", None, A::OrderRead),
        ("get", None, A::OrderRead),
        ("patch_order", Some(PatchClass::Commercial), A::OrderWrite),
        (
            "patch_order",
            Some(PatchClass::Administrative),
            A::OrderEdit,
        ),
        ("add_line", None, A::OrderWrite),
        ("patch_line", Some(PatchClass::Commercial), A::OrderWrite),
        ("patch_line", Some(PatchClass::Administrative), A::OrderEdit),
        ("remove_line", None, A::OrderWrite),
        ("preview", None, A::OrderPreview),
        ("submit", None, A::OrderSubmit),
        ("amend", None, A::OrderAmend),
        ("list_versions", None, A::OrderRead),
        ("get_version", None, A::OrderRead),
        ("cancel", None, A::OrderCancel),
        ("hold", None, A::OrderHold),
        ("resume", None, A::OrderResume),
        ("force_fail", None, A::OrderForceFailUnreconciled),
        ("record_acceptance", None, A::AcceptanceRecord),
        ("get_acceptance", None, A::OrderRead),
        ("list_lines", None, A::OrderRead),
        ("read_audit", None, A::AuditRead),
        ("reflect_approval", None, A::OrderApprovalReflection),
        ("begin_fulfillment", None, A::OrderBeginFulfillment),
        ("report_spawn_signal", None, A::OrderSpawnSignal),
        ("acknowledge", None, A::OrderFulfillmentAcknowledgement),
        ("workflow_cancel", None, A::OrderWorkflowCancel),
    ];
    assert_eq!(
        OPERATIONS.len(),
        25,
        "the 08 section 4.3 matrix covers 25 endpoints"
    );
    for op in OPERATIONS {
        let rows: Vec<_> = expected.iter().filter(|(id, ..)| *id == op.id).collect();
        assert!(!rows.is_empty(), "{} has no census row", op.id);
        for (_, class, action) in rows {
            assert_eq!(operation_action(op, *class), Ok(*action), "{}", op.id);
        }
    }
    let patch = OPERATIONS.iter().find(|o| o.id == "patch_order").unwrap();
    assert_eq!(
        operation_action(patch, None),
        Err(CensusError::MissingPatchClass)
    );
    let cancel = OPERATIONS.iter().find(|o| o.id == "cancel").unwrap();
    assert_eq!(
        operation_action(cancel, Some(PatchClass::Commercial)),
        Err(CensusError::UnexpectedPatchClass)
    );
    let invented = Operation {
        id: "invented",
        method: "POST",
        path: "/bss-orders-lifecycle/v1/orders/{orderId}/invented",
        resource: "order",
        action: "invent",
    };
    assert_eq!(
        operation_action(&invented, None),
        Err(CensusError::Undeclared)
    );
    validate_census().unwrap();
    assert!(!is_targeted(
        OPERATIONS.iter().find(|o| o.id == "list").unwrap()
    ));
    assert!(!is_targeted(
        OPERATIONS.iter().find(|o| o.id == "create").unwrap()
    ));
    assert!(!is_targeted(
        OPERATIONS.iter().find(|o| o.id == "preview").unwrap()
    ));
    assert!(is_targeted(
        OPERATIONS.iter().find(|o| o.id == "get").unwrap()
    ));
}

#[test]
fn catalog_ids_labels_and_descriptors_agree_for_every_action() {
    for action in Action::ALL {
        let permission = action.permission().unwrap();
        let (resource, logical) = action.logical();
        let suffix = format!(
            "~cf.bss.orders.{}_{}.v1",
            resource.replace('-', "_"),
            logical.replace('-', "_")
        );
        assert!(permission.id.ends_with(&suffix), "{}", permission.id);
        assert_eq!(permission.action, logical.replace('-', "_"));
        assert_eq!(action.resource_type().unwrap().name(), permission.resource);
    }
    assert_eq!(permissions::ORDER.name(), labels::ORDER);
    // The aggregate never advertises an owner-tenant substitute.
    for rt in [
        &permissions::ORDER,
        &permissions::ACCEPTANCE,
        &permissions::AUDIT,
    ] {
        assert_eq!(rt.supported_properties(), &[RT, ST, PT, ID]);
    }
    assert_eq!(
        permissions::AUDIT_UNRESOLVED.supported_properties(),
        &[properties::SUBJECT_TENANT_ID]
    );
}

/// Every operation asks its exact resource/action with the trusted caller, target, axes and
/// the forwarded proof reference, always requiring constraints.
#[tokio::test]
async fn every_operation_sends_the_exact_pdp_question() {
    let a = arrangement(10, 20, 30);
    let pdp = ScriptedPdp::new(move |_| Some(allow(vec![exact(1, a)])));
    let pep = pep(pdp.clone());
    for op in OPERATIONS {
        let classes: &[Option<PatchClass>] = if op.action.starts_with("field_class") {
            &[
                Some(PatchClass::Commercial),
                Some(PatchClass::Administrative),
            ]
        } else {
            &[None]
        };
        for class in classes {
            let action = operation_action(op, *class).unwrap();
            let caller = with_proof(&user(100, 10), "proof-1");
            pdp.requests.lock().clear();
            if is_targeted(op) {
                let granted = pep
                    .authorize_target(&caller, action, &found(1, a))
                    .await
                    .unwrap();
                assert_eq!(granted.action(), action);
            } else if action == Action::OrderRead {
                pep.authorize_collection(&caller, action).await.unwrap();
            } else {
                pep.authorize_new_arrangement(&caller, action, a)
                    .await
                    .unwrap();
            }
            let requests = pdp.requests.lock();
            assert_eq!(requests.len(), 1, "{} made one decision", op.id);
            let request = &requests[0];
            let permission = action.permission().unwrap();
            assert_eq!(request.resource.resource_type, permission.resource);
            assert_eq!(request.action.name, permission.action);
            assert_eq!(request.subject.id, u(100));
            assert_eq!(
                request.subject.properties["tenant_id"],
                serde_json::json!(u(10).to_string())
            );
            assert!(request.context.require_constraints);
            assert_eq!(property(request, DELEGATION_PROOF_REF), Some("proof-1"));
            if is_targeted(op) {
                assert_eq!(request.resource.id, Some(u(1)));
                assert_eq!(property(request, ID), Some(u(1).to_string().as_str()));
            } else {
                assert_eq!(request.resource.id, None);
            }
            let has_axes = is_targeted(op) || action != Action::OrderRead;
            for (axis, value) in [(RT, 10), (ST, 20), (PT, 30)] {
                let expected = has_axes.then(|| u(value).to_string());
                assert_eq!(
                    property(request, axis).map(str::to_owned),
                    expected,
                    "{} {axis}",
                    op.id
                );
            }
        }
    }
}

#[tokio::test]
async fn no_proof_means_no_proof_property() {
    let pdp = ScriptedPdp::new(|_| Some(allow(vec![path(vec![eq(RT, u(10))])])));
    pep(pdp.clone())
        .authorize_collection(&user(1, 10), Action::OrderRead)
        .await
        .unwrap();
    assert_eq!(
        property(&pdp.requests.lock()[0], DELEGATION_PROOF_REF),
        None
    );
}

#[tokio::test]
async fn untargeted_denials_disclose_proof_reasons_and_outages_are_sanitized() {
    let a = arrangement(10, 20, 30);
    let cases: Vec<Case> = vec![
        (
            Some(deny(Some(DENY_DELEGATION_PROOF_REQUIRED))),
            Err(AuthzFailure::Refused {
                reason: Reason::DelegationProofRequired,
                proof: Some(ProofDenial::Required),
            }),
        ),
        (
            Some(deny(Some(DENY_DELEGATION_PROOF_INVALID))),
            Err(AuthzFailure::Refused {
                reason: Reason::DelegationProofInvalid,
                proof: Some(ProofDenial::Invalid),
            }),
        ),
        (
            Some(deny(Some("no_matching_rule"))),
            Err(AuthzFailure::Refused {
                reason: Reason::OperationNotPermittedForActor,
                proof: None,
            }),
        ),
        (
            Some(deny(None)),
            Err(AuthzFailure::Refused {
                reason: Reason::OperationNotPermittedForActor,
                proof: None,
            }),
        ),
        (None, Err(AuthzFailure::Unavailable)),
        // Missing constraints on an allow are an integration failure, not permission.
        (Some(allow(vec![])), Err(AuthzFailure::Integration)),
        // An owner-tenant substitute is not a supported Orders property.
        (
            Some(allow(vec![path(vec![eq("owner_tenant_id", u(10))])])),
            Err(AuthzFailure::Integration),
        ),
    ];
    for (response, expected) in cases {
        for action in [Action::OrderCreate, Action::OrderPreview] {
            let response = response.clone();
            let pdp = ScriptedPdp::new(move |_| response.clone());
            let got = pep(pdp.clone())
                .authorize_new_arrangement(&user(1, 10), action, a)
                .await
                .map(|_| ());
            assert_eq!(got, expected);
            assert_eq!(
                pdp.calls().len(),
                1,
                "untargeted requests make no follow-up"
            );
        }
        let response = response.clone();
        let pdp = ScriptedPdp::new(move |_| response.clone());
        let got = pep(pdp)
            .authorize_collection(&user(1, 10), Action::OrderRead)
            .await
            .map(|_| ());
        assert_eq!(got, expected);
    }
    // Only the list read is an untargeted collection decision; nothing is asked otherwise.
    for action in [
        Action::OrderWrite,
        Action::AuditRead,
        Action::AuditUnresolvedRead,
    ] {
        let pdp = ScriptedPdp::new(|_| Some(allow(vec![path(vec![eq(RT, u(10))])])));
        assert_eq!(
            pep(pdp.clone())
                .authorize_collection(&user(1, 10), action)
                .await
                .map(|_| ()),
            Err(AuthzFailure::Integration)
        );
        assert!(pdp.calls().is_empty());
    }
    // Only create/preview are untargeted arrangement decisions.
    let pdp = ScriptedPdp::new(|_| Some(allow(vec![path(vec![eq(RT, u(10))])])));
    assert_eq!(
        pep(pdp)
            .authorize_new_arrangement(&user(1, 10), Action::OrderWrite, a)
            .await
            .map(|_| ()),
        Err(AuthzFailure::Integration)
    );
}

/// D-114/D-141/D-68: hidden and missing targets make the same calls and answer identically.
#[tokio::test]
async fn targeted_denials_follow_up_once_and_never_disclose_existence() {
    let a = arrangement(10, 20, 30);
    // (first decision, follow-up read decision) -> expected reason
    let matrix: Vec<Row> = vec![
        (None, true, Reason::OperationNotPermittedForActor, None),
        (None, false, Reason::OrderNotFound, None),
        (
            Some(DENY_DELEGATION_PROOF_REQUIRED),
            true,
            Reason::OrderNotFound,
            Some(ProofDenial::Required),
        ),
        (
            Some(DENY_DELEGATION_PROOF_INVALID),
            false,
            Reason::OrderNotFound,
            Some(ProofDenial::Invalid),
        ),
    ];
    for (code, readable, reason, proof) in matrix {
        let pdp = ScriptedPdp::new(move |r| {
            if r.action.name == "read" {
                Some(if readable {
                    allow(vec![exact(1, a)])
                } else {
                    deny(None)
                })
            } else {
                Some(deny(code))
            }
        });
        let pep = pep(pdp.clone());
        let hidden = pep
            .authorize_target(&user(1, 99), Action::OrderCancel, &found(1, a))
            .await;
        assert_eq!(refused(hidden), (reason, proof));
        let hidden_calls = pdp.calls();
        pdp.requests.lock().clear();
        let missing = pep
            .authorize_target(&user(1, 99), Action::OrderCancel, &Prefetch::Missing(u(1)))
            .await;
        // The missing arm never discloses 403 and carries no axes.
        assert_eq!(refused(missing).0, Reason::OrderNotFound);
        assert_eq!(pdp.calls(), hidden_calls, "same PDP calls on both arms");
        assert_eq!(
            hidden_calls,
            vec![
                ("cancel".to_owned(), Some(u(1))),
                ("read".to_owned(), Some(u(1)))
            ]
        );
        for request in pdp.requests.lock().iter() {
            assert_eq!(property(request, RT), None);
        }
    }
    // A missing target answers 404 even when the PDP would allow the empty-property request.
    let pdp = ScriptedPdp::new(|_| Some(allow(vec![path(vec![eq(ID, u(1))])])));
    let missing = pep(pdp.clone())
        .authorize_target(&user(1, 10), Action::OrderHold, &Prefetch::Missing(u(1)))
        .await;
    assert_eq!(refused(missing).0, Reason::OrderNotFound);
    assert_eq!(pdp.calls().len(), 2);
}

#[tokio::test]
async fn denied_read_makes_no_follow_up_on_either_arm() {
    let a = arrangement(10, 20, 30);
    for prefetch in [found(1, a), Prefetch::Missing(u(1))] {
        let pdp = ScriptedPdp::new(|_| Some(deny(Some(DENY_DELEGATION_PROOF_INVALID))));
        let got = pep(pdp.clone())
            .authorize_target(&user(1, 10), Action::OrderRead, &prefetch)
            .await;
        assert_eq!(refused(got).0, Reason::OrderNotFound);
        assert_eq!(pdp.calls().len(), 1);
    }
}

#[tokio::test]
async fn outage_on_first_or_follow_up_call_is_unavailable_on_both_arms() {
    let a = arrangement(10, 20, 30);
    for prefetch in [found(1, a), Prefetch::Missing(u(1))] {
        let first = ScriptedPdp::new(|_| None);
        assert!(matches!(
            pep(first)
                .authorize_target(&user(1, 10), Action::OrderSubmit, &prefetch)
                .await,
            Err(AuthzFailure::Unavailable)
        ));
        let follow = ScriptedPdp::new(|r| (r.action.name != "read").then(|| deny(None)));
        assert!(matches!(
            pep(follow)
                .authorize_target(&user(1, 10), Action::OrderSubmit, &prefetch)
                .await,
            Err(AuthzFailure::Unavailable)
        ));
        // A point read of a nonexistent order during an outage is 503, never 404.
        let read = ScriptedPdp::new(|_| None);
        assert!(matches!(
            pep(read)
                .authorize_target(&user(1, 10), Action::OrderRead, &prefetch)
                .await,
            Err(AuthzFailure::Unavailable)
        ));
    }
}

/// Workflow/event-consumer decisions need finite order IDs in every OR path (08 §4.3).
#[tokio::test]
async fn service_and_workflow_decisions_require_finite_ids_in_every_path() {
    let a = arrangement(10, 20, 30);
    let bounded = path(vec![is_in(ID, &[u(1), u(2)]), eq(ST, u(20))]);
    let unbounded = path(vec![eq(ST, u(20))]);
    let cases = vec![
        (vec![bounded.clone()], true),
        (vec![bounded.clone(), unbounded.clone()], false),
        (vec![unbounded.clone()], false),
    ];
    for (constraints, ok) in cases {
        let response = allow(constraints);
        for (caller, action) in [
            (service(7, 70), Action::OrderRead),
            (service(7, 70), Action::OrderHold),
            (user(1, 10), Action::OrderBeginFulfillment),
            (user(1, 10), Action::OrderSpawnSignal),
        ] {
            let r = response.clone();
            let pdp = ScriptedPdp::new(move |_| Some(r.clone()));
            let got = pep(pdp)
                .authorize_target(&caller, action, &found(1, a))
                .await;
            assert_eq!(got.is_ok(), ok, "{action:?}");
            if !ok {
                assert!(matches!(got, Err(AuthzFailure::Integration)));
            }
        }
        // Service collection reads obey the same shape requirement.
        let r = response.clone();
        let pdp = ScriptedPdp::new(move |_| Some(r.clone()));
        assert_eq!(
            pep(pdp)
                .authorize_collection(&service(7, 70), Action::OrderRead)
                .await
                .is_ok(),
            ok
        );
    }
    // A human tenant-scoped read path is valid without an ID restriction.
    let pdp = ScriptedPdp::new(move |_| Some(allow(vec![path(vec![eq(ST, u(20))])])));
    assert!(
        pep(pdp)
            .authorize_target(&user(1, 10), Action::OrderRead, &found(1, a))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn break_glass_is_never_honored_for_a_service_principal() {
    let a = arrangement(10, 20, 30);
    let pdp = ScriptedPdp::new(move |_| Some(allow(vec![exact(1, a)])));
    let got = pep(pdp.clone())
        .authorize_target(
            &service(7, 70),
            Action::OrderForceFailUnreconciled,
            &found(1, a),
        )
        .await;
    assert_eq!(refused(got).0, Reason::OperationNotPermittedForActor);
    assert_eq!(pdp.calls().len(), 2, "same follow-up pattern as any denial");
    let pdp = ScriptedPdp::new(move |_| Some(allow(vec![exact(1, a)])));
    assert!(
        pep(pdp)
            .authorize_target(
                &user(1, 20),
                Action::OrderForceFailUnreconciled,
                &found(1, a)
            )
            .await
            .is_ok()
    );
}

/// Check-both: the proposed side is a separate decision on the complete arrangement.
#[tokio::test]
async fn proposed_arrangement_is_a_separate_complete_decision() {
    let current = arrangement(10, 20, 30);
    let proposed = current.with_delta(None, Some(u(31)));
    assert_eq!(proposed, arrangement(10, 20, 31));
    assert_eq!(
        current.with_delta(Some(u(11)), None),
        arrangement(11, 20, 30)
    );
    // Old allowed, new payer denied; the caller can still read the order -> 403.
    let pdp = ScriptedPdp::new(move |r| {
        let payer = property(r, PT).map(str::to_owned);
        if payer == Some(u(31).to_string()) && r.action.name == "write" {
            Some(deny(None))
        } else {
            Some(allow(vec![exact(1, current)]))
        }
    });
    let pep_ = pep(pdp.clone());
    let target = pep_
        .authorize_target(&user(1, 10), Action::OrderWrite, &found(1, current))
        .await
        .unwrap();
    let got = pep_
        .authorize_proposed(&user(1, 10), &target, proposed)
        .await;
    assert_eq!(refused(got).0, Reason::OperationNotPermittedForActor);
    {
        let calls = pdp.requests.lock();
        assert_eq!(calls.len(), 3);
        assert_eq!(property(&calls[1], PT), Some(u(31).to_string().as_str()));
        // The follow-up read is asked about the current facts, not the proposal.
        assert_eq!(calls[2].action.name, "read");
        assert_eq!(property(&calls[2], PT), Some(u(30).to_string().as_str()));
    }
    // Both allowed: the proposal carries the authorized arrangement.
    let pdp = ScriptedPdp::new(move |_| {
        Some(allow(vec![
            exact(1, current),
            exact(1, arrangement(10, 20, 31)),
        ]))
    });
    let pep_ = pep(pdp);
    let target = pep_
        .authorize_target(&user(1, 10), Action::OrderWrite, &found(1, current))
        .await
        .unwrap();
    let granted = pep_
        .authorize_proposed(&user(1, 10), &target, proposed)
        .await
        .unwrap();
    assert_eq!(granted.arrangement(), proposed);
    assert_eq!(granted.order_id(), Some(u(1)));
}

#[tokio::test]
async fn unresolved_audit_branch_is_optional_but_outage_fails_the_response() {
    let pdp = ScriptedPdp::new(|_| Some(deny(None)));
    assert!(
        pep(pdp)
            .authorize_unresolved_audit(&user(1, 10))
            .await
            .unwrap()
            .is_none()
    );
    let pdp = ScriptedPdp::new(|_| {
        Some(allow(vec![path(vec![eq(
            properties::SUBJECT_TENANT_ID,
            u(10),
        )])]))
    });
    let granted = pep(pdp.clone())
        .authorize_unresolved_audit(&user(1, 10))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(granted.action(), Action::AuditUnresolvedRead);
    assert_eq!(
        pdp.requests.lock()[0].resource.resource_type,
        labels::AUDIT_UNRESOLVED
    );
    let pdp = ScriptedPdp::new(|_| None);
    assert!(matches!(
        pep(pdp).authorize_unresolved_audit(&user(1, 10)).await,
        Err(AuthzFailure::Unavailable)
    ));
}

#[tokio::test]
async fn replay_rechecks_current_disclosure_authority() {
    let a = arrangement(10, 20, 30);
    // A create's stored result is disclosed only after a fresh `order × read` on the order.
    let pdp = ScriptedPdp::new(|_| Some(deny(None)));
    let got = pep(pdp.clone())
        .recheck_replay_disclosure(&user(1, 10), Action::OrderCreate, &found(1, a))
        .await;
    assert_eq!(refused(got).0, Reason::OrderNotFound);
    assert_eq!(pdp.calls(), vec![("read".to_owned(), Some(u(1)))]);
    // Other operations recheck their own action on current facts.
    let pdp = ScriptedPdp::new(move |_| Some(allow(vec![exact(1, a)])));
    let granted = pep(pdp.clone())
        .recheck_replay_disclosure(&user(1, 10), Action::OrderCancel, &found(1, a))
        .await
        .unwrap();
    assert_eq!(granted.action(), Action::OrderCancel);
    assert_eq!(pdp.calls()[0].0, "cancel");
}

#[test]
fn authz_failures_map_to_sanitized_problems() {
    let status = |f: AuthzFailure| crate::api::rest::problem(f.into()).status;
    assert_eq!(
        status(AuthzFailure::Refused {
            reason: Reason::OrderNotFound,
            proof: Some(ProofDenial::Invalid)
        }),
        Some(404)
    );
    assert_eq!(
        status(AuthzFailure::Refused {
            reason: Reason::OperationNotPermittedForActor,
            proof: None
        }),
        Some(403)
    );
    assert_eq!(status(AuthzFailure::Unavailable), Some(503));
    assert_eq!(status(AuthzFailure::Integration), Some(500));
    let body = serde_json::to_string(&crate::api::rest::problem(
        AuthzFailure::Refused {
            reason: Reason::OrderNotFound,
            proof: Some(ProofDenial::Invalid),
        }
        .into(),
    ))
    .unwrap();
    assert!(!body.contains("proof"), "{body}");
}

/// REST header and SDK metadata reach the PEP identically (coordinator decision 2026-10-06).
#[tokio::test]
async fn rest_header_and_sdk_metadata_produce_identical_pdp_requests() {
    use crate::api::rest::{DELEGATION_PROOF_HEADER, caller, delegation_proof, sdk_caller};
    use axum::http::{HeaderMap, HeaderValue};
    let mut headers = HeaderMap::new();
    assert_eq!(delegation_proof(&headers).unwrap(), None);
    headers.insert(DELEGATION_PROOF_HEADER, HeaderValue::from_static("proof-9"));
    let ctx = super::test_pdp::ctx(1, 10, super::test_pdp::USER);
    let rest = caller(ctx.clone(), &headers).unwrap();
    let meta = CallMeta {
        expected_version: OrderVersion::try_from(1).unwrap(),
        idempotency_key: IdempotencyKey::try_from("k".to_owned()).unwrap(),
        correlation_id: None,
        delegation_proof_ref: Some("proof-9".to_owned().try_into().unwrap()),
    };
    let sdk = sdk_caller(&ctx, &meta);
    let pdp = ScriptedPdp::new(|_| Some(allow(vec![path(vec![eq(RT, u(10))])])));
    let pep_ = pep(pdp.clone());
    pep_.authorize_collection(&rest, Action::OrderRead)
        .await
        .unwrap();
    pep_.authorize_collection(&sdk, Action::OrderRead)
        .await
        .unwrap();
    let requests = pdp.requests.lock();
    assert_eq!(
        serde_json::to_value(&requests[0].resource).unwrap(),
        serde_json::to_value(&requests[1].resource).unwrap()
    );
    assert_eq!(requests[0].subject.id, requests[1].subject.id);
    assert_eq!(
        property(&requests[0], DELEGATION_PROOF_REF),
        Some("proof-9")
    );
    // Debug output never reveals the reference.
    assert!(!format!("{rest:?}").contains("proof-9"));
}

#[test]
fn malformed_oversized_or_duplicate_proof_headers_are_request_invalid() {
    use crate::api::rest::{DELEGATION_PROOF_HEADER, delegation_proof};
    use axum::http::{HeaderMap, HeaderValue};
    let invalid = |headers: &HeaderMap| {
        matches!(
            delegation_proof(headers),
            Err(OrdersError::Refused(Reason::RequestInvalid))
        )
    };
    let mut ok = HeaderMap::new();
    ok.insert(
        DELEGATION_PROOF_HEADER,
        HeaderValue::from_str(&"a".repeat(512)).unwrap(),
    );
    assert!(delegation_proof(&ok).unwrap().is_some());
    // D-202: a reference names a proof and never carries one.
    for bad in [
        "",
        "has space",
        &"a".repeat(513),
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2ln",
        "https://user:pw@proofs.example/1",
        "ref?token=abc",
    ] {
        let mut headers = HeaderMap::new();
        headers.insert(DELEGATION_PROOF_HEADER, HeaderValue::from_str(bad).unwrap());
        assert!(invalid(&headers), "{bad:?}");
    }
    let mut non_ascii = HeaderMap::new();
    non_ascii.insert(
        DELEGATION_PROOF_HEADER,
        HeaderValue::from_bytes(b"caf\xc3\xa9").unwrap(),
    );
    assert!(invalid(&non_ascii));
    let mut duplicate = HeaderMap::new();
    duplicate.append(DELEGATION_PROOF_HEADER, HeaderValue::from_static("a"));
    duplicate.append(DELEGATION_PROOF_HEADER, HeaderValue::from_static("b"));
    assert!(invalid(&duplicate));
    // The OpenAPI declaration names the same optional header.
    let param = crate::api::rest::delegation_proof_param();
    assert_eq!(param.name, DELEGATION_PROOF_HEADER);
    assert!(!param.required);
}

#[test]
fn caller_class_comes_only_from_the_authenticated_subject_type() {
    assert_eq!(user(1, 10).actor_class(), ActorClass::User);
    assert_eq!(service(1, 10).actor_class(), ActorClass::Service);
    let untyped = SecurityContext::builder()
        .subject_id(u(1))
        .subject_tenant_id(u(10))
        .build()
        .unwrap();
    assert_eq!(
        Caller::new(untyped, None).actor_class(),
        ActorClass::Service
    );
}

/// The bundled static development provider cannot express Orders' three-axis policy: its
/// owner-tenant-only answer yields no usable constraints, so the PEP fails closed.
#[tokio::test]
async fn bundled_static_provider_fails_closed_for_every_orders_resource() {
    struct BundledStatic;
    #[async_trait::async_trait]
    impl authz_resolver_sdk::AuthZResolverApi for BundledStatic {
        async fn evaluate(
            &self,
            _: toolkit_security::PlatformSecurityContext,
            request: authz_resolver_sdk::EvaluationRequest,
        ) -> Result<authz_resolver_sdk::EvaluationResponse, toolkit_canonical_errors::CanonicalError>
        {
            Ok(static_authz_plugin::domain::Service::new().evaluate(&request))
        }
    }
    let pep = pep(std::sync::Arc::new(BundledStatic));
    let a = arrangement(10, 20, 30);
    assert_eq!(
        pep.authorize_collection(&user(1, 10), Action::OrderRead)
            .await
            .map(|_| ()),
        Err(AuthzFailure::Integration)
    );
    assert_eq!(
        pep.authorize_new_arrangement(&user(1, 10), Action::OrderCreate, a)
            .await
            .map(|_| ()),
        Err(AuthzFailure::Integration)
    );
    for action in [
        Action::OrderWrite,
        Action::AcceptanceRecord,
        Action::AuditRead,
    ] {
        assert!(matches!(
            pep.authorize_target(&user(1, 10), action, &found(1, a))
                .await,
            Err(AuthzFailure::Integration)
        ));
    }
}

/// Gap review (S2-03): a constraint-only PDP answer may say "allowed" with constraints that do
/// not admit the prefetched target. Orders enforces those constraints on the known facts, so such
/// an answer is a denial for that target: same follow-up, same 403/404 mapping, and a read scope
/// that excludes the target never confirms its existence.
#[tokio::test]
async fn constraint_only_allows_that_exclude_the_target_are_denials() {
    let a = arrangement(10, 20, 30);
    let elsewhere = || allow(vec![path(vec![eq(RT, u(99))])]);
    for (read_admits, reason) in [
        (false, Reason::OrderNotFound),
        (true, Reason::OperationNotPermittedForActor),
    ] {
        let pdp = ScriptedPdp::new(move |r| {
            Some(if r.action.name == "read" && read_admits {
                allow(vec![path(vec![eq(RT, u(10))])])
            } else {
                elsewhere()
            })
        });
        let pep_ = pep(pdp.clone());
        let hidden = pep_
            .authorize_target(&user(1, 10), Action::OrderCancel, &found(1, a))
            .await;
        assert_eq!(refused(hidden), (reason, None));
        let hidden_calls = pdp.calls();
        assert_eq!(
            hidden_calls,
            vec![
                ("cancel".to_owned(), Some(u(1))),
                ("read".to_owned(), Some(u(1)))
            ]
        );
        pdp.requests.lock().clear();
        let missing = pep_
            .authorize_target(&user(1, 10), Action::OrderCancel, &Prefetch::Missing(u(1)))
            .await;
        assert_eq!(refused(missing).0, Reason::OrderNotFound);
        assert_eq!(pdp.calls(), hidden_calls, "same PDP calls on both arms");
    }
    // A read whose constraints exclude the target is the non-disclosing not-found, no follow-up.
    let pdp = ScriptedPdp::new(move |_| Some(elsewhere()));
    let read = pep(pdp.clone())
        .authorize_target(&user(1, 10), Action::OrderRead, &found(1, a))
        .await;
    assert_eq!(refused(read).0, Reason::OrderNotFound);
    assert_eq!(pdp.calls().len(), 1);
    // A finite ID set naming another order excludes this target too.
    let pdp = ScriptedPdp::new(|_| Some(allow(vec![path(vec![is_in(ID, &[u(2)])])])));
    let other = pep(pdp)
        .authorize_target(&service(7, 70), Action::OrderBeginFulfillment, &found(1, a))
        .await;
    assert_eq!(refused(other).0, Reason::OrderNotFound);
}

/// Create/Preview: constraints that exclude the proposed arrangement (for example a payer the
/// caller may not use) authorize other arrangements only -> untargeted 403, one call.
#[tokio::test]
async fn new_arrangement_must_be_admitted_by_the_returned_constraints() {
    let pdp = ScriptedPdp::new(|_| Some(allow(vec![path(vec![eq(RT, u(10)), eq(PT, u(30))])])));
    let pep_ = pep(pdp.clone());
    for action in [Action::OrderCreate, Action::OrderPreview] {
        assert_eq!(
            refused(
                pep_.authorize_new_arrangement(&user(1, 10), action, arrangement(10, 20, 31))
                    .await
            ),
            (Reason::OperationNotPermittedForActor, None)
        );
        assert!(
            pep_.authorize_new_arrangement(&user(1, 10), action, arrangement(10, 20, 30))
                .await
                .is_ok()
        );
    }
    assert_eq!(pdp.calls().len(), 4, "no follow-up on untargeted requests");
}

/// Check-both: a proposed-side allow whose constraints exclude the proposal is a denial with the
/// follow-up read on current facts; a seller change is never a proposal (D-119).
#[tokio::test]
async fn proposed_side_constraints_must_admit_the_proposal_and_seller_is_fixed() {
    let current = arrangement(10, 20, 30);
    let pdp = ScriptedPdp::new(move |_| {
        Some(allow(vec![path(vec![
            eq(RT, u(10)),
            eq(ST, u(20)),
            eq(PT, u(30)),
        ])]))
    });
    let pep_ = pep(pdp.clone());
    let target = pep_
        .authorize_target(&user(1, 10), Action::OrderWrite, &found(1, current))
        .await
        .unwrap();
    let got = pep_
        .authorize_proposed(&user(1, 10), &target, current.with_delta(None, Some(u(31))))
        .await;
    assert_eq!(refused(got), (Reason::OperationNotPermittedForActor, None));
    assert_eq!(
        pdp.calls()
            .into_iter()
            .map(|(action, _)| action)
            .collect::<Vec<_>>(),
        vec!["write", "write", "read"]
    );
    pdp.requests.lock().clear();
    let seller_changed = Arrangement {
        seller_tenant_id: u(21),
        ..current
    };
    assert_eq!(
        pep_.authorize_proposed(&user(1, 10), &target, seller_changed)
            .await
            .map(|_| ()),
        Err(AuthzFailure::Integration)
    );
    assert!(pdp.calls().is_empty(), "no proposed-side decision is asked");
}
