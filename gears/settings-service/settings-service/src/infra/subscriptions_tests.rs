//! The subscription service: the key's lock around a registration and a retire,
//! and the order of its decisions.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::json;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::SubscriptionService;
use crate::domain::activation::testing::InProcessLocks;
use crate::domain::activation::{
    ActivationRepository, HeldKey, KeyLocks, ReadGate, SubscriptionManager, subscriber_of,
    tenant_text,
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
        self.service
            .register_subscription(
                &Self::ctx(self.base.tree.root),
                &self.base.key(name),
                tenant,
            )
            .await
            .map(|_| ())
    }

    async fn retire(&self, caller: Uuid, name: &str, tenant: Uuid) -> Result<(), DomainError> {
        self.service
            .retire_subscription(&Self::ctx(caller), &self.base.key(name), tenant)
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
        .register_subscription(&ctx, &key, root)
        .await
        .expect("first");
    let second = f
        .service
        .register_subscription(&ctx, &key, root)
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
        .register_subscription(&Fixture::ctx(f.base.tree.a), &key, c)
        .await;
    assert!(
        matches!(outside, Err(DomainError::NotFound { .. })),
        "{outside:?}"
    );

    assert_eq!(f.locks.asks_for(key.as_str()), 0, "no lock was asked for");
    assert_eq!(f.rows("flag").await, 0);
}

#[tokio::test]
async fn a_retire_deletes_under_the_lock_and_answers_not_found_for_a_key_it_never_held() {
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let root = f.base.tree.root;
    f.register("flag", root).await.expect("registered");
    let key = f.base.key("flag");
    let asked = f.locks.asks_for(key.as_str());

    f.retire(root, "flag", root).await.expect("retired");

    assert_eq!(f.rows("flag").await, 0);
    assert_eq!(f.locks.asks_for(key.as_str()), asked + 1);
    assert!(!f.locks.is_held(key.as_str()));
    let again = f.retire(root, "flag", root).await;
    assert!(
        matches!(again, Err(DomainError::NotFound { .. })),
        "{again:?}"
    );
    assert!(!f.locks.is_held(key.as_str()), "released on a 404 too");
}

#[tokio::test]
async fn a_retire_never_touches_another_subscribers_subscription() {
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let (root, a) = (f.base.tree.root, f.base.tree.a);
    f.register("flag", root).await.expect("registered");

    // A different gear (another subject) asks to retire the same triple.
    let stranger = SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(a)
        .build()
        .expect("context");
    let outcome = f
        .service
        .retire_subscription(&stranger, &f.base.key("flag"), root)
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
async fn a_retire_against_a_held_lock_is_unavailable_and_a_second_call_deletes_it() {
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let root = f.base.tree.root;
    f.register("flag", root).await.expect("registered");
    let key = f.base.key("flag");
    let owner = f.locks.hold(key.as_str());

    let refused = f.retire(root, "flag", root).await;
    assert!(
        matches!(refused, Err(DomainError::Unavailable { .. })),
        "{refused:?}"
    );
    assert_eq!(f.rows("flag").await, 1, "nothing deleted, still subscribed");
    assert!(f.locks.is_held(key.as_str()));

    // The owner lets go; the caller calls `retire` again.
    Box::new(owner).release().await;
    f.retire(root, "flag", root)
        .await
        .expect("a second call deletes it");
    assert_eq!(f.rows("flag").await, 0);
}
