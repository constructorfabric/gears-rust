//! Crate-internal `UsageEmitterFactory` tests.
//!
//! The broad authorize matrix (allow/deny/failing `AuthZ`, PDP property
//! assertions, subject/without_subject/default-from-ctx, collector error
//! propagation, barrier-mode) lives in `tests/emitter_tests.rs`, which builds
//! a runtime through the real `UsageEmitterRuntime::build` path. This file is
//! restricted to cases that genuinely need crate-internal access:
//!
//! - direct unit tests for `enforcer_error_to_emitter_error`, a `pub(crate)`
//!   mapper that is not re-exported and cannot be exercised through the public
//!   factory API; and
//! - `authorize_populates_max_metadata_bytes_from_module_config`, which reads
//!   the `pub(crate)` `UsageEmitter.max_metadata_bytes` field directly to prove
//!   the factory plumbed it from the collector response rather than
//!   substituting a local default.

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::pep::ConstraintCompileError;
use authz_resolver_sdk::{AuthZResolverClient, AuthZResolverError, DenyReason, EnforcerError};
use modkit_db::migration_runner::run_migrations_for_testing;
use modkit_db::outbox::outbox_migrations;
use modkit_db::{ConnectOpts, Db, connect_db};
use modkit_security::SecurityContext;
use usage_collector_sdk::UsageCollectorClientV1;
use usage_collector_sdk::models::UsageRecord;
use uuid::Uuid;

use super::UsageEmitterFactory;
use crate::api::UsageEmitterRuntimeV1;
use crate::config::UsageEmitterConfig;
use crate::domain::runtime::UsageEmitterRuntime;
use crate::error::{UsageEmitterError, enforcer_error_to_emitter_error};

const TEST_MODULE: &str = "test-module";

// ── Mocks (crate-internal `UsageEmitterError` is the collector error type) ────

struct AllowAllAuthZ;

#[async_trait]
impl AuthZResolverClient for AllowAllAuthZ {
    async fn evaluate(
        &self,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext::default(),
        })
    }
}

/// Returns a non-default `max_metadata_bytes` so the factory-plumbing test can
/// prove the emitter copies the field from the collector response rather than
/// substituting a local default.
struct NoopCollectorWith4096;

#[async_trait]
impl UsageCollectorClientV1 for NoopCollectorWith4096 {
    async fn create_usage_record(&self, _record: UsageRecord) -> Result<(), UsageEmitterError> {
        Ok(())
    }

    async fn get_module_config(
        &self,
        _module_name: &str,
    ) -> Result<usage_collector_sdk::ModuleConfig, UsageEmitterError> {
        Ok(usage_collector_sdk::ModuleConfig {
            allowed_metrics: vec![],
            max_metadata_bytes: 4096,
        })
    }
}

async fn build_db(name: &str) -> Db {
    let url = format!("sqlite:file:{name}?mode=memory&cache=shared");
    let db = connect_db(
        &url,
        ConnectOpts {
            max_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    run_migrations_for_testing(&db, outbox_migrations())
        .await
        .unwrap();
    db
}

async fn build_factory(db: Db, authz: Arc<dyn AuthZResolverClient>) -> UsageEmitterFactory {
    let runtime = UsageEmitterRuntime::build(
        UsageEmitterConfig::default(),
        db,
        authz,
        Arc::new(NoopCollectorWith4096),
    )
    .await
    .unwrap();
    runtime.factory(TEST_MODULE)
}

fn make_ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(Uuid::new_v4())
        .build()
        .unwrap()
}

// ── enforcer_error_to_emitter_error ──────────────────────────────────────────

#[test]
fn enforcer_denied_without_reason_uses_authorization_denied_fallback() {
    let err = enforcer_error_to_emitter_error(EnforcerError::Denied { deny_reason: None });
    let UsageEmitterError::PermissionDenied { ctx, .. } = err else {
        panic!("expected PermissionDenied");
    };
    assert_eq!(ctx.reason, "AUTHORIZATION_DENIED");
}

#[test]
fn enforcer_denied_forwards_deny_reason_error_code() {
    let deny_reason = Some(DenyReason {
        error_code: "ERR_FORBIDDEN".to_owned(),
        details: Some("tenant not allowed".to_owned()),
    });
    let err = enforcer_error_to_emitter_error(EnforcerError::Denied { deny_reason });
    let UsageEmitterError::PermissionDenied { ctx, .. } = err else {
        panic!("expected PermissionDenied");
    };
    assert_eq!(
        ctx.reason, "ERR_FORBIDDEN",
        "PDP `deny_reason.error_code` must be surfaced as the canonical reason"
    );
}

#[test]
fn enforcer_denied_forwards_error_code_when_details_absent() {
    let deny_reason = Some(DenyReason {
        error_code: "ERR_FORBIDDEN".to_owned(),
        details: None,
    });
    let err = enforcer_error_to_emitter_error(EnforcerError::Denied { deny_reason });
    let UsageEmitterError::PermissionDenied { ctx, .. } = err else {
        panic!("expected PermissionDenied");
    };
    assert_eq!(ctx.reason, "ERR_FORBIDDEN");
}

#[test]
fn enforcer_compile_failed_maps_to_permission_denied() {
    let err = enforcer_error_to_emitter_error(EnforcerError::CompileFailed(
        ConstraintCompileError::ConstraintsRequiredButAbsent,
    ));
    assert!(matches!(err, UsageEmitterError::PermissionDenied { .. }));
}

#[test]
fn enforcer_evaluation_failed_maps_to_permission_denied() {
    let err = enforcer_error_to_emitter_error(EnforcerError::EvaluationFailed(
        AuthZResolverError::Internal("rpc error".to_owned()),
    ));
    assert!(matches!(err, UsageEmitterError::PermissionDenied { .. }));
}

// ── max_metadata_bytes plumbing (reads `pub(crate)` emitter field) ───────────

#[tokio::test]
async fn authorize_populates_max_metadata_bytes_from_module_config() {
    // `NoopCollectorWith4096::get_module_config` returns
    // `max_metadata_bytes: 4096` (a non-default value). The authorized
    // emitter must surface that exact value, proving the factory copies the
    // field from the collector response rather than substituting a local
    // default. The field is `pub(crate)` and cannot be inspected from an
    // out-of-crate integration test.
    let db = build_db("emit_max_metadata_bytes_plumbed").await;
    let factory = build_factory(db, Arc::new(AllowAllAuthZ)).await;
    let ctx = make_ctx();
    let emitter = factory
        .clone()
        .authorize(&ctx, Uuid::new_v4(), "test.resource")
        .await
        .unwrap();
    assert_eq!(emitter.max_metadata_bytes, 4096);
}
