#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use github_mirror::api::rest::dto::SessionTelemetryDto;
use github_mirror::domain::error::DomainError;
use github_mirror::domain::ports::github::ForceMode;
use github_mirror::domain::service::SyncRequest;
use github_mirror::domain::sync::TelemetrySnapshot;
use toolkit_security::SecurityContext;
use uuid::Uuid;

const OWNER: &str = "rust-lang";
const NAME: &str = "rust";

async fn synced_telemetry(
    service: &common::ConcreteService,
    pump: &mut common::SyncPump,
    ctx: &SecurityContext,
) -> TelemetrySnapshot {
    let queued = service
        .enqueue_sync(ctx, OWNER, NAME, None, ForceMode::None, None)
        .await
        .expect("the sync must queue");
    assert_eq!(pump.drain(service).await, 1);
    let session = service
        .get_session(ctx, queued.session_id)
        .await
        .expect("the session must be readable");
    let raw = session
        .telemetry_json
        .expect("a finished session must carry its telemetry");
    serde_json::from_str(&raw).expect("the stored telemetry must parse")
}

#[tokio::test]
async fn a_first_sync_refines_what_it_indexes_and_a_second_skips_it() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service_with_github(
        common::inmem_db().await,
        "https://api.github.com",
        Arc::new(common::FakeGithub {
            result: Some(common::fetched_repository()),
        }),
    );
    let mut pump = common::SyncPump::take(&service).await;

    let first = synced_telemetry(&service, &mut pump, &ctx).await;
    assert!(first.entities_indexed > 0, "{first:?}");
    assert_eq!(first.entities_refined, first.entities_indexed, "{first:?}");
    assert_eq!(first.entities_skipped, 0, "{first:?}");

    let second = synced_telemetry(&service, &mut pump, &ctx).await;
    assert_eq!(second.entities_refined, 0, "{second:?}");
    assert_eq!(
        second.entities_skipped, second.entities_indexed,
        "{second:?}"
    );
}

#[tokio::test]
async fn a_finished_sync_leaves_no_task_pending_or_running() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service_with_github(
        common::inmem_db().await,
        "https://api.github.com",
        Arc::new(common::FakeGithub {
            result: Some(common::fetched_repository()),
        }),
    );
    let mut pump = common::SyncPump::take(&service).await;

    let telemetry = synced_telemetry(&service, &mut pump, &ctx).await;

    assert_eq!(telemetry.tasks_pending, 0, "{telemetry:?}");
    assert_eq!(telemetry.tasks_running, 0, "{telemetry:?}");
    assert_eq!(telemetry.tasks_failed, 0, "{telemetry:?}");
    assert!(telemetry.tasks_done > 0, "{telemetry:?}");
}

#[tokio::test]
async fn sync_now_refuses_a_telemetry_file_name_that_leaves_its_folder() {
    let service = common::service("https://api.github.com").await;

    let err = service
        .sync_now(
            &common::caller(),
            OWNER,
            NAME,
            Some("../escape.jsonl".to_owned()),
            SyncRequest::default(),
        )
        .await
        .unwrap_err();

    assert!(
        matches!(&err, DomainError::Validation { field, .. } if field == "telemetry_file"),
        "{err:?}"
    );
}

#[test]
fn the_cache_hit_ratio_is_the_share_of_rest_calls_answered_304() {
    let dto = SessionTelemetryDto::from(TelemetrySnapshot {
        rest_calls: 1250,
        not_modified: 1150,
        ..TelemetrySnapshot::default()
    });

    assert!((dto.cache_hit_ratio - 0.92).abs() < 1e-9, "{dto:?}");
}

#[test]
fn a_session_with_no_rest_calls_has_a_zero_cache_hit_ratio() {
    let dto = SessionTelemetryDto::from(TelemetrySnapshot::default());

    assert!(dto.cache_hit_ratio.abs() < f64::EPSILON, "{dto:?}");
}
