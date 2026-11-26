use serde_json::json;

use super::*;

const TENANT: &str = "gts.cf.core.quota.subj.v1~cf.core.quota.tenant.v1~";
const USER_ID: &str = "00000000-0000-0000-0000-00000000002a";

fn subject(value: serde_json::Value) -> Result<SubjectInput, serde_json::Error> {
    serde_json::from_value(value)
}

#[test]
fn both_subject_forms_round_trip() {
    let quota = subject(json!({ "kind": "quota", "type": TENANT, "id": "t1" })).unwrap();
    assert_eq!(
        quota,
        SubjectInput::Quota(QuotaSubject {
            subject_type: TENANT.to_owned(),
            id: "t1".to_owned(),
        })
    );
    assert_eq!(
        serde_json::to_value(&quota).unwrap(),
        json!({ "kind": "quota", "type": TENANT, "id": "t1" })
    );

    let principal = subject(json!({ "kind": "principal", "type": "user", "id": USER_ID })).unwrap();
    assert_eq!(
        principal,
        SubjectInput::Principal(PrincipalSubject {
            subject_type: "user".to_owned(),
            id: Uuid::from_u128(42),
        })
    );
    assert_eq!(
        serde_json::to_value(&principal).unwrap(),
        json!({ "kind": "principal", "type": "user", "id": USER_ID })
    );
}

#[test]
fn a_malformed_subject_is_rejected() {
    let cases = [
        ("missing kind", json!({ "type": TENANT, "id": "t1" })),
        (
            "unknown kind",
            json!({ "kind": "group", "type": TENANT, "id": "t1" }),
        ),
        (
            "non-string kind",
            json!({ "kind": 1, "type": TENANT, "id": "t1" }),
        ),
        (
            "extra field on a quota subject",
            json!({ "kind": "quota", "type": TENANT, "id": "t1", "name": "x" }),
        ),
        (
            "extra field on a principal",
            json!({ "kind": "principal", "type": "user", "id": USER_ID, "name": "x" }),
        ),
        (
            "quota subject without id",
            json!({ "kind": "quota", "type": TENANT }),
        ),
        (
            "quota subject without type",
            json!({ "kind": "quota", "id": "t1" }),
        ),
        (
            "principal without id",
            json!({ "kind": "principal", "type": "user" }),
        ),
        (
            "principal without type",
            json!({ "kind": "principal", "id": USER_ID }),
        ),
        (
            "quota id of a wrong type",
            json!({ "kind": "quota", "type": TENANT, "id": 7 }),
        ),
        (
            "quota type of a wrong type",
            json!({ "kind": "quota", "type": 7, "id": "t1" }),
        ),
        (
            "principal id that is not a uuid",
            json!({ "kind": "principal", "type": "user", "id": "u1" }),
        ),
        (
            "principal type of a wrong type",
            json!({ "kind": "principal", "type": 7, "id": USER_ID }),
        ),
        (
            "empty quota id",
            json!({ "kind": "quota", "type": TENANT, "id": "" }),
        ),
    ];
    for (name, value) in cases {
        assert!(subject(value).is_err(), "{name} must be rejected");
    }
}

#[test]
fn a_nil_principal_id_is_well_formed() {
    let nil = "00000000-0000-0000-0000-000000000000";
    assert!(subject(json!({ "kind": "principal", "type": "user", "id": nil })).is_ok());
}

#[test]
fn a_quota_subject_outside_a_platform_request_has_no_kind() {
    let plain: QuotaSubject =
        serde_json::from_value(json!({ "type": TENANT, "id": "t1" })).unwrap();
    assert_eq!(
        serde_json::to_value(&plain).unwrap(),
        json!({ "type": TENANT, "id": "t1" })
    );

    assert!(
        serde_json::from_value::<QuotaSubject>(
            json!({ "kind": "quota", "type": TENANT, "id": "t1" })
        )
        .is_err(),
        "`kind` is not a field of a bare quota subject"
    );
    assert!(serde_json::from_value::<QuotaSubject>(json!({ "type": TENANT, "id": "" })).is_err());
}

#[test]
fn requests_and_results_ignore_unknown_fields() {
    let debit = json!({
        "tenant_id": USER_ID,
        "subjects": [{ "kind": "quota", "type": TENANT, "id": "t1" }],
        "items": [{ "metric": { "type": "m", "metadata": {} }, "amount": 5, "later": 1 }],
        "idempotency_key": "k",
        "later": true,
    });
    serde_json::from_value::<PlatformDebitRequest>(debit).unwrap();

    serde_json::from_value::<QuotaStateRequest>(
        json!({ "metrics": [{ "type": "m" }], "later": 1 }),
    )
    .unwrap();
    serde_json::from_value::<PlatformReturnRequest>(json!({
        "tenant_id": USER_ID,
        "charge": { "operation": "lease_commit", "idempotency_key": "k", "later": 1 },
        "amounts": [{ "metric_type": "m", "amount": 1, "later": 1 }],
        "idempotency_key": "r",
        "later": 1,
    }))
    .unwrap();

    let decision: QuotaDecision =
        serde_json::from_value(json!({ "allowed": true, "later": 1 })).unwrap();
    assert!(decision.allowed);
    let outcome: LeaseOutcome = serde_json::from_value(json!({
        "outcome": "denied",
        "decision": { "allowed": false, "reason": "quota_exceeded", "later": 1 },
        "later": 1,
    }))
    .unwrap();
    assert_eq!(
        outcome,
        LeaseOutcome::Denied {
            decision: QuotaDecision::denied(DenialReason::QuotaExceeded)
        }
    );
    serde_json::from_value::<QuotaState>(json!({
        "quota_id": "q",
        "subject": { "type": TENANT, "id": "t1" },
        "metric_type": "m",
        "consumed": 1,
        "later": 1,
    }))
    .unwrap();
}

#[test]
fn a_subject_nested_in_a_result_still_rejects_unknown_fields() {
    let state = json!({
        "quota_id": "q",
        "subject": { "type": TENANT, "id": "t1", "later": 1 },
        "metric_type": "m",
        "consumed": 1,
    });
    assert!(serde_json::from_value::<QuotaState>(state).is_err());
}

#[test]
fn a_metric_object_round_trips_unknown_fields_unchanged() {
    let wire = json!({
        "type": "gts.cf.core.quota.metric.v1~x.y.m.v1~",
        "id": "gpt-4o",
        "metadata": { "region": "eu", "nested": { "a": [1, 2] } },
        "later": { "k": "v" },
    });
    let metric: MetricObject = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(metric.extra.get("later"), Some(&json!({ "k": "v" })));
    assert_eq!(serde_json::to_value(&metric).unwrap(), wire);
}

#[test]
fn a_metric_object_needs_type_and_metadata() {
    assert!(serde_json::from_value::<MetricObject>(json!({ "metadata": {} })).is_err());
    assert!(serde_json::from_value::<MetricObject>(json!({ "type": "m" })).is_err());
    assert!(serde_json::from_value::<MetricObject>(json!({ "type": "m", "metadata": 1 })).is_err());
    let bare: MetricObject =
        serde_json::from_value(json!({ "type": "m", "metadata": {} })).unwrap();
    assert_eq!(bare.id, None);
    assert_eq!(
        serde_json::to_value(&bare).unwrap(),
        json!({ "type": "m", "metadata": {} })
    );
}

#[test]
fn a_denial_reason_is_a_snake_case_token() {
    for (reason, token) in [
        (DenialReason::QuotaExceeded, "quota_exceeded"),
        (DenialReason::NoApplicableQuota, "no_applicable_quota"),
        (DenialReason::PrincipalNotMapped, "principal_not_mapped"),
    ] {
        let decision = QuotaDecision::denied(reason).with_diagnostic("quota", "q1");
        let wire = serde_json::to_value(&decision).unwrap();
        assert_eq!(wire["allowed"], json!(false));
        assert_eq!(wire["reason"], json!(token));
        assert_eq!(wire["diagnostics"], json!({ "quota": "q1" }));
        assert_eq!(
            serde_json::from_value::<QuotaDecision>(wire).unwrap(),
            decision
        );
    }
}

#[test]
fn an_unknown_denial_reason_reads_as_a_denial() {
    let decision =
        serde_json::from_value::<QuotaDecision>(json!({ "allowed": false, "reason": "other" }))
            .unwrap();
    assert!(!decision.allowed);
    assert_eq!(decision.reason, Some(DenialReason::Unknown));

    let outcome = serde_json::from_value::<LeaseOutcome>(json!({
        "outcome": "denied",
        "decision": { "allowed": false, "reason": "other" }
    }))
    .unwrap();
    let LeaseOutcome::Denied { decision } = outcome else {
        panic!("expected a denial, got {outcome:?}");
    };
    assert!(!decision.allowed);
    assert_eq!(decision.reason, Some(DenialReason::Unknown));

    let outcome =
        serde_json::from_value::<LeaseOutcome>(json!({ "outcome": "queued", "eta": 5 })).unwrap();
    assert_eq!(outcome, LeaseOutcome::Unknown);
}

#[test]
fn an_acquired_lease_round_trips() {
    let outcome = LeaseOutcome::Acquired {
        token: "tok".to_owned(),
        expires_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
    };
    let wire = serde_json::to_value(&outcome).unwrap();
    assert_eq!(wire["outcome"], json!("acquired"));
    assert_eq!(
        serde_json::from_value::<LeaseOutcome>(wire).unwrap(),
        outcome
    );
}

#[test]
fn a_commit_without_amounts_commits_the_full_held_amounts() {
    let commit: PlatformCommitLeaseRequest = serde_json::from_value(json!({
        "tenant_id": USER_ID,
        "token": "tok",
        "idempotency_key": "k",
    }))
    .unwrap();
    assert_eq!(commit.amounts, None);
    assert!(
        serde_json::to_value(&commit)
            .unwrap()
            .get("amounts")
            .is_none()
    );
}

#[test]
fn the_principal_and_tenant_come_from_the_security_context() {
    let tenant = Uuid::from_u128(7);
    let ctx = SecurityContext::builder()
        .subject_id(Uuid::from_u128(42))
        .subject_tenant_id(tenant)
        .subject_type("gts.cf.core.am.user.v1~")
        .build()
        .unwrap();

    assert_eq!(
        PrincipalSubject::from_security_context(&ctx),
        Some(PrincipalSubject {
            subject_type: "gts.cf.core.am.user.v1~".to_owned(),
            id: Uuid::from_u128(42),
        })
    );
    assert_eq!(caller_tenant_id(&ctx), tenant);

    let untyped = SecurityContext::builder()
        .subject_id(Uuid::from_u128(42))
        .subject_tenant_id(tenant)
        .build()
        .unwrap();
    assert_eq!(PrincipalSubject::from_security_context(&untyped), None);
}
