use opentelemetry::metrics::MeterProvider;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{
    InMemoryMetricExporter, Instrument, PeriodicReader, SdkMeterProvider, Stream,
};

use crate::domain::ports::OagwMetricsPort;
use crate::domain::ports::metric_labels::phase;

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

fn make_meter(provider: &SdkMeterProvider, prefix: &str) -> super::OagwMetricsMeter {
    super::OagwMetricsMeter::new(&provider.meter("oagw"), prefix)
}

// ── Extractors ──────────────────────────────────────────────────────────

fn counter_u64(exporter: &InMemoryMetricExporter, name: &str) -> u64 {
    let metrics = exporter.get_finished_metrics().unwrap();
    for rm in &metrics {
        for sm in rm.scope_metrics() {
            for m in sm.metrics() {
                if m.name() == name
                    && let AggregatedMetrics::U64(MetricData::Sum(sum)) = m.data()
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

fn up_down_i64(exporter: &InMemoryMetricExporter, name: &str) -> i64 {
    let metrics = exporter.get_finished_metrics().unwrap();
    for rm in &metrics {
        for sm in rm.scope_metrics() {
            for m in sm.metrics() {
                if m.name() == name
                    && let AggregatedMetrics::I64(MetricData::Sum(sum)) = m.data()
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

fn histogram_count(exporter: &InMemoryMetricExporter, name: &str) -> u64 {
    let metrics = exporter.get_finished_metrics().unwrap();
    for rm in &metrics {
        for sm in rm.scope_metrics() {
            for m in sm.metrics() {
                if m.name() == name
                    && let AggregatedMetrics::F64(MetricData::Histogram(h)) = m.data()
                {
                    return h.data_points().map(|dp| dp.count()).sum();
                }
            }
        }
    }
    0
}

fn gauge_last_f64(exporter: &InMemoryMetricExporter, name: &str) -> Option<f64> {
    let metrics = exporter.get_finished_metrics().unwrap();
    for rm in &metrics {
        for sm in rm.scope_metrics() {
            for m in sm.metrics() {
                if m.name() == name
                    && let AggregatedMetrics::F64(MetricData::Gauge(g)) = m.data()
                {
                    return g.data_points().last().map(|dp| dp.value());
                }
            }
        }
    }
    None
}

// ── Counter tests ───────────────────────────────────────────────────────

#[test]
fn requests_counter_increments() {
    let (provider, exporter) = local_provider();
    let m = make_meter(&provider, "oagw");

    m.record_request("openai", "/v1/chat", "POST", 200);
    m.record_request("openai", "/v1/chat", "POST", 200);
    m.record_request("openai", "/v1/chat", "POST", 503);

    provider.force_flush().unwrap();

    assert_eq!(counter_u64(&exporter, "oagw_requests"), 3);
}

#[test]
fn errors_counter_increments() {
    let (provider, exporter) = local_provider();
    let m = make_meter(&provider, "oagw");

    m.record_error("openai", "/v1/chat", "DownstreamError");
    m.record_error("anthropic", "/v1/messages", "RequestTimeout");

    provider.force_flush().unwrap();

    assert_eq!(counter_u64(&exporter, "oagw_errors"), 2);
}

#[test]
fn rate_limit_exceeded_counter_increments() {
    let (provider, exporter) = local_provider();
    let m = make_meter(&provider, "oagw");

    m.record_rate_limit_exceeded("openai", "/v1/chat");
    m.record_rate_limit_exceeded("openai", "/v1/chat");

    provider.force_flush().unwrap();

    assert_eq!(counter_u64(&exporter, "oagw_rate_limit_exceeded"), 2);
}

// ── Histogram ───────────────────────────────────────────────────────────

#[test]
fn request_duration_histogram_records() {
    let (provider, exporter) = local_provider();
    let m = make_meter(&provider, "oagw");

    m.record_request_duration_seconds("openai", "/v1/chat", phase::TOTAL, 0.042);
    m.record_request_duration_seconds("openai", "/v1/chat", phase::TOTAL, 0.150);
    m.record_request_duration_seconds("openai", "/v1/chat", phase::TOTAL, 1.5);

    provider.force_flush().unwrap();

    assert_eq!(
        histogram_count(&exporter, "oagw_request_duration_seconds"),
        3
    );
}

// ── UpDownCounter (in-flight gauge) ─────────────────────────────────────

#[test]
fn in_flight_gauge_balances() {
    let (provider, exporter) = local_provider();
    let m = make_meter(&provider, "oagw");

    m.increment_in_flight("openai");
    m.increment_in_flight("openai");
    m.increment_in_flight("openai");
    m.decrement_in_flight("openai");

    provider.force_flush().unwrap();

    assert_eq!(up_down_i64(&exporter, "oagw_requests_in_flight"), 2);
}

#[test]
fn in_flight_returns_to_zero_after_balanced_calls() {
    let (provider, exporter) = local_provider();
    let m = make_meter(&provider, "oagw");

    for _ in 0..5 {
        m.increment_in_flight("anthropic");
    }
    for _ in 0..5 {
        m.decrement_in_flight("anthropic");
    }

    provider.force_flush().unwrap();

    assert_eq!(up_down_i64(&exporter, "oagw_requests_in_flight"), 0);
}

// ── Gauge ───────────────────────────────────────────────────────────────

#[test]
fn rate_limit_usage_ratio_records_last_value() {
    let (provider, exporter) = local_provider();
    let m = make_meter(&provider, "oagw");

    m.record_rate_limit_usage_ratio("openai", "/v1/chat", 0.25);
    m.record_rate_limit_usage_ratio("openai", "/v1/chat", 0.75);

    provider.force_flush().unwrap();

    let v = gauge_last_f64(&exporter, "oagw_rate_limit_usage_ratio")
        .expect("gauge should have at least one data point");
    assert!((v - 0.75).abs() < f64::EPSILON, "expected 0.75, got {v}");
}

#[test]
fn rate_limit_usage_ratio_clamps_values_above_one() {
    let (provider, exporter) = local_provider();
    let m = make_meter(&provider, "oagw");

    m.record_rate_limit_usage_ratio("openai", "/v1/chat", 2.5);
    provider.force_flush().unwrap();

    let v = gauge_last_f64(&exporter, "oagw_rate_limit_usage_ratio")
        .expect("gauge should have at least one data point");
    assert!(
        (v - 1.0).abs() < f64::EPSILON,
        "expected clamp to 1.0, got {v}"
    );
}

#[test]
fn rate_limit_usage_ratio_clamps_negative_values() {
    let (provider, exporter) = local_provider();
    let m = make_meter(&provider, "oagw");

    m.record_rate_limit_usage_ratio("openai", "/v1/chat", -0.5);
    provider.force_flush().unwrap();

    let v = gauge_last_f64(&exporter, "oagw_rate_limit_usage_ratio")
        .expect("gauge should have at least one data point");
    assert!(
        (v - 0.0).abs() < f64::EPSILON,
        "expected clamp to 0.0, got {v}"
    );
}

// ── Prefix configurability ──────────────────────────────────────────────

#[test]
fn configurable_prefix_changes_all_instrument_names() {
    let (provider, exporter) = local_provider();
    let m = make_meter(&provider, "my_oagw");

    m.record_request("openai", "/v1/chat", "GET", 200);

    provider.force_flush().unwrap();

    assert_eq!(counter_u64(&exporter, "my_oagw_requests"), 1);
    // Default name must NOT exist when a custom prefix is used.
    assert_eq!(counter_u64(&exporter, "oagw_requests"), 0);
}

// ── Auth-phase timing ───────────────────────────────────────────────────

#[test]
fn auth_phase_recorded_into_request_duration_histogram() {
    let (provider, exporter) = local_provider();
    let m = make_meter(&provider, "oagw");

    m.record_request_duration_seconds("openai", "/v1/chat", phase::AUTH, 0.012);
    m.record_request_duration_seconds("openai", "/v1/chat", phase::AUTH, 0.024);
    m.record_request_duration_seconds("openai", "/v1/chat", phase::TOTAL, 0.300);

    provider.force_flush().unwrap();

    // Both phases share the same histogram instrument — count is 3 total,
    // dashboards split by the `phase` label.
    assert_eq!(
        histogram_count(&exporter, "oagw_request_duration_seconds"),
        3
    );
}

// ── WebSocket session metrics ───────────────────────────────────────────

#[test]
fn active_websocket_sessions_gauge_balances() {
    let (provider, exporter) = local_provider();
    let m = make_meter(&provider, "oagw");

    m.increment_active_websocket_sessions("openai");
    m.increment_active_websocket_sessions("openai");
    m.increment_active_websocket_sessions("anthropic");
    m.decrement_active_websocket_sessions("openai");

    provider.force_flush().unwrap();

    // 3 inc - 1 dec = net 2 sessions across both hosts
    assert_eq!(up_down_i64(&exporter, "oagw_active_websocket_sessions"), 2);
}

#[test]
fn websocket_session_duration_histogram_records() {
    let (provider, exporter) = local_provider();
    let m = make_meter(&provider, "oagw");

    m.record_websocket_session_duration_seconds("openai", 2.5);
    m.record_websocket_session_duration_seconds("openai", 120.0);
    m.record_websocket_session_duration_seconds("openai", 3600.0);

    provider.force_flush().unwrap();

    assert_eq!(
        histogram_count(&exporter, "oagw_websocket_session_duration_seconds"),
        3
    );
}

// ── OTel semconv label shape ────────────────────────────────────────────

/// Collect the attributes on the single (and only) data point for a
/// `u64` counter named `name`. Panics if the metric is missing or has more
/// than one data point.
fn single_counter_attrs(
    exporter: &InMemoryMetricExporter,
    name: &str,
) -> Vec<(String, opentelemetry::Value)> {
    let metrics = exporter.get_finished_metrics().unwrap();
    for rm in &metrics {
        for sm in rm.scope_metrics() {
            for instr in sm.metrics() {
                if instr.name() != name {
                    continue;
                }
                let AggregatedMetrics::U64(MetricData::Sum(sum)) = instr.data() else {
                    continue;
                };
                let mut dps: Vec<_> = sum.data_points().collect();
                assert_eq!(
                    dps.len(),
                    1,
                    "expected exactly one data point on {name}, got {}",
                    dps.len()
                );
                return dps
                    .remove(0)
                    .attributes()
                    .map(|kv| (kv.key.as_str().to_owned(), kv.value.clone()))
                    .collect();
            }
        }
    }
    panic!("metric {name} not found in exporter output");
}

#[test]
fn requests_counter_uses_otel_semconv_label_keys() {
    use opentelemetry::Value;

    let (provider, exporter) = local_provider();
    let m = make_meter(&provider, "oagw");

    m.record_request("openai", "/v1/chat", "POST", 204);

    provider.force_flush().unwrap();

    let attrs = single_counter_attrs(&exporter, "oagw_requests");
    let lookup = |key: &str| attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());

    assert!(
        matches!(lookup("http.request.method"), Some(Value::String(s)) if s.as_str() == "POST"),
        "expected http.request.method=\"POST\" (OTel semconv), got {:?}",
        lookup("http.request.method"),
    );
    assert!(
        matches!(lookup("http.route"), Some(Value::String(s)) if s.as_str() == "/v1/chat"),
        "expected http.route=\"/v1/chat\" (OTel semconv), got {:?}",
        lookup("http.route"),
    );
    assert!(
        matches!(lookup("http.response.status_code"), Some(Value::I64(204))),
        "expected http.response.status_code=I64(204), got {:?}",
        lookup("http.response.status_code"),
    );
    // Legacy short keys must not leak back into the surface.
    assert!(
        lookup("method").is_none() && lookup("path").is_none(),
        "legacy short label keys should not appear on the requests counter"
    );
}

#[test]
fn normalize_method_collapses_unknown_verbs_to_other() {
    use http::Method;

    // Standard verbs pass through verbatim.
    assert_eq!(super::normalize_method(&Method::GET), "GET");
    assert_eq!(super::normalize_method(&Method::POST), "POST");
    assert_eq!(super::normalize_method(&Method::PATCH), "PATCH");

    // Anything outside the standard set maps to the OTel `_OTHER` sentinel
    // (see api-gateway's normalizer for the matching vocabulary).
    let propfind = Method::from_bytes(b"PROPFIND").unwrap();
    assert_eq!(super::normalize_method(&propfind), "_OTHER");
    let mkcol = Method::from_bytes(b"MKCOL").unwrap();
    assert_eq!(super::normalize_method(&mkcol), "_OTHER");
}
