//! The end-of-request publish hook over the broker client's view.
//!
//! Log-capture assertions assume one process per test (nextest, as the gear's
//! `make` targets run) and may flake under plain `cargo test`.

use std::sync::Arc;

use uuid::Uuid;

use crate::domain::activation::ActivationPairs;
use crate::domain::coherence::testing::{RecordingGateway, publisher};
use crate::domain::coherence::{BroadcastContext, ChangeOutcome, TouchedTenants};
use crate::domain::resolution::scope_class;
use crate::infra::broadcast::build_broadcasts;

fn write(key: &str, tenant: Uuid) -> ChangeOutcome {
    ChangeOutcome::value_write(key, tenant, scope_class::LOCAL, true)
}

#[tokio::test]
async fn types_not_registered_counts_one_skipped_per_built_event_and_publishes_nothing() {
    let mut gateway = RecordingGateway::ready(Uuid::new_v4());
    gateway.state.types_registered = false;
    let gateway = Arc::new(gateway);
    let (hook, metrics) = publisher(&gateway);
    hook.publish_value_writes(
        [
            write("a", Uuid::from_u128(1)),
            write("b", Uuid::from_u128(1)),
            write("a", Uuid::from_u128(2)),
        ],
        Uuid::new_v4(),
        &ActivationPairs::new(),
    )
    .await;
    assert_eq!(metrics.all(), ["skipped", "skipped"]);
    assert!(gateway.events().is_empty());
}

#[tokio::test]
async fn an_unknown_root_tenant_builds_nothing_and_counts_one_skipped_per_tenant_touched() {
    let mut gateway = RecordingGateway::ready(Uuid::new_v4());
    gateway.root = None;
    gateway.state.root_tenant_known = false;
    let gateway = Arc::new(gateway);
    let (hook, metrics) = publisher(&gateway);
    // Two tenants, one of them with a key set that would split: still two.
    let mut outcomes: Vec<ChangeOutcome> = (0..200)
        .map(|i| write(&format!("{i:04}{}", "x".repeat(1018)), Uuid::from_u128(1)))
        .collect();
    outcomes.push(write("b", Uuid::from_u128(2)));
    hook.publish_value_writes(outcomes, Uuid::new_v4(), &ActivationPairs::new())
        .await;
    assert_eq!(metrics.all(), ["skipped", "skipped"]);
    assert!(gateway.events().is_empty());
}

#[tokio::test]
async fn without_a_broker_a_key_set_over_the_limit_counts_one_skipped_per_part() {
    let root = Uuid::new_v4();
    let mut gateway = RecordingGateway::ready(root);
    gateway.broker = false;
    let gateway = Arc::new(gateway);
    let (hook, metrics) = publisher(&gateway);
    let keys: Vec<String> = (0..200)
        .map(|i| format!("{i:04}{}", "x".repeat(1018)))
        .collect();
    let touched = {
        let mut touched = TouchedTenants::new();
        touched.record_all(keys.iter().map(|k| write(k, Uuid::from_u128(1))));
        touched
    };
    let parts = build_broadcasts(
        &touched.resolve(root),
        &BroadcastContext {
            root_tenant: root,
            change_set_id: Uuid::nil(),
            trace_parent: None,
        },
    )
    .len();
    assert!(parts > 1, "the key set splits");
    hook.publish_request(&touched, &ActivationPairs::new(), Uuid::new_v4(), None)
        .await;
    assert_eq!(metrics.all(), vec!["skipped"; parts]);
    assert!(gateway.events().is_empty());
}

#[tokio::test]
async fn an_empty_request_publishes_nothing_and_counts_nothing() {
    let gateway = Arc::new(RecordingGateway::without_broker());
    let (hook, metrics) = publisher(&gateway);
    hook.publish_request(
        &TouchedTenants::new(),
        &ActivationPairs::new(),
        Uuid::new_v4(),
        None,
    )
    .await;
    hook.publish_declaration_changes(Vec::new(), Uuid::new_v4())
        .await;
    assert!(metrics.all().is_empty());
}

#[tokio::test]
async fn a_named_tenant_that_is_the_root_merges_with_a_root_change_in_one_broadcast() {
    let root = Uuid::new_v4();
    let gateway = Arc::new(RecordingGateway::ready(root));
    let (hook, metrics) = publisher(&gateway);
    let change_set = Uuid::new_v4();
    hook.publish_value_writes(
        [
            write("local", root),
            ChangeOutcome::value_write("wide", Uuid::new_v4(), scope_class::GLOBAL, true),
        ],
        change_set,
        &ActivationPairs::new(),
    )
    .await;
    let events = gateway.events();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].data().expect("body")["changed_keys"],
        serde_json::json!(["local", "wide"])
    );
    assert_eq!(events[0].tenant_id(), root);
    assert_eq!(metrics.all(), ["sent"]);
}

#[tokio::test]
async fn a_publish_carries_identifiers_only() {
    let gateway = Arc::new(RecordingGateway::ready(Uuid::new_v4()));
    let (hook, _) = publisher(&gateway);
    hook.publish_value_writes(
        [write("k", Uuid::from_u128(7))],
        Uuid::new_v4(),
        &ActivationPairs::new(),
    )
    .await;
    let body = gateway.events()[0].data().cloned().expect("body");
    let mut names: Vec<_> = body
        .as_object()
        .expect("an object")
        .keys()
        .cloned()
        .collect();
    names.sort();
    assert_eq!(names, ["change_set_id", "changed_keys", "tenant"]);
}

#[tokio::test]
async fn a_publish_inside_a_request_span_carries_that_span_as_trace_parent() {
    use opentelemetry::trace::TracerProvider as _;
    use tracing::Instrument as _;
    use tracing_subscriber::layer::SubscriberExt as _;

    let root = Uuid::new_v4();
    let gateway = Arc::new(RecordingGateway::ready(root));
    let (hook, _metrics) = publisher(&gateway);
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder().build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("publisher-test")));
    let _guard = tracing::subscriber::set_default(subscriber);
    let span = tracing::info_span!("request");
    let span_trace = span.in_scope(toolkit_trace_context::current_trace_id);
    hook.publish_value_writes(
        [write("a", Uuid::from_u128(1))],
        Uuid::new_v4(),
        &ActivationPairs::new(),
    )
    .instrument(span.clone())
    .await;
    hook.publish_declaration_changes(["k".to_owned()], Uuid::new_v4())
        .instrument(span)
        .await;
    let events = gateway.events();
    assert_eq!(events.len(), 2);
    for event in events {
        let carried = event.trace_parent().expect("trace_parent is carried");
        let parts: Vec<&str> = carried.split('-').collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(Some(parts[1].to_owned()), span_trace);
    }
}

#[tokio::test]
async fn a_publish_with_no_active_span_carries_no_trace_parent() {
    let root = Uuid::new_v4();
    let gateway = Arc::new(RecordingGateway::ready(root));
    let (hook, _metrics) = publisher(&gateway);
    hook.publish_declaration_changes(["k".to_owned()], Uuid::new_v4())
        .await;
    let events = gateway.events();
    assert_eq!(events.len(), 1);
    assert!(events[0].trace_parent().is_none());
}

#[derive(Clone, Default)]
struct LogBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuf {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn one_refused_publish_is_one_warn_and_the_skipped_rest_say_nothing_loud() {
    let mut gateway = RecordingGateway::ready(Uuid::new_v4());
    gateway.refuse = true;
    let gateway = Arc::new(gateway);
    let (hook, metrics) = publisher(&gateway);
    let buf = LogBuf::default();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(buf.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .finish(),
    );
    // Two tenants: the first publish is refused, the second is skipped.
    hook.publish_value_writes(
        [
            write("a", Uuid::from_u128(1)),
            write("b", Uuid::from_u128(2)),
        ],
        Uuid::new_v4(),
        &ActivationPairs::new(),
    )
    .await;
    assert_eq!(metrics.all(), ["failed", "skipped"]);
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert_eq!(log.matches("WARN").count(), 1, "{log}");
    assert!(
        log.contains("no produce grant"),
        "the one warn names the reason: {log}"
    );
}

#[tokio::test]
async fn a_request_that_sent_nothing_is_not_an_info_line() {
    let gateway = Arc::new(RecordingGateway::without_broker());
    let (hook, _metrics) = publisher(&gateway);
    let buf = LogBuf::default();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(buf.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .finish(),
    );
    hook.publish_value_writes(
        [write("a", Uuid::from_u128(1))],
        Uuid::new_v4(),
        &ActivationPairs::new(),
    )
    .await;
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(!log.contains("INFO") && !log.contains("WARN"), "{log}");
    assert!(log.contains("DEBUG"), "{log}");
}
