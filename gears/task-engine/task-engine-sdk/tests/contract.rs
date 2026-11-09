//! Smoke tests: the contract covers the document, and its examples decode.

use serde_json::json;
use task_engine_sdk::models::{ClaimResponse, CreateTaskRequest, TaskState, WaitCondition};
use task_engine_sdk::{
    TaskEngineApi, UnimplementedTaskEngine, task_engine_api_ir, task_engine_api_rest_http_binding,
};
use toolkit_contract::ir::validation::validate_http_binding;

#[test]
fn contract_has_one_method_per_operation_and_a_valid_rest_binding() {
    let contract = task_engine_api_ir();
    let binding = task_engine_api_rest_http_binding();
    assert_eq!(contract.methods.len(), 35, "openapi.yaml has 35 operations");
    assert_eq!(binding.methods.len(), 35);
    validate_http_binding(&contract, &binding).expect("REST binding is valid");
    assert_eq!(
        binding
            .find_method("get_task")
            .map(|m| m.path_template.as_str()),
        Some("/tasks/{task_id}")
    );
    assert_eq!(
        binding
            .find_method("list_queue_events")
            .map(|m| m.path_template.as_str()),
        Some("/queues/{queue_id}/events")
    );
}

#[test]
fn claim_response_example_decodes() {
    // `ClaimResponse.example` of openapi.yaml, with the abbreviated task filled in.
    let body = json!({
        "session_id": "0192f0c4-0000-7000-8000-000000000001",
        "domain": "default",
        "heartbeat_interval_secs": 10,
        "claims": [{
            "task": {
                "id": "0192f0c4-0000-7000-8000-0000000000a1",
                "type": "gts.cf.core.tasks.task.v1~acme.reports._.render.v1~",
                "queue": "gts.cf.core.tasks.queue.v1~acme.reports._.render.v1",
                "owner_tenant_id": "0192f0c4-0000-7000-8000-0000000000f1",
                "created_by": "svc-reports",
                "state": "claimed",
                "urgency": "normal",
                "priority": 100,
                "attempt": 1,
                "created_at": "2026-11-03T10:00:00Z",
                "updated_at": "2026-11-03T10:00:00Z"
            },
            "lease": { "lease_token": "6f1c2d3e-4b5a-4c6d-8e7f-001122334455", "lease_epoch": 1,
                       "attempt": 1, "lease_secs": 30, "domain": "default" },
            "authority": { "mode": "creator_verified", "checked": [], "checked_at": "2026-11-03T10:00:00Z" },
            "resumed": null
        }]
    });
    let resp: ClaimResponse = serde_json::from_value(body).expect("decodes");
    assert_eq!(resp.claims[0].task.state, TaskState::Claimed);
}

#[test]
fn create_example_round_trips_and_unknown_enum_values_are_absorbed() {
    let task = CreateTaskRequest::new(
        gts::GtsTypeId::new("gts.cf.core.tasks.task.v1~acme.reports._.render.v1~"),
        json!({ "report_id": "r-42", "format": "pdf" }),
        "key-1",
    );
    let wire = serde_json::to_value(&task).expect("encodes");
    assert_eq!(
        wire["type"],
        "gts.cf.core.tasks.task.v1~acme.reports._.render.v1~"
    );
    assert_eq!(
        serde_json::from_value::<CreateTaskRequest>(wire).expect("decodes"),
        task
    );

    assert_eq!(
        serde_json::from_value::<TaskState>(json!("from_the_future")).expect("open enum"),
        TaskState::Unknown
    );
    let cond: WaitCondition = serde_json::from_value(json!({
        "id": "0192f0c4-0000-7000-8000-0000000000c1",
        "kind": "timer",
        "state": "pending",
        "resume_at": "2026-11-04T00:00:00Z"
    }))
    .expect("stored condition decodes");
    assert_eq!(
        cond.kind(),
        task_engine_sdk::models::WaitConditionKind::Timer
    );
}

#[tokio::test]
async fn placeholder_answers_unimplemented() {
    let engine: &dyn TaskEngineApi = &UnimplementedTaskEngine;
    let ctx = toolkit_security::SecurityContext::anonymous();
    let err = engine
        .get_task(ctx, uuid::Uuid::nil())
        .await
        .expect_err("placeholder");
    assert!(err.to_string().contains("get_task"), "{err}");
}

/// Every enum the engine sends must absorb a value this SDK does not know
/// (`#[serde(other)] Unknown`). Forgetting the attribute compiles fine but makes
/// one unknown value fail a whole response, so it is checked here. Add new
/// response enums to this list.
#[test]
fn open_enums_decode_unknown_values() {
    use task_engine_sdk::models::{
        AuthorityMode, AuthorityVerdict, BulkOutcome, CancelMode, DependencyMode, ErrorFault,
        HeartbeatOutcome, OnPredecessorFailure, OnTimeout, OperatedBy, PredecessorOutcome,
        ProgressEvents, QueueKind, ResultCode, RetryStrategy, TenantFairness, TimedOutReason,
        Urgency, WaitConditionKind, WaitConditionState, WaitMode, WaitOutcome,
    };

    macro_rules! assert_open {
        ($($ty:ty),+ $(,)?) => {$( assert_open::<$ty>(stringify!($ty), &<$ty>::Unknown); )+};
    }

    assert_open!(
        TaskState,
        ResultCode,
        Urgency,
        CancelMode,
        DependencyMode,
        OnPredecessorFailure,
        WaitMode,
        WaitConditionKind,
        WaitConditionState,
        OnTimeout,
        ErrorFault,
        QueueKind,
        RetryStrategy,
        TenantFairness,
        OperatedBy,
        ProgressEvents,
        WaitOutcome,
        TimedOutReason,
        AuthorityMode,
        AuthorityVerdict,
        HeartbeatOutcome,
        BulkOutcome,
        PredecessorOutcome,
    );
}

fn assert_open<T>(name: &str, unknown: &T)
where
    T: serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let decoded = serde_json::from_value::<T>(json!("value_from_a_newer_engine"))
        .unwrap_or_else(|e| panic!("{name} is not open: {e}"));
    assert_eq!(&decoded, unknown, "{name}");
}

#[test]
fn gts_ids_are_validated_on_decode() {
    use task_engine_sdk::models::ClaimRequest;
    let ok = json!({ "worker_id": "w-1", "queues": ["gts.cf.core.tasks.queue.v1~acme.reports._.render.v1"] });
    serde_json::from_value::<ClaimRequest>(ok).expect("valid queue instance id");
    let bad = json!({ "worker_id": "w-1", "queues": ["not-a-gts-id"] });
    assert!(
        serde_json::from_value::<ClaimRequest>(bad).is_err(),
        "malformed queue id is rejected"
    );
}

#[test]
fn set_wait_200_decodes_as_either_shape() {
    use task_engine_sdk::models::SetWaitOutcome;
    let stored: SetWaitOutcome = serde_json::from_value(json!({
        "wait_group": {
            "id": "0192f0c4-0000-7000-8000-0000000000d1",
            "mode": "all",
            "parked": true,
            "conditions": []
        },
        "task_state": "waiting"
    }))
    .expect("SetWaitResponse decodes");
    assert!(stored.stored().is_some());

    let satisfied: SetWaitOutcome = serde_json::from_value(json!({
        "parked": false,
        "result": {
            "wait_group_id": "0192f0c4-0000-7000-8000-0000000000d1",
            "outcome": "satisfied",
            "conditions": [{
                "id": "0192f0c4-0000-7000-8000-0000000000c1",
                "kind": "timer",
                "state": "satisfied",
                "result": { "resume_at": "2026-11-04T00:00:00Z" }
            }]
        }
    }))
    .expect("ParkResponse decodes");
    assert!(satisfied.stored().is_none());
    assert!(satisfied.satisfied_result().is_some());
}

#[test]
fn queue_event_and_optional_bodies_follow_the_document() {
    use task_engine_sdk::models::{
        BulkCancelRequest, CancelRequest, PredecessorOutcome, Queue, QueueHistoryEventPage,
        QueueReleasedData, ReenqueueRequest, SearchTasksQuery, WaitConditionSpec,
    };

    let page: QueueHistoryEventPage = serde_json::from_value(json!({
        "items": [{
            "id": "0192f0c4-0000-7000-8000-0000000000e1",
            "event_type": "queueReleased",
            "actor": null,
            "timestamp": "2026-11-03T10:00:00Z",
            "payload": {
                "queue": "gts.cf.core.tasks.queue.v1~acme.reports._.render.v1",
                "automatic": true
            }
        }],
        "page_info": { "next_cursor": null, "prev_cursor": null, "limit": 25 }
    }))
    .expect("queue history decodes");
    let released: QueueReleasedData =
        serde_json::from_value(page.items[0].payload.clone()).expect("payload decodes");
    assert!(released.automatic && released.actor.is_none());

    // `counts` is only sent to callers holding `configure`.
    let queue: Queue = serde_json::from_value(json!({
        "id": "gts.cf.core.tasks.queue.v1~cf.core.tasks.dead_letter.v1",
        "kind": "dead_letter",
        "domains": [],
        "hold": null
    }))
    .expect("queue without counts decodes");
    assert!(queue.counts.is_none());

    // Optional bodies and server-side defaults encode as `{}` / without `mode`.
    assert_eq!(
        serde_json::to_value(CancelRequest::default()).unwrap(),
        json!({})
    );
    assert_eq!(
        serde_json::to_value(ReenqueueRequest::default()).unwrap(),
        json!({})
    );
    let bulk = BulkCancelRequest::new(vec![uuid::Uuid::nil()]);
    assert!(serde_json::to_value(&bulk).unwrap().get("mode").is_none());

    // A timer condition carries no timeout fields.
    let timer = WaitConditionSpec::timer("2026-11-04T00:00:00Z".parse().unwrap());
    assert_eq!(
        serde_json::to_value(&timer).unwrap(),
        json!({ "kind": "timer", "resume_at": "2026-11-04T00:00:00Z" })
    );

    assert!(PredecessorOutcome::FailedIgnored.is_satisfied());
    assert!(!PredecessorOutcome::Failed.is_satisfied());
    assert_eq!(
        SearchTasksQuery::has_tag_value("project", "q'4"),
        "has_tag('project', 'q''4')"
    );
}
