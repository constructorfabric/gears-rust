#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistryImpl;
use toolkit::config::ConfigProvider;
use toolkit::{ClientHub, Gear, GearCtx, RestApiCapability};
use tower::ServiceExt;
use uuid::Uuid;

use super::BssApprovalsGear;

struct StaticConfig(Option<serde_json::Value>);

impl ConfigProvider for StaticConfig {
    fn get_gear_config(&self, name: &str) -> Option<&serde_json::Value> {
        if name == "bss-approvals" {
            self.0.as_ref()
        } else {
            None
        }
    }
}

fn ctx(value: Option<serde_json::Value>) -> GearCtx {
    GearCtx::new(
        "bss-approvals",
        Uuid::from_u128(1),
        Arc::new(StaticConfig(value)),
        Arc::new(ClientHub::new()),
        CancellationToken::new(),
    )
}

async fn status_of(gear: &BssApprovalsGear, context: &GearCtx) -> StatusCode {
    let app = gear
        .register_rest(context, axum::Router::new(), &OpenApiRegistryImpl::new())
        .unwrap();
    app.oneshot(
        Request::builder()
            .uri("/bss-approvals/v1/approval-units")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
    .status()
}

#[tokio::test]
async fn a_missing_block_does_not_serve_and_a_present_block_does() {
    let absent = BssApprovalsGear::default();
    let context = ctx(None);
    Gear::init(&absent, &context).await.unwrap();
    assert_eq!(status_of(&absent, &context).await, StatusCode::NOT_FOUND);

    let nameless = BssApprovalsGear::default();
    let context = ctx(Some(json!({ "database": { "url": "unused" } })));
    Gear::init(&nameless, &context).await.unwrap();
    assert_eq!(status_of(&nameless, &context).await, StatusCode::NOT_FOUND);

    let present = BssApprovalsGear::default();
    let context = ctx(Some(json!({ "config": { "sources": ["pricing"] } })));
    Gear::init(&present, &context).await.unwrap();
    assert_eq!(
        status_of(&present, &context).await,
        StatusCode::UNAUTHORIZED
    );

    let invalid = BssApprovalsGear::default();
    let context = ctx(Some(json!({ "config": { "sources": 1 } })));
    assert!(Gear::init(&invalid, &context).await.is_err());

    let duplicate = BssApprovalsGear::default();
    let context = ctx(Some(
        json!({ "config": { "sources": ["pricing", "pricing"] } }),
    ));
    assert!(Gear::init(&duplicate, &context).await.is_err());
    let blank = BssApprovalsGear::default();
    let context = ctx(Some(json!({ "config": { "sources": [" "] } })));
    assert!(Gear::init(&blank, &context).await.is_err());
}
