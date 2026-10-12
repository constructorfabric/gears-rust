//! Tower layer that records OpenTelemetry metrics for outbound HTTP requests.
//!
//! Emits a single instrument following [OpenTelemetry HTTP client semantic
//! conventions][semconv]:
//! - `http.client.request.duration` — histogram (seconds)
//!
//! Attributes: `http.request.method`, `http.route`, `server.address`,
//! `server.port` (when the URI carries an explicit port), and
//! `http.response.status_code` (on success) or `error.type` (on failure).
//!
//! Modeled after go-appkit's `MetricsRoundTripper`: one duration histogram plus
//! a build-time request classifier that produces the bounded `http.route`
//! label, preventing cardinality explosion from raw paths. Like the Go version,
//! this layer sits outside the retry loop, so it observes one logical request
//! regardless of transport-level retries.
//!
//! [semconv]: https://opentelemetry.io/docs/specs/semconv/http/http-metrics/

use crate::error::HttpError;
use crate::request::RequestType;
use crate::response::ResponseBody;
use bytes::Bytes;
use http::{Request, Response};
use http_body_util::Full;
use opentelemetry::metrics::{Histogram, Meter};
use opentelemetry::{KeyValue, global};
use std::borrow::Cow;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;
use tower::{Layer, Service};

/// Classifies a request into a low-cardinality route label (the `http.route`
/// attribute). Set once when the client is built; invoked on every request.
///
/// This is the Rust analogue of go-appkit's `ClassifyRequest` callback. It must
/// return a *bounded* set of values (e.g. route templates like
/// `GET /users/{id}`), never a raw path containing identifiers, otherwise the
/// metric cardinality is unbounded.
pub type ClassifyFn = Arc<dyn Fn(&Request<Full<Bytes>>) -> Cow<'static, str> + Send + Sync>;

/// Explicit histogram bucket boundaries (seconds) for request duration.
///
/// The SDK's default boundaries are count-oriented (hundreds–thousands) and
/// useless for a seconds-valued duration. These mirror go-appkit's buckets with
/// finer low-end resolution, so client-side percentiles stay meaningful and
/// comparable across the two implementations.
const DURATION_BOUNDARIES_SECS: &[f64] = &[
    0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 150.0, 300.0, 600.0,
];

/// Default classifier producing `"METHOD host"` (mirrors go-appkit's default
/// `summary`). Never returns a raw path, so it cannot blow up cardinality.
#[must_use]
pub fn default_classify(req: &Request<Full<Bytes>>) -> Cow<'static, str> {
    let host = req.uri().host().unwrap_or("unknown");
    // Use the normalized method (`_OTHER` for unknown verbs) so the route label
    // stays consistent with the `http.request.method` attribute and cannot be
    // widened by arbitrary method strings.
    Cow::Owned(format!("{} {}", normalize_method(req.method()), host))
}

/// Normalize HTTP method per [OTel semantic conventions][semconv].
///
/// Unknown methods map to `_OTHER` to bound attribute cardinality. Mirrors the
/// server-side helper in `api-gateway`'s `http_metrics` middleware (duplicated
/// here so `toolkit-http` stays free of a dependency on that gear).
///
/// [semconv]: https://opentelemetry.io/docs/specs/semconv/http/http-metrics/
fn normalize_method(method: &http::Method) -> &'static str {
    match *method {
        http::Method::GET => "GET",
        http::Method::POST => "POST",
        http::Method::PUT => "PUT",
        http::Method::DELETE => "DELETE",
        http::Method::PATCH => "PATCH",
        http::Method::HEAD => "HEAD",
        http::Method::OPTIONS => "OPTIONS",
        http::Method::CONNECT => "CONNECT",
        http::Method::TRACE => "TRACE",
        _ => "_OTHER",
    }
}

/// Low-cardinality `error.type` value for a transport-level failure.
///
/// This layer sits outside the load-shed/retry layers (and inside the buffer),
/// and the inner service returns `Ok(Response)` for all HTTP statuses (including
/// 4xx/5xx). Only transport-class failures reach the `Err` arm here — the
/// `OTel` analogue of go-appkit's `status="0"`. Because the layer is outside the
/// concurrency limiter, a load-shed rejection reaches this arm too and is
/// recorded as `"overloaded"`. Everything else collapses to `"other"` rather
/// than enumerating variants that cannot occur at this point.
fn error_type(err: &HttpError) -> &'static str {
    match err {
        HttpError::Timeout(_) => "timeout",
        HttpError::DeadlineExceeded(_) => "deadline_exceeded",
        HttpError::Transport(_) => "transport",
        HttpError::Tls(_) => "tls",
        HttpError::Overloaded => "overloaded",
        _ => "other",
    }
}

/// Tower layer recording HTTP client request-duration metrics.
#[derive(Clone)]
pub struct MetricsLayer {
    duration: Histogram<f64>,
    classify: ClassifyFn,
}

impl MetricsLayer {
    /// Create a metrics layer.
    ///
    /// `client_type` names the OpenTelemetry instrumentation scope (the meter),
    /// mirroring go-appkit's `ClientType` and the server-side `gear_name`.
    /// `classify` produces the bounded `http.route` attribute for each request.
    #[must_use]
    pub fn new(client_type: &str, classify: ClassifyFn) -> Self {
        let scope = opentelemetry::InstrumentationScope::builder(client_type.to_owned()).build();
        let meter = global::meter_with_scope(scope);
        Self::with_meter(&meter, classify)
    }

    /// Create a metrics layer using a caller-provided [`Meter`].
    ///
    /// Use this to bind the instrument to a specific `MeterProvider` instead of
    /// the global one (e.g. for tests or multi-provider setups). The instrument
    /// name, unit, bucket boundaries, and behavior are identical to [`new`](Self::new).
    #[must_use]
    pub fn with_meter(meter: &Meter, classify: ClassifyFn) -> Self {
        let duration = meter
            .f64_histogram("http.client.request.duration")
            .with_description("Duration of outbound HTTP client requests")
            .with_unit("s")
            .with_boundaries(DURATION_BOUNDARIES_SECS.to_vec())
            .build();
        Self { duration, classify }
    }
}

impl<S> Layer<S> for MetricsLayer {
    type Service = MetricsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        MetricsService {
            inner,
            duration: self.duration.clone(),
            classify: self.classify.clone(),
        }
    }
}

/// Service that records a duration metric for each outbound request.
#[derive(Clone)]
pub struct MetricsService<S> {
    inner: S,
    duration: Histogram<f64>,
    classify: ClassifyFn,
}

impl<S> Service<Request<Full<Bytes>>> for MetricsService<S>
where
    S: Service<Request<Full<Bytes>>, Response = Response<ResponseBody>, Error = HttpError>
        + Clone
        + Send
        + 'static,
    S::Future: Send,
{
    type Response = Response<ResponseBody>;
    type Error = HttpError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Full<Bytes>>) -> Self::Future {
        // Compute attributes from the request up front; the request itself is
        // moved into the inner service.
        let route = (self.classify)(&req).into_owned();
        let method = normalize_method(req.method());
        let server_address = req.uri().host().unwrap_or("unknown").to_owned();
        let server_port = req.uri().port_u16();
        // Read request_type set by RequestBuilder::with_request_type — mirrors
        // go-appkit's GetRequestTypeFromContext.
        let request_type = req
            .extensions()
            .get::<RequestType>()
            .map(|rt| rt.0.clone().into_owned());
        let duration = self.duration.clone();

        // Swap so we call the instance that was poll_ready'd, leaving a fresh
        // clone for the next poll_ready cycle (Tower Service contract).
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);

        Box::pin(async move {
            let start = Instant::now();
            let result = inner.call(req).await;
            let elapsed = start.elapsed().as_secs_f64();

            let mut attrs = vec![
                KeyValue::new("http.request.method", method),
                KeyValue::new("http.route", route),
                KeyValue::new("server.address", server_address),
            ];
            // OTel client semconv pairs server.address with server.port; only
            // explicit ports are present in the URI (default 80/443 are elided).
            if let Some(port) = server_port {
                attrs.push(KeyValue::new("server.port", i64::from(port)));
            }
            if let Some(rt) = request_type {
                attrs.push(KeyValue::new("request_type", rt));
            }
            match &result {
                Ok(response) => attrs.push(KeyValue::new(
                    "http.response.status_code",
                    i64::from(response.status().as_u16()),
                )),
                Err(e) => attrs.push(KeyValue::new("error.type", error_type(e))),
            }
            duration.record(elapsed, &attrs);

            result
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::request::RequestType;
    use http::StatusCode;
    use http_body_util::{BodyExt, Empty};
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, HistogramDataPoint, MetricData};
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, SdkMeterProvider};
    use std::convert::Infallible;
    use tower::{ServiceBuilder, ServiceExt, service_fn};

    fn empty_response(status: StatusCode) -> Response<ResponseBody> {
        let body: ResponseBody = Empty::<Bytes>::new()
            .map_err(|e: Infallible| -> Box<dyn std::error::Error + Send + Sync> { match e {} })
            .boxed();
        Response::builder().status(status).body(body).unwrap()
    }

    /// Collect the histogram data point for `http.client.request.duration` whose
    /// attributes contain every `(key, value)` in `expected`. Returns `None` if
    /// no matching point was exported.
    fn find_duration_point(
        exporter: &InMemoryMetricExporter,
        expected: &[(&str, &str)],
    ) -> Option<HistogramDataPoint<f64>> {
        let batches = exporter.get_finished_metrics().unwrap();
        for rm in &batches {
            for sm in rm.scope_metrics() {
                for metric in sm.metrics() {
                    if metric.name() != "http.client.request.duration" {
                        continue;
                    }
                    let AggregatedMetrics::F64(MetricData::Histogram(hist)) = metric.data() else {
                        continue;
                    };
                    for dp in hist.data_points() {
                        let matches = expected.iter().all(|(k, v)| {
                            dp.attributes()
                                .any(|kv| kv.key.as_str() == *k && kv.value.to_string() == *v)
                        });
                        if matches {
                            return Some(dp.clone());
                        }
                    }
                }
            }
        }
        None
    }

    fn test_provider() -> (SdkMeterProvider, InMemoryMetricExporter) {
        let exporter = InMemoryMetricExporter::default();
        let provider = SdkMeterProvider::builder()
            .with_periodic_exporter(exporter.clone())
            .build();
        (provider, exporter)
    }

    #[tokio::test]
    async fn records_duration_with_attributes_on_success() {
        let (provider, exporter) = test_provider();
        let meter = provider.meter("test-client");
        let classify: ClassifyFn = Arc::new(|_req| Cow::Borrowed("GET /users/{id}"));
        let layer = MetricsLayer::with_meter(&meter, classify);

        let inner = service_fn(|_req: Request<Full<Bytes>>| async {
            Ok::<_, HttpError>(empty_response(StatusCode::OK))
        });
        let mut svc = ServiceBuilder::new().layer(layer).service(inner);
        let req = Request::builder()
            .method(http::Method::GET)
            .uri("https://example.com:8443/users/123")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = svc.ready().await.unwrap().call(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        provider.force_flush().unwrap();
        let point = find_duration_point(
            &exporter,
            &[
                ("http.request.method", "GET"),
                ("http.route", "GET /users/{id}"),
                ("server.address", "example.com"),
                ("server.port", "8443"),
                ("http.response.status_code", "200"),
            ],
        )
        .expect("a duration data point with the expected attributes should be exported");
        assert_eq!(point.count(), 1, "exactly one observation recorded");
    }

    #[tokio::test]
    async fn records_error_type_on_transport_failure() {
        let (provider, exporter) = test_provider();
        let meter = provider.meter("test-client");
        let layer = MetricsLayer::with_meter(&meter, Arc::new(default_classify));

        let inner = service_fn(|_req: Request<Full<Bytes>>| async {
            Err::<Response<ResponseBody>, _>(HttpError::Timeout(std::time::Duration::from_secs(1)))
        });
        let mut svc = ServiceBuilder::new().layer(layer).service(inner);
        let req = Request::builder()
            .method(http::Method::GET)
            .uri("https://example.com/")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let err = svc.ready().await.unwrap().call(req).await.unwrap_err();
        assert!(matches!(err, HttpError::Timeout(_)));

        provider.force_flush().unwrap();
        let point = find_duration_point(&exporter, &[("error.type", "timeout")])
            .expect("a duration data point tagged error.type=timeout should be exported");
        assert_eq!(point.count(), 1);
        // Failures must not carry a status code.
        assert!(
            point
                .attributes()
                .all(|kv| kv.key.as_str() != "http.response.status_code"),
            "transport failures must not record http.response.status_code"
        );
    }

    #[test]
    fn default_classify_normalizes_method_and_drops_path() {
        let req = Request::builder()
            .method(http::Method::POST)
            .uri("https://api.example.com/users/abc-123-uuid")
            .body(Full::new(Bytes::new()))
            .unwrap();
        // Raw path with an identifier must never leak into the label.
        assert_eq!(default_classify(&req), "POST api.example.com");

        let exotic = Request::builder()
            .method(http::Method::from_bytes(b"PROPFIND").unwrap())
            .uri("https://api.example.com/dav")
            .body(Full::new(Bytes::new()))
            .unwrap();
        assert_eq!(default_classify(&exotic), "_OTHER api.example.com");
    }

    #[test]
    fn normalize_method_caps_unknown() {
        assert_eq!(normalize_method(&http::Method::GET), "GET");
        let custom = http::Method::from_bytes(b"PROPFIND").unwrap();
        assert_eq!(normalize_method(&custom), "_OTHER");
    }

    #[tokio::test]
    async fn records_request_type_attribute_when_set() {
        let (provider, exporter) = test_provider();
        let meter = provider.meter("test-client");
        let layer = MetricsLayer::with_meter(&meter, Arc::new(default_classify));

        let inner = service_fn(|_req: Request<Full<Bytes>>| async {
            Ok::<_, HttpError>(empty_response(StatusCode::OK))
        });
        let mut svc = ServiceBuilder::new().layer(layer).service(inner);

        let mut req = Request::builder()
            .method(http::Method::GET)
            .uri("https://example.com/tenants/123")
            .body(Full::new(Bytes::new()))
            .unwrap();
        req.extensions_mut()
            .insert(RequestType::new("tenants_resolve"));

        svc.ready().await.unwrap().call(req).await.unwrap();

        provider.force_flush().unwrap();
        let point = find_duration_point(&exporter, &[("request_type", "tenants_resolve")])
            .expect("request_type attribute should appear in exported metric");
        assert_eq!(point.count(), 1);
    }

    #[tokio::test]
    async fn omits_request_type_when_not_set() {
        let (provider, exporter) = test_provider();
        let meter = provider.meter("test-client");
        let layer = MetricsLayer::with_meter(&meter, Arc::new(default_classify));

        let inner = service_fn(|_req: Request<Full<Bytes>>| async {
            Ok::<_, HttpError>(empty_response(StatusCode::OK))
        });
        let mut svc = ServiceBuilder::new().layer(layer).service(inner);

        let req = Request::builder()
            .method(http::Method::GET)
            .uri("https://example.com/tenants/123")
            .body(Full::new(Bytes::new()))
            .unwrap();

        svc.ready().await.unwrap().call(req).await.unwrap();

        provider.force_flush().unwrap();
        let dp = find_duration_point(&exporter, &[("http.request.method", "GET")])
            .expect("a data point should be exported");
        assert!(
            dp.attributes().all(|kv| kv.key.as_str() != "request_type"),
            "request_type must not appear when not set"
        );
    }

    #[test]
    fn error_type_maps_transport_class_failures() {
        assert_eq!(
            error_type(&HttpError::Timeout(std::time::Duration::from_secs(1))),
            "timeout"
        );
        assert_eq!(
            error_type(&HttpError::Transport("boom".into())),
            "transport"
        );
        assert_eq!(error_type(&HttpError::Overloaded), "overloaded");
    }

    /// A load-shed rejection (`HttpError::Overloaded`) reaching this layer — as
    /// it does in `build()`, where the concurrency limiter is inner to the
    /// metrics layer — is recorded as `error.type = "overloaded"`, so shed
    /// requests are counted rather than invisible.
    #[tokio::test]
    async fn records_error_type_overloaded_when_shed() {
        let (provider, exporter) = test_provider();
        let meter = provider.meter("test-client");
        let layer = MetricsLayer::with_meter(&meter, Arc::new(default_classify));

        let inner = service_fn(|_req: Request<Full<Bytes>>| async {
            Err::<Response<ResponseBody>, _>(HttpError::Overloaded)
        });
        let mut svc = ServiceBuilder::new().layer(layer).service(inner);
        let req = Request::builder()
            .method(http::Method::GET)
            .uri("https://example.com/")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let err = svc.ready().await.unwrap().call(req).await.unwrap_err();
        assert!(matches!(err, HttpError::Overloaded));

        provider.force_flush().unwrap();
        let point = find_duration_point(&exporter, &[("error.type", "overloaded")])
            .expect("a shed request should record error.type=overloaded");
        assert_eq!(point.count(), 1);
    }
}
