//! REST handlers for the usage-collector gateway.

use std::sync::Arc;

use axum::extract::Path;
use axum::{Extension, Json};
use http::StatusCode;
use modkit::api::problem::Problem;
use modkit_security::SecurityContext;
use usage_collector_sdk::UsageCollectorError;
use usage_emitter::UsageEmitterRuntimeV1;

use crate::domain::Service;

use super::dto::{AllowedMetricResponse, CreateUsageRecordRequest, ModuleConfigResponse};

/// Handler for `POST /usage-collector/v1/records`.
///
/// # Errors
/// Returns a [`Problem`] on authorization failure, validation errors, or internal emitter failure.
#[tracing::instrument(
    skip_all,
    fields(
        module = %req.module,
        tenant_id = %req.tenant_id,
        resource_id = %req.resource_id,
    ),
)]
pub async fn handle_create_usage_record(
    Extension(ctx): Extension<SecurityContext>,
    Extension(runtime): Extension<Arc<dyn UsageEmitterRuntimeV1>>,
    Json(req): Json<CreateUsageRecordRequest>,
) -> Result<StatusCode, Problem> {
    let emitter = authorize_request(&ctx, &runtime, &req).await?;

    let mut builder = emitter
        .usage_record_builder(req.metric, req.value)
        .map_err(|e| canonical_error_to_problem(&e))?
        .with_timestamp(req.timestamp);

    // Normalize whitespace-only keys at the boundary so the builder always sees
    // `Some(non-empty)` or `None`. A raw whitespace key is treated as missing for gauges
    // (UUID generated at enqueue time) and surfaces as the canonical InvalidArgument for
    // counters at validation — consistent across both metric kinds.
    if let Some(key) = req
        .idempotency_key
        .map(|k| k.trim().to_owned())
        .filter(|k| !k.is_empty())
    {
        builder = builder.with_idempotency_key(key);
    }

    if let Some(meta) = req.metadata {
        builder = builder.with_metadata(meta);
    }

    // `usage_record_builder` prefills every required field (module, tenant_id,
    // resource_id, resource_type, metric, kind, value); the optional setters above
    // only add fields, never clear required ones — so this `.build()` is currently
    // unreachable on the error branch. The prefill invariant is regression-tested
    // by `cyberware-usage-emitter`'s `usage_record_builder_prefills_authorized_fields_for_gauge`
    // and `usage_record_builder_resolves_counter_kind_from_allowed_metrics` (in
    // `usage_record_builder_tests.rs`), which assert that `.build()` succeeds after
    // the prefill alone. We still funnel any future `.build()` error through
    // `canonical_error_to_problem` so a legitimate failure path keeps the canonical
    // 4xx contract instead of panicking inside an axum handler.
    let record = builder
        .build()
        .map_err(|e| canonical_error_to_problem(&e))?;
    emitter
        .enqueue(record)
        .await
        .map_err(|e| canonical_error_to_problem(&e))?;

    // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-gateway-ingest-handler:inst-gw-7
    Ok(StatusCode::NO_CONTENT)
    // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-gateway-ingest-handler:inst-gw-7
}

async fn authorize_request(
    ctx: &SecurityContext,
    runtime: &Arc<dyn UsageEmitterRuntimeV1>,
    req: &CreateUsageRecordRequest,
) -> Result<usage_emitter::UsageEmitter, Problem> {
    // Forward the caller's stated subject intent verbatim. `Some(s)` becomes
    // an explicit binding; `None` becomes the explicit "no subject" choice —
    // NOT the default-from-context fallback. Substituting the gateway's own
    // `SecurityContext` subject when the caller sent `None` would silently
    // attribute the request to the wrong principal at the PDP boundary.
    let factory = runtime.factory(&req.module).with_tenant(req.tenant_id);
    let factory = match req.subject.clone() {
        Some(s) => factory.with_subject(s),
        None => factory.without_subject(),
    };
    factory
        .authorize(ctx, req.resource_id, &req.resource_type)
        .await
        .map_err(|e| canonical_error_to_problem(&e))
}

/// Handler for `GET /usage-collector/v1/modules/{module_name}/config`.
///
/// # Errors
/// Returns a [`Problem`] if the module is not found or the collector call fails.
// @cpt-flow:cpt-cf-usage-collector-flow-sdk-and-ingest-core-fetch-module-config:p2
#[tracing::instrument(skip_all, fields(module = %module_name))]
pub async fn handle_get_module_config(
    Path(module_name): Path<String>,
    Extension(service): Extension<Arc<Service>>,
) -> Result<Json<ModuleConfigResponse>, Problem> {
    // @cpt-begin:cpt-cf-usage-collector-flow-sdk-and-ingest-core-fetch-module-config:p2:inst-cfg-2
    // Authenticated request received; ModKit pipeline enforces authentication before handler entry.
    // @cpt-end:cpt-cf-usage-collector-flow-sdk-and-ingest-core-fetch-module-config:p2:inst-cfg-2

    // @cpt-begin:cpt-cf-usage-collector-flow-sdk-and-ingest-core-fetch-module-config:p2:inst-cfg-3
    // Convert DomainError → UsageCollectorError via From, then funnel through the canonical
    // problem mapping so REST has a single source of truth for HTTP status / detail.
    let config = service
        .get_module_config(&module_name)
        .map_err(|e| canonical_error_to_problem(&UsageCollectorError::from(e)))?;
    // @cpt-end:cpt-cf-usage-collector-flow-sdk-and-ingest-core-fetch-module-config:p2:inst-cfg-3

    // @cpt-begin:cpt-cf-usage-collector-flow-sdk-and-ingest-core-fetch-module-config:p2:inst-cfg-5
    let response = ModuleConfigResponse {
        allowed_metrics: config
            .allowed_metrics
            .into_iter()
            .map(|m| AllowedMetricResponse {
                name: m.name,
                kind: m.kind,
            })
            .collect(),
        max_metadata_bytes: config.max_metadata_bytes,
    };

    Ok(Json(response))
    // @cpt-end:cpt-cf-usage-collector-flow-sdk-and-ingest-core-fetch-module-config:p2:inst-cfg-5
}

fn canonical_error_to_problem(e: &UsageCollectorError) -> Problem {
    // Round-trip through the canonical Problem so its serialized per-category
    // context (reason / constraint / violations / resource_type / resource_name)
    // is preserved as an RFC 9457 extension member rather than dropped to a
    // title/detail pair. Built-in canonical contexts are plain structs and
    // shouldn't fail to serialize; if they ever do we fall back to a
    // context-less Problem so the request still returns the correct status.
    match modkit_canonical_errors::Problem::from_error(e) {
        Ok(canonical) => {
            let status =
                StatusCode::from_u16(canonical.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            Problem::new(status, canonical.title, canonical.detail)
                .with_type(canonical.problem_type)
                .with_context(canonical.context)
        }
        Err(err) => {
            // Reaching this branch means a built-in canonical context failed to serialize —
            // e.g. a new error variant carrying a non-`Serialize`-clean context. The client
            // still gets the correct status code, but the per-category context (reason /
            // constraint / violations) is silently dropped. Escalate to `error!` so log-based
            // alerting fires, and attach a stable `context_serialization_failed` extension on
            // the returned Problem so operators have a machine-distinguishable signal that
            // does not depend on log scraping.
            tracing::error!(
                error = %err,
                source_error = %e,
                "canonical-to-problem conversion failed; falling back to context-less Problem"
            );
            Problem::new(
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                e.title(),
                e.detail(),
            )
            .with_context(serde_json::json!({ "context_serialization_failed": true }))
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "handlers_tests.rs"]
mod handlers_tests;
