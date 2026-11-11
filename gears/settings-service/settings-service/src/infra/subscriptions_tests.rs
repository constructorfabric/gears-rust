//! The subscription service: the key's lock around a registration and an unsubscribe,
//! the order of its decisions, and the per-instance identity of both.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::json;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::SubscriptionService;
use crate::domain::activation::testing::{
    InProcessLocks, ONE_INSTANCE, RecordingNotifyMetrics, capture, logged,
};
use crate::domain::activation::{
    ActivationRepository, HeldKey, KeyLocks, NotifyMetrics, ReadGate, SubscriptionManager,
    subscriber_of, tenant_text,
};
use crate::domain::error::DomainError;
use crate::domain::resolution::scope_class;
use crate::infra::storage::access_repo::AccessRepo;
use crate::infra::storage::activation_repo::ActivationRepo;
use crate::infra::storage::declaration_repo::DeclarationRepo;
use crate::infra::storage::value_repo::ValueRepo;
use crate::test_support::{FakeReadGate, ReadAnswer, ResolutionHarness};

type Service = SubscriptionService<DeclarationRepo, ValueRepo, AccessRepo, ActivationRepo>;

struct Fixture {
    base: ResolutionHarness,
    gate: Arc<FakeReadGate>,
    locks: Arc<InProcessLocks>,
    service: Service,
}

impl Fixture {
    async fn new() -> Self {
        let base = ResolutionHarness::new().await;
        let gate = FakeReadGate::allowing();
        let locks = InProcessLocks::with_budget(Duration::from_millis(60));
        let manager = Arc::new(SubscriptionManager::new(
            Arc::clone(&base.resolver),
            ActivationRepo,
            Arc::clone(&gate) as Arc<dyn ReadGate>,
            Arc::new(RecordingNotifyMetrics::default()) as Arc<dyn NotifyMetrics>,
        ));
        let service = SubscriptionService::new(
            Arc::clone(&base.db),
            manager,
            Arc::clone(&locks) as Arc<dyn KeyLocks>,
        );
        Self {
            base,
            gate,
            locks,
            service,
        }
    }

    fn ctx(tenant: Uuid) -> SecurityContext {
        crate::test_support::service_context_for(tenant)
    }

    async fn register(&self, name: &str, tenant: Uuid) -> Result<(), DomainError> {
        self.register_instance(name, tenant, Some(ONE_INSTANCE))
            .await
    }

    async fn register_instance(
        &self,
        name: &str,
        tenant: Uuid,
        instance_id: Option<Uuid>,
    ) -> Result<(), DomainError> {
        self.service
            .register_subscription(
                &Self::ctx(self.base.tree.root),
                &self.base.key(name),
                tenant,
                instance_id,
                Some("host"),
            )
            .await
            .map(|_| ())
    }

    async fn unsubscribe(&self, caller: Uuid, name: &str, tenant: Uuid) -> Result<(), DomainError> {
        self.unsubscribe_instance(caller, name, tenant, Some(ONE_INSTANCE))
            .await
    }

    async fn unsubscribe_instance(
        &self,
        caller: Uuid,
        name: &str,
        tenant: Uuid,
        instance_id: Option<Uuid>,
    ) -> Result<(), DomainError> {
        self.service
            .unsubscribe(
                &Self::ctx(caller),
                &self.base.key(name),
                tenant,
                instance_id,
            )
            .await
    }

    async fn rows(&self, name: &str) -> usize {
        let conn = self.base.db.conn().expect("connection");
        ActivationRepo
            .of_key(&conn, self.base.key(name).as_str())
            .await
            .expect("rows")
            .len()
    }
}

#[tokio::test]
async fn a_registration_commits_under_the_keys_lock_and_releases_it() {
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let root = f.base.tree.root;
    let key = f.base.key("flag");

    f.register("flag", root).await.expect("registered");

    assert_eq!(f.rows("flag").await, 1);
    assert_eq!(f.locks.asks_for(key.as_str()), 1);
    assert!(
        !f.locks.is_held(key.as_str()),
        "released before it answered"
    );
    assert_eq!(f.locks.released.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn registering_twice_keeps_one_row_and_answers_the_same_subscription() {
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let root = f.base.tree.root;
    let ctx = Fixture::ctx(root);
    let key = f.base.key("flag");
    let first = f
        .service
        .register_subscription(&ctx, &key, root, Some(ONE_INSTANCE), None)
        .await
        .expect("first");
    let second = f
        .service
        .register_subscription(&ctx, &key, root, Some(ONE_INSTANCE), None)
        .await
        .expect("second");
    assert_eq!(first.id, second.id);
    assert_eq!(first.subscriber, subscriber_of(&ctx));
    assert_eq!(first.tenant, tenant_text(root));
    assert_eq!(f.rows("flag").await, 1);
}

#[tokio::test]
async fn a_wait_that_runs_out_registers_nothing_and_leaves_the_lock_to_its_owner() {
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let key = f.base.key("flag");
    let owner = f.locks.hold(key.as_str());

    let outcome = f.register("flag", f.base.tree.root).await;

    assert!(
        matches!(outcome, Err(DomainError::Unavailable { .. })),
        "{outcome:?}"
    );
    assert_eq!(f.rows("flag").await, 0);
    assert!(f.locks.is_held(key.as_str()));
    drop(owner);
}

#[tokio::test]
async fn a_refused_registration_never_contends_for_the_lock() {
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let (root, c) = (f.base.tree.root, f.base.tree.c);
    let key = f.base.key("flag");

    f.gate.answer(ReadAnswer::Deny);
    assert!(matches!(
        f.register("flag", root).await,
        Err(DomainError::Unauthorized { .. })
    ));
    f.gate.answer(ReadAnswer::Down);
    assert!(matches!(
        f.register("flag", root).await,
        Err(DomainError::Unavailable { .. })
    ));
    f.gate.answer(ReadAnswer::Allow(
        crate::domain::category::DomainVisibility::Unrestricted,
    ));
    assert!(matches!(
        f.register("undeclared", root).await,
        Err(DomainError::NotFound { .. })
    ));
    // A tenant outside the caller's subtree.
    let outside = f
        .service
        .register_subscription(
            &Fixture::ctx(f.base.tree.a),
            &key,
            c,
            Some(ONE_INSTANCE),
            None,
        )
        .await;
    assert!(
        matches!(outside, Err(DomainError::NotFound { .. })),
        "{outside:?}"
    );

    assert_eq!(f.locks.asks_for(key.as_str()), 0, "no lock was asked for");
    assert_eq!(f.rows("flag").await, 0);
}

#[tokio::test]
async fn an_unsubscribe_deletes_under_the_lock_and_answers_not_found_for_a_key_it_never_held() {
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let root = f.base.tree.root;
    f.register("flag", root).await.expect("registered");
    let key = f.base.key("flag");
    let asked = f.locks.asks_for(key.as_str());

    f.unsubscribe(root, "flag", root)
        .await
        .expect("unsubscribed");

    assert_eq!(f.rows("flag").await, 0);
    assert_eq!(f.locks.asks_for(key.as_str()), asked + 1);
    assert!(!f.locks.is_held(key.as_str()));
    let again = f.unsubscribe(root, "flag", root).await;
    assert!(
        matches!(again, Err(DomainError::NotFound { .. })),
        "{again:?}"
    );
    assert!(!f.locks.is_held(key.as_str()), "released on a 404 too");
}

#[tokio::test]
async fn an_unsubscribe_never_touches_another_subscribers_subscription() {
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let (root, a) = (f.base.tree.root, f.base.tree.a);
    f.register("flag", root).await.expect("registered");

    // A different gear (another subject) asks to unsubscribe the same triple.
    let stranger = SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(a)
        .build()
        .expect("context");
    let outcome = f
        .service
        .unsubscribe(&stranger, &f.base.key("flag"), root, Some(ONE_INSTANCE))
        .await;

    assert!(
        matches!(outcome, Err(DomainError::NotFound { .. })),
        "{outcome:?}"
    );
    assert_eq!(
        f.rows("flag").await,
        1,
        "nothing of the other subscriber's deleted"
    );
}

#[tokio::test]
async fn an_unsubscribe_against_a_held_lock_is_unavailable_and_a_second_call_deletes_it() {
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let root = f.base.tree.root;
    f.register("flag", root).await.expect("registered");
    let key = f.base.key("flag");
    let owner = f.locks.hold(key.as_str());

    let refused = f.unsubscribe(root, "flag", root).await;
    assert!(
        matches!(refused, Err(DomainError::Unavailable { .. })),
        "{refused:?}"
    );
    assert_eq!(f.rows("flag").await, 1, "nothing deleted, still subscribed");
    assert!(f.locks.is_held(key.as_str()));

    // The owner lets go; the caller calls `unsubscribe` again.
    Box::new(owner).release().await;
    f.unsubscribe(root, "flag", root)
        .await
        .expect("a second call deletes it");
    assert_eq!(f.rows("flag").await, 0);
}

#[tokio::test]
async fn a_call_without_an_instance_id_is_invalid_input_and_never_contends_for_the_lock() {
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let root = f.base.tree.root;
    let key = f.base.key("flag");
    f.register("flag", root).await.expect("registered");
    let asked = f.locks.asks_for(key.as_str());

    let registration = f.register_instance("flag", root, None).await;
    let unsubscribe = f.unsubscribe_instance(root, "flag", root, None).await;

    for outcome in [registration, unsubscribe] {
        assert!(
            matches!(
                &outcome,
                Err(DomainError::Validation { code, .. })
                    if *code == crate::field::INSTANCE_ID_REQUIRED
            ),
            "{outcome:?}"
        );
    }
    assert_eq!(
        f.locks.asks_for(key.as_str()),
        asked,
        "no lock was asked for"
    );
    assert_eq!(
        f.rows("flag").await,
        1,
        "nothing registered, nothing deleted"
    );
}

#[tokio::test]
async fn each_instance_registers_and_unsubscribes_its_own_subscription_under_the_lock() {
    let (logs, _guard) = capture();
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let root = f.base.tree.root;
    let (one, two) = (Uuid::now_v7(), Uuid::now_v7());
    f.register_instance("flag", root, Some(one))
        .await
        .expect("one");
    f.register_instance("flag", root, Some(two))
        .await
        .expect("two");
    assert_eq!(f.rows("flag").await, 2);

    // An instance id the subscriber never registered: 404, nothing deleted.
    let never = f
        .unsubscribe_instance(root, "flag", root, Some(Uuid::now_v7()))
        .await;
    assert!(
        matches!(never, Err(DomainError::NotFound { .. })),
        "{never:?}"
    );
    assert_eq!(f.rows("flag").await, 2);

    f.unsubscribe_instance(root, "flag", root, Some(one))
        .await
        .expect("unsubscribed");
    let left = {
        let conn = f.base.db.conn().expect("connection");
        ActivationRepo
            .of_key(&conn, f.base.key("flag").as_str())
            .await
            .expect("rows")
    };
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].instance_id, two, "the sibling is untouched");
    assert!(!f.locks.is_held(f.base.key("flag").as_str()));

    let text = logged(&logs);
    let infos: Vec<&str> = text.lines().filter(|l| l.contains("INFO")).collect();
    for (line, instance) in [
        ("subscription registered", one),
        ("subscription registered", two),
        ("subscription unsubscribed", one),
    ] {
        assert!(
            infos
                .iter()
                .any(|l| l.contains(line) && l.contains(&instance.to_string())),
            "`{line}` names {instance}: {text}"
        );
    }
}
