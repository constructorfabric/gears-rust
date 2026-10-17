#![allow(clippy::expect_used, clippy::unwrap_used)]

use chrono::Utc;
use opentelemetry::trace::{TraceContextExt as _, TracerProvider as _};
use std::sync::{Arc, Mutex};

use opentelemetry_sdk::trace::{SdkTracerProvider, SpanData, SpanProcessor};
use toolkit_gts::gts_id;
use tracing_subscriber::layer::SubscriberExt as _;
use uuid::Uuid;

use super::trace::{batch_span, event_span};
use super::types::{EventBatch, RawEvent};
use crate::sequence::Sequence;

const TRACE_A: &str = "0af7651916cd43dd8448eb211c80319c";
const TRACE_B: &str = "4bf92f3577b34da6a3ce929d0e0e4736";

fn traceparent(trace_id: &str) -> String {
    format!("00-{trace_id}-b7ad6b7169203331-01")
}

fn raw_event(offset: i64, trace_parent: Option<String>) -> RawEvent {
    RawEvent {
        id: Uuid::new_v4(),
        type_id: gts::GtsTypeId::new(gts_id!(
            "cf.core.events.event.v1~example.orders.order_created.x.v1~"
        )),
        topic: gts::GtsInstanceId::try_new(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        ))
        .unwrap(),
        tenant_id: Uuid::nil(),
        subject: format!("order-{offset}"),
        subject_type: gts::GtsTypeId::new("gts.x.eb.test.subject.v1~"),
        partition: 7,
        sequence: Sequence::assigned(offset),
        offset: Sequence::assigned(offset),
        occurred_at: Utc::now(),
        sequence_time: Utc::now(),
        trace_parent,
        data: serde_json::json!({ "offset": offset }),
    }
}

/// Records every span that ends, synchronously, so a test can inspect it.
#[derive(Debug, Clone, Default)]
struct Recorder(Arc<Mutex<Vec<SpanData>>>);

impl SpanProcessor for Recorder {
    fn on_start(&self, _span: &mut opentelemetry_sdk::trace::Span, _cx: &opentelemetry::Context) {}

    fn on_end(&self, span: SpanData) {
        self.0.lock().unwrap().push(span);
    }

    fn force_flush(&self) -> opentelemetry_sdk::error::OTelSdkResult {
        Ok(())
    }

    fn shutdown_with_timeout(
        &self,
        _timeout: std::time::Duration,
    ) -> opentelemetry_sdk::error::OTelSdkResult {
        Ok(())
    }
}

/// Run `f` under a live OTel pipeline; returns its result and the spans that ended.
fn with_otel<R>(f: impl FnOnce() -> R) -> (R, Vec<SpanData>) {
    opentelemetry::global::set_text_map_propagator(
        opentelemetry_sdk::propagation::TraceContextPropagator::new(),
    );
    let recorder = Recorder::default();
    let provider = SdkTracerProvider::builder()
        .with_span_processor(recorder.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("consumer-trace-test")));
    let result = tracing::subscriber::with_default(subscriber, f);
    let spans = recorder.0.lock().unwrap().clone();
    (result, spans)
}

fn current_trace_id() -> String {
    opentelemetry::Context::current()
        .span()
        .span_context()
        .trace_id()
        .to_string()
}

/// A handler span continues the trace the event was produced under.
#[test]
fn event_span_continues_the_producer_trace() {
    let (continued, _) = with_otel(|| {
        let event = raw_event(1, Some(traceparent(TRACE_A)));
        event_span(&event).in_scope(current_trace_id)
    });
    assert_eq!(continued, TRACE_A);
}

/// Without a usable `traceparent` the handler span is not tied to any
/// producer trace (and an invalid value is ignored, not propagated).
#[test]
fn event_span_without_a_valid_traceparent_starts_its_own_trace() {
    let ((none, invalid), _) = with_otel(|| {
        let none = event_span(&raw_event(1, None)).in_scope(current_trace_id);
        let invalid = event_span(&raw_event(2, Some("not-a-traceparent".to_owned())))
            .in_scope(current_trace_id);
        (none, invalid)
    });
    for own in [none, invalid] {
        assert_ne!(own, TRACE_A);
        assert_ne!(own, TRACE_B);
    }
}

/// A batch span links each distinct producer trace once, and claims none of
/// them as its parent.
#[test]
fn batch_span_links_every_distinct_producer_trace() {
    let (own_trace, spans) = with_otel(|| {
        let events = vec![
            raw_event(1, Some(traceparent(TRACE_A))),
            raw_event(2, Some(traceparent(TRACE_B))),
            raw_event(3, Some(traceparent(TRACE_A))),
            raw_event(4, None),
        ];
        let span = batch_span(&EventBatch::new(&events));
        let own = span.in_scope(current_trace_id);
        drop(span);
        own
    });
    assert_ne!(own_trace, TRACE_A);
    assert_ne!(own_trace, TRACE_B);

    let batch = spans
        .iter()
        .find(|span| span.name == "event_broker.consume_batch")
        .expect("the batch span was exported");
    let mut linked: Vec<String> = batch
        .links
        .iter()
        .map(|link| link.span_context.trace_id().to_string())
        .collect();
    linked.sort();
    assert_eq!(linked, vec![TRACE_A.to_owned(), TRACE_B.to_owned()]);
}
