use super::*;
use crate::telemetry::config::{
    Exporter, ExporterKind, OpenTelemetryConfig, OpenTelemetryResource, Sampler, TracingConfig,
};
use std::collections::{BTreeMap, HashMap};

/// Helper to build an `OpenTelemetryConfig` with the given tracing config.
fn otel_with_tracing(tracing: TracingConfig) -> OpenTelemetryConfig {
    OpenTelemetryConfig {
        tracing,
        ..Default::default()
    }
}

#[test]
#[cfg(feature = "otel")]
fn test_init_tracing_disabled() {
    let otel = otel_with_tracing(TracingConfig {
        enabled: false,
        ..Default::default()
    });

    let result = init_tracing(&otel);
    assert!(result.is_err());
}

#[tokio::test]
#[cfg(feature = "otel")]
async fn test_init_tracing_enabled() {
    let otel = otel_with_tracing(TracingConfig {
        enabled: true,
        ..Default::default()
    });

    let result = init_tracing(&otel);
    assert!(result.is_ok());
}

#[test]
#[cfg(feature = "otel")]
fn test_init_tracing_with_resource_attributes() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();

    let mut attrs = BTreeMap::new();
    attrs.insert("service.version".to_owned(), "1.0.0".to_owned());
    attrs.insert("deployment.environment".to_owned(), "test".to_owned());

    let otel = OpenTelemetryConfig {
        resource: OpenTelemetryResource {
            service_name: "test-service".to_owned(),
            attributes: attrs,
        },
        tracing: TracingConfig {
            enabled: true,
            ..Default::default()
        },
        ..Default::default()
    };

    let result = init_tracing(&otel);
    assert!(result.is_ok());
}

#[test]
#[cfg(feature = "otel")]
fn test_init_tracing_with_always_on_sampler() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();

    let otel = otel_with_tracing(TracingConfig {
        enabled: true,
        sampler: Some(Sampler::AlwaysOn {}),
        ..Default::default()
    });

    let result = init_tracing(&otel);
    assert!(result.is_ok());
}

#[test]
#[cfg(feature = "otel")]
fn test_init_tracing_with_always_off_sampler() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();

    let otel = otel_with_tracing(TracingConfig {
        enabled: true,
        sampler: Some(Sampler::AlwaysOff {}),
        ..Default::default()
    });

    let result = init_tracing(&otel);
    assert!(result.is_ok());
}

#[test]
#[cfg(feature = "otel")]
fn test_init_tracing_with_ratio_sampler() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();

    let otel = otel_with_tracing(TracingConfig {
        enabled: true,
        sampler: Some(Sampler::ParentBasedRatio { ratio: Some(0.5) }),
        ..Default::default()
    });

    let result = init_tracing(&otel);
    assert!(result.is_ok());
}

#[test]
#[cfg(feature = "otel")]
fn test_init_tracing_with_http_exporter() {
    let _rt = tokio::runtime::Runtime::new().unwrap();

    let otel = otel_with_tracing(TracingConfig {
        enabled: true,
        exporter: Some(Exporter {
            kind: ExporterKind::OtlpHttp,
            endpoint: Some("http://localhost:4318".to_owned()),
            headers: None,
            timeout_ms: Some(5000),
        }),
        ..Default::default()
    });

    let result = init_tracing(&otel);
    assert!(result.is_ok());
}

#[test]
#[cfg(feature = "otel")]
fn test_init_tracing_with_grpc_exporter() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();

    let otel = otel_with_tracing(TracingConfig {
        enabled: true,
        exporter: Some(Exporter {
            kind: ExporterKind::OtlpGrpc,
            endpoint: Some("http://localhost:4317".to_owned()),
            headers: None,
            timeout_ms: Some(5000),
        }),
        ..Default::default()
    });

    let result = init_tracing(&otel);
    assert!(result.is_ok());
}

#[test]
#[cfg(feature = "otel")]
fn test_build_headers_from_cfg_empty() {
    temp_env::with_var_unset("OTEL_EXPORTER_OTLP_HEADERS", || {
        let cfg = TracingConfig {
            enabled: true,
            ..Default::default()
        };

        let result = build_headers_from_cfg_and_env(cfg.exporter.as_ref());
        assert!(
            result.is_none(),
            "expected None when no headers configured and no env"
        );
    });
}

#[test]
#[cfg(feature = "otel")]
fn test_build_headers_from_cfg_with_headers() {
    let mut headers = HashMap::new();
    headers.insert("authorization".to_owned(), "Bearer token".to_owned());

    let cfg = TracingConfig {
        enabled: true,
        exporter: Some(Exporter {
            kind: ExporterKind::OtlpHttp,
            endpoint: Some("http://localhost:4318".to_owned()),
            headers: Some(headers.clone()),
            timeout_ms: None,
        }),
        ..Default::default()
    };

    let result = build_headers_from_cfg_and_env(cfg.exporter.as_ref());
    assert!(result.is_some());
    let result_headers = result.unwrap();
    assert_eq!(
        result_headers.get("authorization"),
        Some(&"Bearer token".to_owned())
    );
}

#[test]
#[cfg(feature = "otel")]
fn test_build_metadata_from_cfg_empty() {
    temp_env::with_var_unset("OTEL_EXPORTER_OTLP_HEADERS", || {
        let cfg = TracingConfig {
            enabled: true,
            ..Default::default()
        };

        let result = build_metadata_from_cfg_and_env(cfg.exporter.as_ref());
        assert!(
            result.is_none(),
            "expected None when no headers configured and no env"
        );
    });
}

#[test]
#[cfg(feature = "otel")]
fn test_build_metadata_from_cfg_with_headers() {
    let mut headers = HashMap::new();
    headers.insert("authorization".to_owned(), "Bearer token".to_owned());

    let cfg = TracingConfig {
        enabled: true,
        exporter: Some(Exporter {
            kind: ExporterKind::OtlpGrpc,
            endpoint: Some("http://localhost:4317".to_owned()),
            headers: Some(headers.clone()),
            timeout_ms: None,
        }),
        ..Default::default()
    };

    let result = build_metadata_from_cfg_and_env(cfg.exporter.as_ref());
    assert!(result.is_some());
    let metadata = result.unwrap();
    assert!(!metadata.is_empty());
}

#[test]
#[cfg(feature = "otel")]
fn test_build_metadata_multiple_headers() {
    let mut headers = HashMap::new();
    headers.insert("authorization".to_owned(), "Bearer token".to_owned());
    headers.insert("x-custom-header".to_owned(), "custom-value".to_owned());

    let cfg = TracingConfig {
        enabled: true,
        exporter: Some(Exporter {
            kind: ExporterKind::OtlpGrpc,
            endpoint: Some("http://localhost:4317".to_owned()),
            headers: Some(headers.clone()),
            timeout_ms: None,
        }),
        ..Default::default()
    };

    let result = build_metadata_from_cfg_and_env(cfg.exporter.as_ref());
    assert!(result.is_some());
    let metadata = result.unwrap();
    assert_eq!(metadata.len(), 2);
}

#[test]
#[cfg(feature = "otel")]
fn test_build_metadata_invalid_header_name_skipped() {
    let mut headers = HashMap::new();
    headers.insert("valid-header".to_owned(), "value1".to_owned());
    headers.insert("invalid header with spaces".to_owned(), "value2".to_owned());

    let cfg = TracingConfig {
        enabled: true,
        exporter: Some(Exporter {
            kind: ExporterKind::OtlpGrpc,
            endpoint: Some("http://localhost:4317".to_owned()),
            headers: Some(headers.clone()),
            timeout_ms: None,
        }),
        ..Default::default()
    };

    let result = build_metadata_from_cfg_and_env(cfg.exporter.as_ref());
    assert!(result.is_some());
    let metadata = result.unwrap();
    // Should only have the valid header
    assert_eq!(metadata.len(), 1);
}

#[test]
fn test_shutdown_tracing_does_not_panic() {
    // Should not panic regardless of feature state
    shutdown_tracing();
}

#[test]
#[cfg(feature = "otel")]
fn test_init_metrics_provider_disabled() {
    let otel = OpenTelemetryConfig {
        metrics: crate::telemetry::config::MetricsConfig {
            enabled: false,
            ..Default::default()
        },
        ..Default::default()
    };
    // Disabled path returns Ok (noop — global provider stays NoopMeterProvider)
    let result = init_metrics_provider(&otel);
    assert!(result.is_ok());
}
