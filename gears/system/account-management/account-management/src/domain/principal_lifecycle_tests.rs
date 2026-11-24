//! Unit tests for the tenant hard-delete bridge and the request-path error
//! mapping in [`super`].

#![allow(clippy::expect_used, reason = "test helpers")]

use std::sync::{Arc, Mutex};

use account_management_sdk::{PrincipalLifecycleEvent, PrincipalLifecycleHookError};
use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{PrincipalLifecycleNotifier, build_tenant_hard_delete_hook, into_domain_error};
use crate::domain::error::DomainError;
use crate::domain::tenant::hooks::HookError;

/// Notifier that records each delivery and answers with a fixed result.
struct RecordingNotifier {
    result: Result<(), PrincipalLifecycleHookError>,
    seen: Mutex<Vec<(Uuid, PrincipalLifecycleEvent)>>,
}

impl RecordingNotifier {
    fn answering(result: Result<(), PrincipalLifecycleHookError>) -> Arc<Self> {
        Arc::new(Self {
            result,
            seen: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl PrincipalLifecycleNotifier for RecordingNotifier {
    async fn notify(
        &self,
        ctx: &SecurityContext,
        event: &PrincipalLifecycleEvent,
    ) -> Result<(), PrincipalLifecycleHookError> {
        self.seen
            .lock()
            .expect("lock")
            .push((ctx.subject_tenant_id(), event.clone()));
        self.result.clone()
    }
}

#[tokio::test]
async fn tenant_hook_delivers_tenant_hard_delete_under_a_platform_scoped_actor() {
    let notifier = RecordingNotifier::answering(Ok(()));
    let hook = build_tenant_hard_delete_hook(notifier.clone());
    let tenant_id = Uuid::now_v7();

    hook(tenant_id).await.expect("hook succeeds");

    assert_eq!(
        *notifier.seen.lock().expect("lock"),
        vec![(
            Uuid::nil(),
            PrincipalLifecycleEvent::TenantHardDelete { tenant_id }
        )]
    );
}

#[tokio::test]
async fn tenant_hook_maps_retryable_to_retryable() {
    let hook = build_tenant_hard_delete_hook(RecordingNotifier::answering(Err(
        PrincipalLifecycleHookError::Retryable {
            detail: "db down".to_owned(),
        },
    )));

    let err = hook(Uuid::now_v7()).await.expect_err("hook fails");

    assert!(matches!(err, HookError::Retryable { .. }), "{err:?}");
}

#[tokio::test]
async fn tenant_hook_maps_terminal_to_terminal() {
    let hook = build_tenant_hard_delete_hook(RecordingNotifier::answering(Err(
        PrincipalLifecycleHookError::Terminal {
            detail: "bad data".to_owned(),
        },
    )));

    let err = hook(Uuid::now_v7()).await.expect_err("hook fails");

    assert!(matches!(err, HookError::Terminal { .. }), "{err:?}");
}

#[test]
fn request_path_maps_retryable_to_service_unavailable_and_terminal_to_internal() {
    let event = PrincipalLifecycleEvent::UserDeprovisioned {
        tenant_id: Uuid::now_v7(),
        user_id: Uuid::now_v7(),
    };

    let retryable = into_domain_error(
        &event,
        PrincipalLifecycleHookError::Retryable {
            detail: "x".to_owned(),
        },
    );
    assert!(
        matches!(retryable, DomainError::ServiceUnavailable { .. }),
        "{retryable:?}"
    );

    let terminal = into_domain_error(
        &event,
        PrincipalLifecycleHookError::Terminal {
            detail: "x".to_owned(),
        },
    );
    assert!(
        matches!(terminal, DomainError::Internal { .. }),
        "{terminal:?}"
    );
}
