#![allow(clippy::expect_used)]

use quota_enforcement_sdk::PeriodType;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{CreateQuotaDto, UpdateQuotaDto};
use crate::domain::error::DomainError;
use crate::domain::quotas::{CreateQuotaRequest, Presence, UpdateQuotaRequest};
use crate::domain::tokens;
use crate::test_support::{LLM_USER_PROJECTION, METRIC_TOKENS};

fn create_body() -> Value {
    json!({
        "tenant_id": Uuid::from_u128(1),
        "subject": { "projection_type": LLM_USER_PROJECTION, "subject_id": "u1" },
        "metric": METRIC_TOKENS,
        "quota_type": "gts.cf.core.qe.quota_type.v1~cf.core.qe.consumption.v1",
        "period": "gts.cf.core.qe.period_type.v1~cf.core.qe.month.v1",
        "enforcement_mode": "gts.cf.core.qe.enforcement_type.v1~cf.core.qe.hard.v1",
        "cap": -1,
        "source": "gts.cf.core.qe.source_type.v1~cf.core.qe.operator.v1"
    })
}

#[test]
fn create_bodies_keep_period_presence_and_signed_caps() {
    let dto: CreateQuotaDto = serde_json::from_value(create_body()).expect("body");
    assert_eq!(dto.period, Presence::Value(PeriodType::Month));
    assert_eq!(dto.cap, Some(-1), "a negative cap reaches the domain");
    let request = CreateQuotaRequest::try_from(dto).expect("request");
    assert_eq!(
        request.subject.projection_type.as_ref(),
        LLM_USER_PROJECTION
    );
    assert_eq!(request.metadata, None);

    let mut body = create_body();
    body["period"] = Value::Null;
    let dto: CreateQuotaDto = serde_json::from_value(body).expect("null period");
    assert_eq!(dto.period, Presence::Null);
    let mut body = create_body();
    body.as_object_mut().expect("object").remove("period");
    let dto: CreateQuotaDto = serde_json::from_value(body).expect("absent period");
    assert_eq!(dto.period, Presence::Absent);

    let mut body = create_body();
    body["bogus"] = json!(1);
    assert!(
        serde_json::from_value::<CreateQuotaDto>(body).is_err(),
        "unknown keys"
    );
    let mut body = create_body();
    body["constraint_contract"] = json!({ "type_id": "x", "version": 1 });
    assert!(
        serde_json::from_value::<CreateQuotaDto>(body).is_err(),
        "the contract is never caller-supplied"
    );
    let mut body = create_body();
    body["enforcement_mode"] = json!("gts.cf.core.qe.enforcement_type.v1~cf.core.qe.soft.v1");
    assert!(
        serde_json::from_value::<CreateQuotaDto>(body).is_err(),
        "the enforcement mode enum is closed"
    );
    let mut body = create_body();
    body["subject"]["projection_type"] = json!("not a type id");
    let dto: CreateQuotaDto = serde_json::from_value(body).expect("body");
    assert_eq!(
        CreateQuotaRequest::try_from(dto).expect_err("projection parses in the domain"),
        DomainError::InvalidArgument {
            field: "subject.projection_type",
            reason: tokens::PROJECTION_INVALID
        }
    );
}

#[test]
fn update_bodies_distinguish_absent_null_and_value_for_every_gated_field() {
    let dto: UpdateQuotaDto = serde_json::from_value(json!({})).expect("empty body");
    assert_eq!(dto, UpdateQuotaDto::default());
    let dto: UpdateQuotaDto = serde_json::from_value(json!({
        "metric": null,
        "quota_type": "gts.cf.core.qe.quota_type.v1~cf.core.qe.rate.v1",
        "cap": null,
        "validity_window": null,
        "notification_thresholds": []
    }))
    .expect("body");
    assert_eq!(dto.metric, Presence::Null);
    assert!(matches!(dto.quota_type, Presence::Value(Value::String(_))));
    assert_eq!(dto.period, Presence::Absent);
    assert_eq!(dto.cap, Presence::Null);
    assert_eq!(dto.validity_window, Presence::Null);
    assert_eq!(dto.notification_thresholds, Some(Vec::new()));
    let request = UpdateQuotaRequest::from(dto);
    assert_eq!(request.cap, Presence::Null);
    assert_eq!(request.validity_window, Presence::Null);

    let dto: UpdateQuotaDto = serde_json::from_value(json!({
        "cap": 7,
        "validity_window": { "start": "2026-01-01T00:00:00Z" }
    }))
    .expect("body");
    assert_eq!(dto.cap, Presence::Value(7));
    assert!(matches!(dto.validity_window, Presence::Value(_)));
    assert!(
        serde_json::from_value::<UpdateQuotaDto>(json!({ "bogus": 1 })).is_err(),
        "unknown keys"
    );
}
