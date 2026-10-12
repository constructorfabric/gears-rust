//! Gear-level smoke tests: `init` wires the admission client into
//! `ClientHub`, bad deployments fail startup, and the serve phase resolves
//! the engine, signals ready and stops cleanly.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use admission_control::AdmissionControl;
use admission_control::config::EngineConfig;
use admission_control_sdk::{
    AdmissionClientV1, AdmissionEnginePluginClientV1, AdmissionEnginePluginSpecV1,
    AdmissionRequest, EngineFailure, EngineRequest, EngineResult, FailureCondition,
    PolicyReference, RefusalCause, Verdict,
};
use async_trait::async_trait;
use common::{
    Registration, TestRegistry, WIDGET, config, gear_ctx, gear_ctx_raw, gear_ctx_with, tenant_user,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::Gear;
use toolkit::client_hub::ClientScope;
use toolkit::lifecycle::ReadySignal;
use toolkit_gts::PluginV1;
use toolkit_security::SecurityContext;
use types_registry_sdk::testing::make_test_instance;
use uuid::Uuid;

/// Engine selection by `vendor` alone.
fn engine(vendor: &str) -> EngineConfig {
    EngineConfig {
        vendor: vendor.to_owned(),
        instance_id: None,
    }
}

/// An engine that always answers `result`.
struct FixedEngine(EngineResult);

#[async_trait]
impl AdmissionEnginePluginClientV1 for FixedEngine {
    async fn evaluate(
        &self,
        _ctx: &SecurityContext,
        _request: &EngineRequest,
    ) -> Result<EngineResult, EngineFailure> {
        Ok(self.0.clone())
    }
}

fn request() -> AdmissionRequest {
    AdmissionRequest::new(
        "infrastructure-resource-manager",
        "create",
        WIDGET,
        Uuid::from_u128(0xB1),
    )
    .with_property("name", json!("widget"))
}

fn cause(verdict: &Verdict) -> &RefusalCause {
    match verdict {
        Verdict::Refused(refusal) => &refusal.cause,
        other => panic!("expected a refusal, got {other:?}"),
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

    let verdict = client.admit(&tenant_user(), &request()).await.unwrap();
    assert_eq!(
        cause(&verdict),
        &RefusalCause::CouldNotRun {
            condition: FailureCondition::NoEngine
        }
    );

    cancel.cancel();
    serving.await.unwrap().expect("clean stop");
}

#[tokio::test]
async fn a_configured_engine_is_resolved_and_consulted() {
    const SEGMENT: &str = "acme.test.engine.fixed.v1";
    let denial = PolicyReference {
        bundle_id: Uuid::from_u128(1),
        version_id: Uuid::from_u128(2),
        document_id: Uuid::from_u128(3),
        document_name: "doc".to_owned(),
    };
    let permit = EngineResult::Permit {
        shadow_denials: Vec::new(),
    };
    let deny = EngineResult::Deny {
        reason_code: "POLICY_DENIED".to_owned(),
        denials: vec![denial.clone()],
        shadow_denials: Vec::new(),
    };
    for result in [permit, deny] {
        let (id, content) =
            PluginV1::<AdmissionEnginePluginSpecV1>::build_registration(SEGMENT, "acme", 1)
                .unwrap();
        let instance_id = AsRef::<str>::as_ref(&id).to_owned();
        let registry = TestRegistry::with_instances(
            Registration::Accept,
            [make_test_instance(&instance_id, content)],
        );
        let mut with_engine = config();
        with_engine.engine = Some(engine("acme"));
        let (ctx, hub) = gear_ctx_with(&with_engine, registry);
        hub.register_scoped::<dyn AdmissionEnginePluginClientV1>(
            ClientScope::gts_id(&instance_id),
            Arc::new(FixedEngine(result.clone())),
        );

        let gear = Arc::new(AdmissionControl::default());
        gear.init(&ctx).await.expect("init");
        let client = hub
            .get::<dyn AdmissionClientV1>()
            .expect("client registered");
        let cancel = CancellationToken::new();
        let serving = serve(&gear, &cancel).await;

        let verdict = client.admit(&tenant_user(), &request()).await.unwrap();
        match result {
            EngineResult::Permit { .. } => assert!(verdict.is_admitted(), "{verdict:?}"),
            EngineResult::Deny { .. } => assert_eq!(
                cause(&verdict),
                &RefusalCause::Policy {
                    reason_code: "POLICY_DENIED".to_owned(),
                    denials: vec![denial.clone()],
                }
            ),
            other => panic!("unexpected engine result {other:?}"),
        }

        cancel.cancel();
        serving.await.unwrap().expect("clean stop");
    }
}

#[tokio::test]
async fn a_call_before_serve_reaches_the_configured_engine() {
    // `serve` runs concurrently with other gears' serve phases and with
    // incoming requests, so an early call must not find "no engine".
    let (id, content) = PluginV1::<AdmissionEnginePluginSpecV1>::build_registration(
        "acme.test.engine.early.v1",
        "acme",
        1,
    )
    .unwrap();
    let instance_id = AsRef::<str>::as_ref(&id).to_owned();
    let registry = TestRegistry::with_instances(
        Registration::Accept,
        [make_test_instance(&instance_id, content)],
    );
    let mut with_engine = config();
    with_engine.engine = Some(engine("acme"));
    let (ctx, hub) = gear_ctx_with(&with_engine, registry);
    hub.register_scoped::<dyn AdmissionEnginePluginClientV1>(
        ClientScope::gts_id(&instance_id),
        Arc::new(FixedEngine(EngineResult::Permit {
            shadow_denials: Vec::new(),
        })),
    );

    let gear = Arc::new(AdmissionControl::default());
    gear.init(&ctx).await.expect("init");
    let client = hub
        .get::<dyn AdmissionClientV1>()
        .expect("client registered");
    let verdict = client.admit(&tenant_user(), &request()).await.unwrap();
    assert!(verdict.is_admitted(), "{verdict:?}");
}

#[tokio::test]
async fn zero_numeric_settings_fail_startup() {
    let mut zero_timeout = config();
    zero_timeout.engine_timeout_ms = 0;
    let (ctx, _hub) = gear_ctx(&zero_timeout, Registration::Accept);
    let err = AdmissionControl::default().init(&ctx).await.unwrap_err();
    assert!(format!("{err:#}").contains("engine_timeout_ms"), "{err:#}");
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

    let (ctx, _hub) = gear_ctx_raw(
        &json!({ "admit_on_failure": true }),
        TestRegistry::new(Registration::Accept),
    );
    assert!(AdmissionControl::default().init(&ctx).await.is_err());
}

#[tokio::test]
async fn serve_fails_startup_when_the_configured_engine_is_unresolvable() {
    let mut with_engine = config();
    with_engine.engine = Some(engine("nobody"));
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
