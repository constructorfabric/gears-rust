//! Tests for [`super::HookDispatcher`]: hook discovery through the types
//! registry and scoped `ClientHub`, fan-out to every hook, failure ranking,
//! and containment of misbehaving hooks.

#![allow(clippy::expect_used, reason = "test helpers")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use account_management_sdk::{
    PrincipalLifecycleEvent, PrincipalLifecycleHookError, PrincipalLifecycleHookSpecV1,
    PrincipalLifecycleHookV1,
};
use async_trait::async_trait;
use toolkit::ClientHub;
use toolkit::client_hub::ClientScope;
use toolkit::gts::PluginV1;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry};
use uuid::Uuid;

use super::HookDispatcher;
use crate::domain::principal_lifecycle::PrincipalLifecycleNotifier;

/// Shared log of `(hook name, event)` pairs, in delivery order.
type DeliveryLog = Arc<Mutex<Vec<(&'static str, PrincipalLifecycleEvent)>>>;

/// What a test hook does when an event arrives.
#[derive(Clone)]
enum Behaviour {
    Succeed,
    Fail(PrincipalLifecycleHookError),
    Panic,
    /// Never answers. Sets the flag when its future is dropped, which is
    /// how a test sees that the hook was cancelled rather than left running.
    Hang(Arc<AtomicBool>),
}

/// Sets its flag when dropped.
struct SetOnDrop(Arc<AtomicBool>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct TestHook {
    name: &'static str,
    behaviour: Behaviour,
    log: DeliveryLog,
}

#[async_trait]
impl PrincipalLifecycleHookV1 for TestHook {
    async fn on_event(
        &self,
        _ctx: &SecurityContext,
        event: &PrincipalLifecycleEvent,
    ) -> Result<(), PrincipalLifecycleHookError> {
        self.log
            .lock()
            .expect("log lock")
            .push((self.name, event.clone()));
        match &self.behaviour {
            Behaviour::Succeed => Ok(()),
            Behaviour::Fail(err) => Err(err.clone()),
            Behaviour::Panic => panic!("test hook panics on purpose"),
            Behaviour::Hang(dropped) => {
                let _guard = SetOnDrop(Arc::clone(dropped));
                std::future::pending().await
            }
        }
    }
}

/// Publish a hook instance for `segment` and return it with its id.
fn instance(segment: &str) -> (String, serde_json::Value) {
    let (id, payload) =
        PluginV1::<PrincipalLifecycleHookSpecV1>::build_registration(segment, "test", 0)
            .expect("build_registration succeeds for the marker spec");
    (id.as_ref().to_owned(), payload)
}

/// A platform registry holding the given `(instance id, document)` pairs.
fn registry_with(instances: Vec<(String, serde_json::Value)>) -> Arc<MockTypesRegistry> {
    let registry = Arc::new(MockTypesRegistry::new());
    for (gts_id, content) in instances {
        registry.seed(&gts_id, content);
    }
    registry
}

/// A registry advertising one instance per hook, and a hub holding each
/// hook's scoped client. Hooks are given in an order that differs from
/// their instance-id order, so delivery order is shown to follow the ids.
fn dispatcher_with(hooks: Vec<(&'static str, Behaviour)>) -> (HookDispatcher, DeliveryLog) {
    let log: DeliveryLog = Arc::new(Mutex::new(Vec::new()));
    let hub = Arc::new(ClientHub::new());
    let mut instances = Vec::new();
    for (name, behaviour) in hooks {
        let (gts_id, content) = instance(&format!("test.pkg._.{name}.v1"));
        let hook: Arc<dyn PrincipalLifecycleHookV1> = Arc::new(TestHook {
            name,
            behaviour,
            log: Arc::clone(&log),
        });
        hub.register_scoped::<dyn PrincipalLifecycleHookV1>(ClientScope::gts_id(&gts_id), hook);
        instances.push((gts_id, content));
    }
    (HookDispatcher::new(hub, registry_with(instances)), log)
}

fn user_event() -> PrincipalLifecycleEvent {
    PrincipalLifecycleEvent::UserDeprovisioned {
        tenant_id: Uuid::now_v7(),
        user_id: Uuid::now_v7(),
    }
}

fn retryable() -> PrincipalLifecycleHookError {
    PrincipalLifecycleHookError::Retryable {
        detail: "db down".to_owned(),
    }
}

fn terminal() -> PrincipalLifecycleHookError {
    PrincipalLifecycleHookError::Terminal {
        detail: "bad data".to_owned(),
    }
}

fn names(log: &DeliveryLog) -> Vec<&'static str> {
    log.lock()
        .expect("log lock")
        .iter()
        .map(|(name, _)| *name)
        .collect()
}

#[tokio::test]
async fn no_registered_hook_is_success() {
    let (dispatcher, _) = dispatcher_with(Vec::new());

    dispatcher
        .notify(&SecurityContext::anonymous(), &user_event())
        .await
        .expect("nothing to deliver to");
}

#[tokio::test]
async fn every_hook_receives_the_event_in_instance_id_order() {
    let (dispatcher, log) = dispatcher_with(vec![
        ("zeta", Behaviour::Succeed),
        ("alpha", Behaviour::Succeed),
    ]);
    let event = user_event();

    dispatcher
        .notify(&SecurityContext::anonymous(), &event)
        .await
        .expect("both hooks succeed");

    assert_eq!(
        *log.lock().expect("log lock"),
        vec![("alpha", event.clone()), ("zeta", event)]
    );
}

#[tokio::test]
async fn a_failing_hook_does_not_stop_the_others() {
    let (dispatcher, log) = dispatcher_with(vec![
        ("alpha", Behaviour::Fail(retryable())),
        ("beta", Behaviour::Succeed),
    ]);

    let err = dispatcher
        .notify(&SecurityContext::anonymous(), &user_event())
        .await
        .expect_err("the failure surfaces");

    assert!(
        matches!(err, PrincipalLifecycleHookError::Retryable { .. }),
        "{err:?}"
    );
    assert_eq!(names(&log), vec!["alpha", "beta"]);
}

#[tokio::test]
async fn terminal_outranks_retryable_whatever_the_order() {
    for hooks in [
        vec![
            ("alpha", Behaviour::Fail(terminal())),
            ("beta", Behaviour::Fail(retryable())),
        ],
        vec![
            ("alpha", Behaviour::Fail(retryable())),
            ("beta", Behaviour::Fail(terminal())),
        ],
    ] {
        let (dispatcher, _) = dispatcher_with(hooks);

        let err = dispatcher
            .notify(&SecurityContext::anonymous(), &user_event())
            .await
            .expect_err("the failures surface");

        assert!(
            matches!(err, PrincipalLifecycleHookError::Terminal { .. }),
            "{err:?}"
        );
    }
}

#[tokio::test]
async fn a_panicking_hook_is_contained_as_terminal() {
    let (dispatcher, log) = dispatcher_with(vec![
        ("alpha", Behaviour::Panic),
        ("beta", Behaviour::Succeed),
    ]);

    let err = dispatcher
        .notify(&SecurityContext::anonymous(), &user_event())
        .await
        .expect_err("the panic surfaces as a failure");

    assert!(
        matches!(err, PrincipalLifecycleHookError::Terminal { .. }),
        "{err:?}"
    );
    assert_eq!(names(&log), vec!["alpha", "beta"]);
}

#[tokio::test]
async fn an_advertised_hook_without_a_client_fails_retryable() {
    // The instance is in the registry but nothing registered its client —
    // typically its gear is still starting. Skipping it would silently drop
    // that gear's cleanup, so the delivery fails and is retried.
    let registry = registry_with(vec![instance("test.pkg._.missing.v1")]);
    let dispatcher = HookDispatcher::new(Arc::new(ClientHub::new()), registry);

    let err = dispatcher
        .notify(&SecurityContext::anonymous(), &user_event())
        .await
        .expect_err("a missing client fails the delivery");

    assert!(
        matches!(err, PrincipalLifecycleHookError::Retryable { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn an_unavailable_registry_fails_retryable() {
    let registry = registry_with(vec![instance("test.pkg._.alpha.v1")]);
    registry.inject(Fault::on(Call::List).fail(CanonicalError::service_unavailable().create()));
    let dispatcher = HookDispatcher::new(Arc::new(ClientHub::new()), registry);

    let err = dispatcher
        .notify(&SecurityContext::anonymous(), &user_event())
        .await
        .expect_err("discovery failure surfaces");

    assert!(
        matches!(err, PrincipalLifecycleHookError::Retryable { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_malformed_hook_instance_fails_terminal() {
    // The instance is not a `PluginV1<PrincipalLifecycleHookSpecV1>`: a
    // broken deployment no retry can fix, so it must reach an operator
    // rather than be retried forever.
    let (gts_id, _) = instance("test.pkg._.broken.v1");
    let registry = registry_with(vec![(gts_id.clone(), serde_json::json!({ "id": gts_id }))]);
    let dispatcher = HookDispatcher::new(Arc::new(ClientHub::new()), registry);

    let err = dispatcher
        .notify(&SecurityContext::anonymous(), &user_event())
        .await
        .expect_err("a malformed instance fails the delivery");

    assert!(
        matches!(err, PrincipalLifecycleHookError::Terminal { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_hook_instance_with_a_mismatched_content_id_fails_terminal() {
    // The registry lists the instance under one id while its content claims
    // another: inconsistent data, not a transient condition.
    let (listed_id, _) = instance("test.pkg._.listed.v1");
    let (_, other) = instance("test.pkg._.other.v1");
    let registry = registry_with(vec![(listed_id, other)]);
    let dispatcher = HookDispatcher::new(Arc::new(ClientHub::new()), registry);

    let err = dispatcher
        .notify(&SecurityContext::anonymous(), &user_event())
        .await
        .expect_err("a mismatched instance fails the delivery");

    assert!(
        matches!(err, PrincipalLifecycleHookError::Terminal { .. }),
        "{err:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_hook_that_outlives_its_budget_is_cancelled_not_left_running() {
    // A hook that never answers must not keep running after its delivery
    // gives up: otherwise every retry or retention tick would start one
    // more copy beside it. The paused clock jumps straight to the timeout.
    let dropped = Arc::new(AtomicBool::new(false));
    let (dispatcher, log) = dispatcher_with(vec![
        ("alpha", Behaviour::Hang(Arc::clone(&dropped))),
        ("beta", Behaviour::Succeed),
    ]);

    let err = dispatcher
        .notify(&SecurityContext::anonymous(), &user_event())
        .await
        .expect_err("the hung hook fails the delivery");

    assert!(
        matches!(err, PrincipalLifecycleHookError::Retryable { .. }),
        "{err:?}"
    );
    assert!(
        dropped.load(Ordering::SeqCst),
        "the hung hook's future must be dropped, i.e. cancelled"
    );
    assert_eq!(
        names(&log),
        vec!["alpha", "beta"],
        "the next hook still runs after the hung one is cancelled"
    );
}

#[tokio::test]
async fn a_compatible_minor_version_hook_is_delivered_to() {
    // The listing pattern `…principal_lifecycle_hook.v1~` matches a hook
    // registered under the compatible minor version `….v1.0~`. Dropping it
    // would let a deletion skip that hook's cleanup without any error.
    let minor_id = "gts.cf.toolkit.plugins.plugin.v1~cf.core.am.principal_lifecycle_hook.v1.0~test.pkg._.minor.v1";
    let (_, mut document) = instance("test.pkg._.minor.v1");
    document["id"] = serde_json::Value::String(minor_id.to_owned());

    let log: DeliveryLog = Arc::new(Mutex::new(Vec::new()));
    let hub = Arc::new(ClientHub::new());
    let hook: Arc<dyn PrincipalLifecycleHookV1> = Arc::new(TestHook {
        name: "minor",
        behaviour: Behaviour::Succeed,
        log: Arc::clone(&log),
    });
    hub.register_scoped::<dyn PrincipalLifecycleHookV1>(ClientScope::gts_id(minor_id), hook);
    let dispatcher = HookDispatcher::new(hub, registry_with(vec![(minor_id.to_owned(), document)]));

    dispatcher
        .notify(&SecurityContext::anonymous(), &user_event())
        .await
        .expect("the minor-version hook is delivered to");

    assert_eq!(names(&log), vec!["minor"]);
}
