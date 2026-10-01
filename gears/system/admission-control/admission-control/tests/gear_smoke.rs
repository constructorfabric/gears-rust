//! Gear-level smoke tests: `init` wires the admission client into
//! `ClientHub`, bad deployments fail startup, and the serve phase resolves
//! the engine, signals ready and stops cleanly.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use admission_control::AdmissionControl;
use admission_control_sdk::{
    AdmissionClientV1, AdmissionRequest, FailureCondition, RefusalCause, Verdict,
};
use common::{RESERVED_ID, Registration, WIDGET, config, gear_ctx, tenant_user};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::Gear;
use toolkit::lifecycle::ReadySignal;
use uuid::Uuid;

fn request(name: &str) -> AdmissionRequest {
    AdmissionRequest::new(
        "infrastructure-resource-manager",
        "create",
        WIDGET,
        Uuid::from_u128(0xB1),
    )
    .with_property("name", json!(name))
}

fn cause(verdict: &Verdict) -> &RefusalCause {
    match verdict {
        Verdict::Refused(refusal) => &refusal.cause,
        other @ Verdict::Admitted(_) => panic!("expected a refusal, got {other:?}"),
    }
}

async fn serve(
    gear: &Arc<AdmissionControl>,
    cancel: &CancellationToken,
) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(Arc::clone(gear).serve(cancel.clone(), ReadySignal::from_sender(tx)));
    rx.await.expect("serve signals ready");
    handle
}

#[tokio::test]
async fn init_registers_the_client_and_refuses_fail_closed_without_an_engine() {
    let (ctx, hub) = gear_ctx(&config(), Registration::Accept);
    let gear = Arc::new(AdmissionControl::default());
    assert!(gear.admission_service().is_none(), "no service before init");
    gear.init(&ctx).await.expect("init");
    assert!(
        gear.admission_service().is_some(),
        "the service exists after init"
    );
    let client = hub
        .get::<dyn AdmissionClientV1>()
        .expect("client registered");

    let cancel = CancellationToken::new();
    let serving = serve(&gear, &cancel).await;

    let verdict = client
        .admit(&tenant_user(), &request("fine"))
        .await
        .unwrap();
    assert_eq!(
        cause(&verdict),
        &RefusalCause::CouldNotRun {
            condition: FailureCondition::NoEngine
        }
    );
    let verdict = client
        .admit(&tenant_user(), &request("cf-widget"))
        .await
        .unwrap();
    assert_eq!(
        cause(&verdict),
        &RefusalCause::BuiltinPolicy {
            policy_id: RESERVED_ID.to_owned()
        }
    );

    cancel.cancel();
    serving.await.unwrap().expect("clean stop");
}

#[tokio::test]
async fn an_unreachable_registry_does_not_block_startup() {
    let (ctx, _hub) = gear_ctx(&config(), Registration::Unreachable);
    let gear = Arc::new(AdmissionControl::default());
    gear.init(&ctx).await.expect("init");
    let cancel = CancellationToken::new();
    let serving = serve(&gear, &cancel).await;
    cancel.cancel();
    serving.await.unwrap().expect("clean stop");
}

#[tokio::test]
async fn init_fails_on_a_rejected_event_type_or_bad_configuration() {
    let (ctx, _hub) = gear_ctx(&config(), Registration::Reject);
    let err = AdmissionControl::default().init(&ctx).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("registration rejected"),
        "{err:#}"
    );

    let (ctx, _hub) = gear_ctx(&json!({ "admit_on_failure": true }), Registration::Accept);
    assert!(AdmissionControl::default().init(&ctx).await.is_err());

    let mut unresolvable = config();
    unresolvable["builtin_policies"][0]["resource_types"] = json!(["gts.cf.core.test.missing.v1~"]);
    let (ctx, _hub) = gear_ctx(&unresolvable, Registration::Accept);
    let err = AdmissionControl::default().init(&ctx).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("resource type unresolved"),
        "{err:#}"
    );
}

#[tokio::test]
async fn serve_fails_startup_when_the_configured_engine_is_unresolvable() {
    let mut with_engine = config();
    with_engine["engine"] = json!({ "vendor": "nobody" });
    let (ctx, _hub) = gear_ctx(&with_engine, Registration::Accept);
    let gear = Arc::new(AdmissionControl::default());
    gear.init(&ctx).await.expect("init");

    let (tx, rx) = tokio::sync::oneshot::channel();
    let err = Arc::clone(&gear)
        .serve(CancellationToken::new(), ReadySignal::from_sender(tx))
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("cannot be resolved"), "{err:#}");
    assert!(rx.await.is_err(), "ready must not be signalled");
}
