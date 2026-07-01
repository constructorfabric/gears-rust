use super::*;
use chat_engine_sdk::models::LifecycleState;
use std::time::Duration;

#[test]
fn invalid_transition_yields_conflict() {
    let err =
        ChatEngineError::invalid_transition(LifecycleState::HardDeleted, LifecycleState::Active);
    assert!(matches!(err, ChatEngineError::Conflict { .. }));
    assert!(err.to_string().contains("hard_deleted -> active"));
}

#[test]
fn db_err_record_not_found_maps_to_not_found() {
    let err: ChatEngineError = DbErr::RecordNotFound("missing".into()).into();
    assert!(matches!(
        err,
        ChatEngineError::NotFound {
            resource: "record",
            ..
        }
    ));
}

#[test]
fn db_err_other_maps_to_internal() {
    let err: ChatEngineError = DbErr::Custom("boom".into()).into();
    assert!(matches!(err, ChatEngineError::Internal { .. }));
}

#[test]
fn plugin_error_invalid_input_maps_to_bad_request() {
    let err: ChatEngineError = PluginError::invalid_input("payload too small").into();
    assert!(matches!(err, ChatEngineError::BadRequest { .. }));
}

#[test]
fn plugin_error_unauthorized_maps_to_forbidden() {
    let err: ChatEngineError = PluginError::unauthorized("token expired").into();
    assert!(matches!(err, ChatEngineError::Forbidden { .. }));
}

#[test]
fn plugin_error_not_found_maps_to_not_found() {
    let err: ChatEngineError = PluginError::not_found("model gpt-99").into();
    assert!(matches!(
        err,
        ChatEngineError::NotFound {
            resource: "plugin_resource",
            ..
        }
    ));
}

#[test]
fn plugin_error_rate_limited_preserves_retry_after() {
    let err: ChatEngineError = PluginError::rate_limited(Some(Duration::from_secs(5))).into();
    match err {
        ChatEngineError::BackendUnavailable { retry_after, .. } => {
            assert_eq!(retry_after, Some(Duration::from_secs(5)));
        }
        other => panic!("expected BackendUnavailable, got {other:?}"),
    }
}

#[test]
fn plugin_error_transient_and_timeout_map_to_backend_unavailable() {
    let t: ChatEngineError = PluginError::transient("upstream 502").into();
    let to: ChatEngineError = PluginError::timeout().into();
    assert!(matches!(t, ChatEngineError::BackendUnavailable { .. }));
    assert!(matches!(to, ChatEngineError::BackendUnavailable { .. }));
}

#[test]
fn plugin_error_internal_maps_to_internal() {
    let err: ChatEngineError = PluginError::internal("bug").into();
    assert!(matches!(err, ChatEngineError::Internal { .. }));
}

#[test]
fn anyhow_maps_to_internal() {
    let err: ChatEngineError = anyhow::anyhow!("something broke").into();
    assert!(matches!(err, ChatEngineError::Internal { .. }));
}
