#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use toolkit_canonical_errors::Problem;

use crate::gts::ADMISSION_CONTROL_RESOURCE;

fn problem(cause: &RefusalCause) -> serde_json::Value {
    serde_json::to_value(Problem::from(cause.to_canonical_error())).unwrap()
}

#[test]
fn builtin_refusal_maps_to_failed_precondition_naming_policy() {
    let cause = RefusalCause::BuiltinPolicy {
        policy_id: "reserved-name-prefix".to_owned(),
    };
    let err = cause.to_canonical_error();
    assert_eq!(err.status_code(), 400);
    assert_eq!(err.resource_type(), Some(ADMISSION_CONTROL_RESOURCE));
    let v = &problem(&cause)["context"]["violations"][0];
    assert_eq!(v["type"], reason::BUILTIN_POLICY_REFUSED);
    assert_eq!(v["subject"], "reserved-name-prefix");
    assert!(!cause.is_retryable());
}

#[test]
fn policy_refusal_carries_reason_code() {
    let cause = RefusalCause::Policy {
        reason_code: "POLICY_DENIED".to_owned(),
        denials: Vec::new(),
    };
    let v = &problem(&cause)["context"]["violations"][0];
    assert_eq!(v["type"], reason::POLICY_REFUSED);
    assert_eq!(v["subject"], "POLICY_DENIED");
}

#[test]
fn request_too_large_names_bound() {
    for bound in [SizeBound::ContextBytes, SizeBound::PropertyCount] {
        let cause = RefusalCause::RequestTooLarge { bound };
        assert_eq!(cause.to_canonical_error().status_code(), 400);
        let fv = &problem(&cause)["context"]["field_violations"][0];
        assert_eq!(fv["field"], bound.as_str());
        assert_eq!(fv["reason"], reason::REQUEST_TOO_LARGE);
    }
}

#[test]
fn could_not_run_is_retryable_service_unavailable() {
    for condition in FailureCondition::ALL {
        let cause = RefusalCause::CouldNotRun { condition };
        let err = cause.to_canonical_error();
        assert_eq!(err.status_code(), 503);
        assert_eq!(
            err.detail(),
            format!("{}: {}", reason::COULD_NOT_RUN, condition.as_str())
        );
        assert!(cause.is_retryable());
    }
}

#[test]
fn failure_condition_labels_match_serde() {
    for c in FailureCondition::ALL {
        assert_eq!(serde_json::to_value(c).unwrap(), c.as_str());
    }
}

#[test]
fn verdict_helpers() {
    let id = Uuid::from_u128(1);
    assert!(Verdict::Admitted(Admission { correlation_id: id }).is_admitted());
    let refused = Verdict::Refused(Refusal {
        cause: RefusalCause::RequestTooLarge {
            bound: SizeBound::ContextBytes,
        },
        correlation_id: id,
    });
    assert!(!refused.is_admitted());
    assert_eq!(refused.correlation_id(), id);
}

#[test]
fn refusal_event_round_trips_and_omits_absent_optionals() {
    let event = RefusalEvent {
        correlation_id: Uuid::from_u128(1),
        occurred_at: OffsetDateTime::UNIX_EPOCH,
        enforcing_gear: "g".to_owned(),
        action: "create".to_owned(),
        resource_type: "gts.cf.core.test.widget.v1~".to_owned(),
        resource_id: None,
        resource_tenant_id: Uuid::from_u128(2),
        subject_id: Uuid::from_u128(3),
        subject_tenant_id: Uuid::from_u128(4),
        enforced: false,
        cause: RefusalEventCause::Policy,
        builtin_policy_id: None,
        condition: None,
        policy: Some(PolicyReference {
            bundle_id: Uuid::from_u128(5),
            version_id: Uuid::from_u128(6),
            document_id: Uuid::from_u128(7),
            document_name: "doc".to_owned(),
        }),
        property_names: vec!["size".to_owned()],
    };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["cause"], "policy");
    assert_eq!(value["enforced"], false);
    assert!(value.get("resource_id").is_none());
    assert!(value.get("condition").is_none());
    assert_eq!(
        serde_json::from_value::<RefusalEvent>(value).unwrap(),
        event
    );
}

#[test]
fn admission_request_builder() {
    let req = AdmissionRequest::new("g", "create", "gts.cf.core.test.widget.v1~", Uuid::nil())
        .with_resource_id(Uuid::from_u128(9))
        .with_property("k", serde_json::json!(1));
    assert_eq!(req.resource_id, Some(Uuid::from_u128(9)));
    assert_eq!(req.properties.len(), 1);
}

#[test]
fn identifier_grammar_accepts_the_documented_shape() {
    for ok in [
        "create",
        "infrastructure-resource-manager",
        "0",
        "a.b_c:d-e",
        "v1:resource.create",
        &"a".repeat(IDENTIFIER_MAX_LEN),
    ] {
        assert!(validate_identifier("action", ok).is_ok(), "{ok}");
    }
}

#[test]
fn identifier_grammar_rejects_everything_else_without_echoing_the_value() {
    let too_long = "a".repeat(IDENTIFIER_MAX_LEN + 1);
    for bad in [
        "",
        "Create",
        "-create",
        ".create",
        "_create",
        ":create",
        "create now",
        "create\n",
        "caf\u{e9}",
        "a/b",
        "secret=hunter2",
        too_long.as_str(),
    ] {
        let err = validate_identifier("enforcing_gear", bad).unwrap_err();
        assert_eq!(err.status_code(), 400);
        assert_eq!(err.resource_type(), Some(ADMISSION_CONTROL_RESOURCE));
        let p = serde_json::to_value(Problem::from(err)).unwrap();
        let fv = &p["context"]["field_violations"][0];
        assert_eq!(fv["field"], "enforcing_gear");
        assert_eq!(fv["reason"], reason::INVALID_IDENTIFIER);
        if !bad.is_empty() {
            assert!(
                !p.to_string().contains(bad),
                "the rejected value is never echoed"
            );
        }
    }
}

#[test]
fn resource_type_accepts_gts_type_identifiers_up_to_the_bound() {
    for ok in [
        "gts.cf.core.test.widget.v1~",
        "gts.cf.core.events.event.v1~cf.core.admission_control.refusal.v1~",
    ] {
        assert!(ok.len() <= RESOURCE_TYPE_MAX_LEN);
        assert!(validate_resource_type(ok).is_ok(), "{ok}");
    }
}

#[test]
fn resource_type_rejects_free_text_instances_patterns_and_oversize_without_echo() {
    let oversize = format!(
        "gts.cf.core.{}.widget.v1~",
        "a".repeat(RESOURCE_TYPE_MAX_LEN)
    );
    let huge = format!("Free text {}", "x".repeat(2 * 1024 * 1024));
    for bad in [
        "",
        "Free text resource type",
        "gts.cf.core.test.widget.v1",
        "gts.cf.core.*",
        " gts.cf.core.test.widget.v1~",
        "gts.cf.core.test.widget.v1~ ",
        "GTS.cf.core.test.widget.v1~",
        "gts.cf.toolkit.plugins.plugin.v1~cf.core.admission_control.engine.v1~acme.x.engine.v1",
        oversize.as_str(),
        huge.as_str(),
    ] {
        let err = validate_resource_type(bad).unwrap_err();
        assert_eq!(err.status_code(), 400);
        let p = serde_json::to_value(Problem::from(err)).unwrap();
        let fv = &p["context"]["field_violations"][0];
        assert_eq!(fv["field"], "resource_type");
        assert_eq!(fv["reason"], reason::INVALID_IDENTIFIER);
        if !bad.trim().is_empty() {
            assert!(!p.to_string().contains(bad.trim()), "never echoed");
        }
    }
}
