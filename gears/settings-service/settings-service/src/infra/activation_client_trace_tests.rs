//! One trace from the broker's delivery to the report: the group's
//! `change_notification.consume` span continues the publisher's trace, and
//! the handle's `change_notification.handle` span — where the handler runs and
//! the outcomes are reported — is a child of it, though the handle's own task
//! takes the bundle after the consume span has closed.

use tracing::Instrument as _;

use super::*;
use crate::domain::activation::testing::{SpanCapture, gear_attributes};

const CARRIED_TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const PUBLISHER_SPAN: &str = "00f067aa0ba902b7";

/// The service, each report made inside a `probe.report` span.
struct ProbedBackend(Arc<FakeBackend>);

#[async_trait]
impl ActivationBackend for ProbedBackend {
    async fn register(
        &self,
        ctx: &SecurityContext,
        key: &SettingKey,
        tenant: Uuid,
        identity: &InstanceIdentity,
    ) -> Result<(), RegisterError> {
        self.0.register(ctx, key, tenant, identity).await
    }

    async fn unsubscribe(
        &self,
        ctx: &SecurityContext,
        key: &SettingKey,
        tenant: Uuid,
        instance_id: Uuid,
    ) -> Result<(), UnsubscribeFailure> {
        self.0.unsubscribe(ctx, key, tenant, instance_id).await
    }

    async fn report(
        &self,
        ctx: &SecurityContext,
        response: BackResponse,
    ) -> Result<crate::domain::activation::ReportResult, DomainError> {
        self.0
            .report(ctx, response)
            .instrument(tracing::info_span!("probe.report"))
            .await
    }
}

/// A handler that applies every bundle inside a `probe.handler` span.
struct ProbedHandler(Arc<Recorder>);

#[async_trait]
impl SettingChangeHandler for ProbedHandler {
    async fn on_change(&self, bundle: ChangeBundle) -> Vec<(SettingKey, ActivationOutcome)> {
        self.0
            .on_change(bundle)
            .instrument(tracing::info_span!("probe.handler"))
            .await
    }
}

/// A plugged client over the probed service. Built after the span capture,
/// which stays this thread's subscriber: the fixture's log capture is not
/// installed.
struct Traced {
    broker: Arc<ScriptedBroker>,
    backend: Arc<FakeBackend>,
    client: ActivationClient,
}

fn traced() -> Traced {
    let hub = Arc::new(ClientHub::new());
    let broker = Arc::new(ScriptedBroker::on_topic(ACTIVATION_TOPIC_ID));
    hub.register::<dyn EventBrokerApi>(Arc::clone(&broker) as Arc<dyn EventBrokerApi>);
    let backend = Arc::new(FakeBackend::default());
    let client = ActivationClient::new(ConsumerDeps {
        backend: Arc::new(ProbedBackend(Arc::clone(&backend))) as Arc<dyn ActivationBackend>,
        reader: Arc::new(FakeReader::default()) as Arc<dyn SettingsReaderClient>,
        metrics: Arc::new(RecordingNotifyMetrics::default()) as Arc<dyn NotifyMetrics>,
        hub,
        timing: timing(),
        identity: instance(),
    });
    Traced {
        broker,
        backend,
        client,
    }
}

impl Traced {
    async fn subscribe(&self, recorder: &Arc<Recorder>) -> SubscriptionHandle {
        self.client
            .subscribe(
                &ctx_of(GEAR),
                vec![key("a")],
                tenant(1).to_string(),
                Arc::new(ProbedHandler(Arc::clone(recorder))) as Arc<dyn SettingChangeHandler>,
            )
            .await
            .expect("subscribe answers a handle")
    }
}

#[tokio::test]
async fn the_handler_and_the_report_run_in_a_child_of_the_consume_span_in_the_publishers_trace() {
    let spans = SpanCapture::start();
    let t = traced();
    let handle = t.subscribe(&Recorder::applying()).await;
    until("the key to register", || is_registered(&handle, "a")).await;
    until("the stream to open", || t.broker.opened_streams() >= 1).await;
    let event = traced_wire(
        CHANGE_NOTIFICATION_EVENT_TYPE,
        tenant(1),
        Utc::now(),
        notification(tenant(1), GEAR, &[("a", tenant(0xca))]),
        Some(format!("00-{CARRIED_TRACE}-{PUBLISHER_SPAN}-01")),
    );
    t.broker.send(Ok(WireFrame::Event(event)));
    until("the report", || t.backend.reports().len() == 1).await;
    until("the handle span to close", || {
        !spans.named("change_notification.handle").is_empty()
    })
    .await;

    let consume = spans.one("change_notification.consume");
    let handle_span = spans.one("change_notification.handle");
    let handler = spans.one("probe.handler");
    let report = spans.one("probe.report");
    assert_eq!(consume.span_context.trace_id().to_string(), CARRIED_TRACE);
    assert_eq!(consume.parent_span_id.to_string(), PUBLISHER_SPAN);
    for span in [&handle_span, &handler, &report] {
        assert_eq!(
            span.span_context.trace_id().to_string(),
            CARRIED_TRACE,
            "`{}` continues the publisher's trace",
            span.name
        );
    }
    assert_eq!(
        handle_span.parent_span_id,
        consume.span_context.span_id(),
        "the handle's delivery is a child of the consume span"
    );
    assert_eq!(handler.parent_span_id, handle_span.span_context.span_id());
    assert_eq!(report.parent_span_id, handle_span.span_context.span_id());

    // Identifiers only: never a key, a value, a change_ref or a hash.
    assert!(gear_attributes(&handle_span).is_empty());
    let mut consumed: Vec<String> = gear_attributes(&consume)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    consumed.sort();
    assert_eq!(consumed, ["event_id", "tenant"]);
    drop(handle);
}

#[tokio::test]
async fn a_bundle_without_a_notification_is_handled_in_a_root_span_of_its_own() {
    let spans = SpanCapture::start();
    let t = traced();
    // A key not found once is registered on a retry, which owes the handler
    // its value: a bundle no notification queued.
    t.backend.register_fails("a", RegisterError::NotFound, 1);
    let recorder = Recorder::applying();
    let _handle = t.subscribe(&recorder).await;
    until("the owed bundle", || recorder.bundles().len() == 1).await;
    until("the handle span to close", || {
        !spans.named("change_notification.handle").is_empty()
    })
    .await;
    let handle_span = spans.one("change_notification.handle");
    assert_eq!(
        handle_span.parent_span_id,
        opentelemetry::trace::SpanId::INVALID,
        "no notification, no trace to continue"
    );
    assert!(spans.named("change_notification.consume").is_empty());
}
