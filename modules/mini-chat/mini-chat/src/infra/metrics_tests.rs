use opentelemetry::metrics::MeterProvider;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{
    InMemoryMetricExporter, Instrument, PeriodicReader, SdkMeterProvider, Stream,
};

use crate::domain::ports::MiniChatMetricsPort;

const CARDINALITY_LIMIT: usize = 2000;

fn local_provider() -> (SdkMeterProvider, InMemoryMetricExporter) {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .with_view(|_: &Instrument| {
            Stream::builder()
                .with_cardinality_limit(CARDINALITY_LIMIT)
                .build()
                .ok()
        })
        .build();
    (provider, exporter)
}

fn extract_counter_value(exporter: &InMemoryMetricExporter, name: &str) -> u64 {
    let metrics = exporter.get_finished_metrics().unwrap();
    for resource_metrics in &metrics {
        for scope_metrics in resource_metrics.scope_metrics() {
            for metric in scope_metrics.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data()
                {
                    return sum
                        .data_points()
                        .map(opentelemetry_sdk::metrics::data::SumDataPoint::value)
                        .sum();
                }
            }
        }
    }
    0
}

#[test]
fn stream_counters_increment() {
    let (provider, exporter) = local_provider();
    let m = super::MiniChatMetricsMeter::new(&provider.meter("mini-chat"), "mini_chat");

    m.record_stream_started("openai", "gpt-5.2");
    m.record_stream_started("openai", "gpt-5.2");
    m.record_stream_completed("openai", "gpt-5.2");
    m.record_stream_failed("openai", "gpt-5.2", "rate_limited");

    provider.force_flush().unwrap();

    assert_eq!(
        extract_counter_value(&exporter, "mini_chat_stream_started"),
        2
    );
    assert_eq!(
        extract_counter_value(&exporter, "mini_chat_stream_completed"),
        1
    );
    assert_eq!(
        extract_counter_value(&exporter, "mini_chat_stream_failed"),
        1
    );
}

#[test]
fn turn_mutation_counter_increments() {
    let (provider, exporter) = local_provider();
    let m = super::MiniChatMetricsMeter::new(&provider.meter("mini-chat"), "mini_chat");

    m.record_turn_mutation("retry", "ok");
    m.record_turn_mutation("edit", "not_latest");
    m.record_turn_mutation("delete", "ok");

    provider.force_flush().unwrap();

    assert_eq!(
        extract_counter_value(&exporter, "mini_chat_turn_mutation"),
        3
    );
}

#[test]
fn quota_counters_increment() {
    let (provider, exporter) = local_provider();
    let m = super::MiniChatMetricsMeter::new(&provider.meter("mini-chat"), "mini_chat");

    m.record_quota_preflight("allow", "gpt-5.2", "premium");
    m.record_quota_reserve("daily");
    m.record_quota_commit("daily");
    m.record_quota_overshoot("monthly");

    provider.force_flush().unwrap();

    assert_eq!(
        extract_counter_value(&exporter, "mini_chat_quota_preflight"),
        1
    );
    assert_eq!(
        extract_counter_value(&exporter, "mini_chat_quota_reserve"),
        1
    );
    assert_eq!(
        extract_counter_value(&exporter, "mini_chat_quota_commit"),
        1
    );
    assert_eq!(
        extract_counter_value(&exporter, "mini_chat_quota_overshoot"),
        1
    );
}

#[test]
fn code_interpreter_counter_increments() {
    let (provider, exporter) = local_provider();
    let m = super::MiniChatMetricsMeter::new(&provider.meter("mini-chat"), "mini_chat");

    m.record_code_interpreter_calls("gpt-5.2", 3);

    provider.force_flush().unwrap();

    assert_eq!(
        extract_counter_value(&exporter, "mini_chat_code_interpreter_calls"),
        3
    );
}

#[test]
fn configurable_prefix() {
    let (provider, exporter) = local_provider();
    let m = super::MiniChatMetricsMeter::new(&provider.meter("custom"), "my_chat");

    m.record_stream_started("azure", "gpt-4o");

    provider.force_flush().unwrap();

    assert_eq!(
        extract_counter_value(&exporter, "my_chat_stream_started"),
        1
    );
    // Original prefix should NOT exist
    assert_eq!(
        extract_counter_value(&exporter, "mini_chat_stream_started"),
        0
    );
}

#[test]
fn cancellation_counters_increment() {
    let (provider, exporter) = local_provider();
    let m = super::MiniChatMetricsMeter::new(&provider.meter("mini-chat"), "mini_chat");

    m.record_cancel_requested("user_stop");
    m.record_cancel_effective("user_stop");
    m.record_streams_aborted("client_disconnect");

    provider.force_flush().unwrap();

    assert_eq!(
        extract_counter_value(&exporter, "mini_chat_cancel_requested"),
        1
    );
    assert_eq!(
        extract_counter_value(&exporter, "mini_chat_cancel_effective"),
        1
    );
    assert_eq!(
        extract_counter_value(&exporter, "mini_chat_streams_aborted"),
        1
    );
}
