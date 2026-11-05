//! Status-to-outcome mapping of the single and batch publish calls, against a
//! loopback server that answers one fixed status.

use super::*;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::post;
use chrono::TimeZone;

/// Serves `router` on an ephemeral loopback port; the client's base URL.
async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("resolve the bound address");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve");
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn an_all_duplicate_batch_is_reported_as_duplicate() {
    let base_url = serve(Router::new().route(
        "/event-broker/v1/events:batch",
        post(|| async { StatusCode::OK }),
    ))
    .await;
    let broker = RestBroker::new(base_url).expect("build the client");

    let outcome = broker
        .publish_batch(
            &SecurityContext::anonymous(),
            &[Event {
                id: uuid::Uuid::from_u128(0x0190_f1a2_0000_7000_8000_0000_0000_0c01),
                type_id: crate::GtsTypeId::new("gts.cf.core.events.event.v1~x.eb.sdk.placed.v1~"),
                tenant_id: uuid::Uuid::from_u128(0x0190_f1a2_0000_7000_a000_0000_0000_0c01),
                source: "sdk-test".to_owned(),
                subject: "order-1".to_owned(),
                subject_type: crate::GtsTypeId::new("gts.x.eb.sdk.order.v1~"),
                occurred_at: chrono::Utc
                    .with_ymd_and_hms(2026, 10, 1, 12, 0, 0)
                    .single()
                    .expect("a valid timestamp"),
                trace_parent: None,
                data: None,
                partition: None,
                sequence: None,
                sequence_time: None,
                meta: None,
            }],
        )
        .await
        .expect("a 200 is a success");

    assert_eq!(outcome, IngestOutcome::Duplicate);
}

#[tokio::test]
async fn a_single_publish_answered_200_is_reported_as_duplicate() {
    let base_url =
        serve(Router::new().route("/event-broker/v1/events", post(|| async { StatusCode::OK })))
            .await;
    let broker = RestBroker::new(base_url).expect("build the client");
    let event = Event {
        id: uuid::Uuid::from_u128(0x0190_f1a2_0000_7000_8000_0000_0000_0c02),
        type_id: crate::GtsTypeId::new("gts.cf.core.events.event.v1~x.eb.sdk.placed.v1~"),
        tenant_id: uuid::Uuid::from_u128(0x0190_f1a2_0000_7000_a000_0000_0000_0c01),
        source: "sdk-test".to_owned(),
        subject: "order-1".to_owned(),
        subject_type: crate::GtsTypeId::new("gts.x.eb.sdk.order.v1~"),
        occurred_at: chrono::Utc
            .with_ymd_and_hms(2026, 10, 1, 12, 0, 0)
            .single()
            .expect("a valid timestamp"),
        trace_parent: None,
        data: None,
        partition: None,
        sequence: None,
        sequence_time: None,
        meta: None,
    };

    let published = broker
        .publish(&SecurityContext::anonymous(), &event)
        .await
        .expect("a 200 is a success");
    let published_sync = broker
        .publish_sync(&SecurityContext::anonymous(), &event)
        .await
        .expect("a 200 is a success");

    assert_eq!(
        (published, published_sync),
        (IngestOutcome::Duplicate, IngestOutcome::Duplicate)
    );
}
