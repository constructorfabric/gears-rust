//! `record_change` called as the commit calls it: over the rows the change left
//! and the pre-image it replaced, with the subscriptions that qualified.

use std::sync::Arc;

use secrecy::SecretString;
use serde_json::json;
use toolkit_db::secure::SecureDeleteExt;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::activation::testing::one;
use crate::domain::activation::{
    ActivationPublisher, ActivationRepository, ActivationState, ActivationStatus, ExpectedValue,
    InstanceKey, Qualified, ReadGate, RecordedChange, SubscriberTenant, Subscription,
    SubscriptionManager, tenant_text,
};
use crate::domain::declaration::{Declaration, DeclarationRepository};
use crate::domain::error::DomainError;
use crate::domain::resolution::scope_class;
use crate::domain::value::{StoredValue, ValueRepository};
use crate::infra::storage::access_repo::AccessRepo;
use crate::infra::storage::activation_repo::ActivationRepo;
use crate::infra::storage::declaration_repo::DeclarationRepo;
use crate::infra::storage::value_repo::ValueRepo;
use crate::test_support::{FakeReadGate, ResolutionHarness, SECRET};

type Publisher = ActivationPublisher<DeclarationRepo, ValueRepo, AccessRepo, ActivationRepo>;

struct Fixture {
    base: ResolutionHarness,
    publisher: Publisher,
    manager: SubscriptionManager<DeclarationRepo, ValueRepo, AccessRepo, ActivationRepo>,
}

impl Fixture {
    async fn new() -> Self {
        let base = ResolutionHarness::new().await;
        let publisher = ActivationPublisher::new(Arc::clone(&base.resolver), ActivationRepo);
        let manager = SubscriptionManager::new(
            Arc::clone(&base.resolver),
            ActivationRepo,
            FakeReadGate::allowing() as Arc<dyn ReadGate>,
            Arc::new(crate::domain::activation::NoNotifyMetrics),
        );
        Self {
            base,
            publisher,
            manager,
        }
    }

    async fn declaration(&self, name: &str) -> Declaration {
        let conn = self.base.db.conn().expect("connection");
        DeclarationRepo
            .find_by_key(
                &conn,
                &AccessScope::allow_all(),
                self.base.key(name).as_str(),
            )
            .await
            .expect("lookup")
            .expect("declared")
    }

    async fn subscribe(&self, subscriber: &str, name: &str, tenant: Uuid) {
        let conn = self.base.db.conn().expect("connection");
        ActivationRepo
            .register(
                &conn,
                &one(
                    subscriber,
                    self.base.key(name).as_str(),
                    &tenant_text(tenant),
                ),
                None,
            )
            .await
            .expect("registered");
    }

    async fn row(&self, declaration: Uuid, tenant: Uuid) -> Option<StoredValue> {
        let conn = self.base.db.conn().expect("connection");
        ValueRepo
            .find_one(&conn, &AccessScope::allow_all(), declaration, tenant)
            .await
            .expect("row")
    }

    async fn state(&self, subscriber: &str, name: &str, tenant: Uuid) -> Option<ActivationState> {
        let conn = self.base.db.conn().expect("connection");
        ActivationRepo
            .state(
                &conn,
                &one(
                    subscriber,
                    self.base.key(name).as_str(),
                    &tenant_text(tenant),
                ),
            )
            .await
            .expect("state")
    }

    async fn qualify(&self, declaration: &Declaration, written: Uuid) -> Vec<Qualified> {
        let conn = self.base.db.conn().expect("connection");
        self.manager
            .resolve_subscribers(&conn, declaration, written)
            .await
            .expect("qualified")
    }

    /// Record a change at `written` whose rows are already stored.
    async fn record(
        &self,
        declaration: &Declaration,
        written: Uuid,
        pre_image: Option<&StoredValue>,
        qualified: &[Qualified],
        plaintext: Option<&SecretString>,
    ) -> Result<Vec<SubscriberTenant>, DomainError> {
        self.record_as(
            Uuid::new_v4(),
            declaration,
            written,
            pre_image,
            qualified,
            plaintext,
        )
        .await
    }

    /// The identity of instance `n` of `subscriber` for `name` at `tenant`.
    fn instance(&self, subscriber: &str, n: u128, name: &str, tenant: Uuid) -> InstanceKey {
        InstanceKey {
            subscriber: subscriber.to_owned(),
            instance_id: Uuid::from_u128(0x1000 + n),
            key: self.base.key(name).as_str().to_owned(),
            tenant: tenant_text(tenant),
        }
    }

    /// Register instance `n` of `subscriber`.
    async fn subscribe_instance(
        &self,
        subscriber: &str,
        n: u128,
        name: &str,
        tenant: Uuid,
    ) -> InstanceKey {
        let instance = self.instance(subscriber, n, name, tenant);
        let conn = self.base.db.conn().expect("connection");
        ActivationRepo
            .register(&conn, &instance, None)
            .await
            .expect("registered");
        instance
    }

    async fn state_of(&self, instance: &InstanceKey) -> Option<ActivationState> {
        let conn = self.base.db.conn().expect("connection");
        ActivationRepo.state(&conn, instance).await.expect("state")
    }

    async fn subscription_of(&self, instance: &InstanceKey) -> Subscription {
        let conn = self.base.db.conn().expect("connection");
        ActivationRepo
            .find(&conn, instance)
            .await
            .expect("subscription")
            .expect("subscribed")
    }

    /// Count one accepted delivery on every due state, then call the
    /// instance's state of `change_ref` silent and mark its subscription, as
    /// the delivery loop does.
    async fn silence(&self, instance: &InstanceKey, change_ref: Uuid) {
        let conn = self.base.db.conn().expect("connection");
        let at = time::OffsetDateTime::now_utc();
        let due = ActivationRepo
            .due(&conn, at + time::Duration::days(1))
            .await
            .expect("due");
        ActivationRepo
            .stamp_due(&conn, &due, at)
            .await
            .expect("stamped");
        assert!(
            ActivationRepo
                .call_silent(&conn, instance, change_ref, at)
                .await
                .expect("called silent"),
            "the state was awaiting the change"
        );
        ActivationRepo
            .mark_silent(&conn, instance, at)
            .await
            .expect("marked");
    }

    /// Record a change at `written` under `change_ref`.
    async fn record_as(
        &self,
        change_ref: Uuid,
        declaration: &Declaration,
        written: Uuid,
        pre_image: Option<&StoredValue>,
        qualified: &[Qualified],
        plaintext: Option<&SecretString>,
    ) -> Result<Vec<SubscriberTenant>, DomainError> {
        let conn = self.base.db.conn().expect("connection");
        self.publisher
            .record_change(
                &conn,
                &RecordedChange {
                    declaration,
                    written,
                    root: self.base.tree.root,
                    pre_image,
                    change_ref,
                    qualified,
                    plaintext,
                },
            )
            .await
    }
}

#[tokio::test]
async fn a_subscription_deleted_before_the_upsert_gets_no_state_while_the_others_get_theirs() {
    let f = Fixture::new().await;
    let (root, a) = (f.base.tree.root, f.base.tree.a);
    let id = f
        .base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    for subscriber in ["kept", "retired"] {
        f.subscribe(subscriber, "flag", a).await;
    }
    let declaration = f.declaration("flag").await;
    // The change's read set holds both subscriptions...
    let qualified = f.qualify(&declaration, root).await;
    assert_eq!(qualified.len(), 2);
    // ...and one is retired before the states are written.
    {
        use crate::infra::storage::entity::subscription::{Column, Entity};
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        let conn = f.base.db.conn().expect("connection");
        Entity::delete_many()
            .filter(Column::Subscriber.eq("retired"))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(&conn)
            .await
            .expect("deleted");
    }
    f.base.set(id, root, json!(true)).await;

    let written = f
        .record(&declaration, root, None, &qualified, None)
        .await
        .expect("recorded");

    assert_eq!(
        written,
        vec![SubscriberTenant {
            subscriber: "kept".to_owned(),
            tenant: tenant_text(a),
        }]
    );
    assert!(f.state("kept", "flag", a).await.is_some());
    assert!(
        f.state("retired", "flag", a).await.is_none(),
        "the insert-select finds no subscription for it"
    );
}

#[tokio::test]
async fn a_revert_is_judged_from_the_pre_image() {
    let f = Fixture::new().await;
    let root = f.base.tree.root;
    let id = f
        .base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    f.base.set(id, root, json!(true)).await;
    let pre_image = f.row(id, root).await.expect("the override");
    f.subscribe("gear", "flag", root).await;
    let declaration = f.declaration("flag").await;
    let qualified = f.qualify(&declaration, root).await;
    // The override is cleared: the value falls back to the default.
    {
        let conn = f.base.db.conn().expect("connection");
        ValueRepo
            .delete(
                &conn,
                &AccessScope::allow_all(),
                id,
                root,
                pre_image.last_change_at,
            )
            .await
            .expect("cleared");
    }

    f.record(&declaration, root, Some(&pre_image), &qualified, None)
        .await
        .expect("recorded");

    let state = f
        .state("gear", "flag", root)
        .await
        .expect("true to false is a change");
    assert_eq!(state.expected_value, "false");
}

#[tokio::test]
async fn a_first_override_equal_to_what_the_scope_inherited_affects_nothing() {
    let f = Fixture::new().await;
    let (root, a) = (f.base.tree.root, f.base.tree.a);
    let id = f
        .base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    f.base.set(id, root, json!(true)).await;
    f.subscribe("gear", "flag", a).await;
    let declaration = f.declaration("flag").await;
    let qualified = f.qualify(&declaration, a).await;
    // `a` gains an override of the value it already inherited.
    f.base.set(id, a, json!(true)).await;

    let written = f
        .record(&declaration, a, None, &qualified, None)
        .await
        .expect("recorded");

    assert!(written.is_empty());
    assert!(f.state("gear", "flag", a).await.is_none());
}

#[tokio::test]
async fn an_override_flagged_for_review_does_not_shadow_the_change() {
    // The resolver falls through a flagged row, so the change reaches `b`.
    let f = Fixture::new().await;
    let (root, b) = (f.base.tree.root, f.base.tree.b);
    let id = f
        .base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    f.base.set_flagged(id, b, json!(false)).await;
    f.subscribe("gear", "flag", b).await;
    let declaration = f.declaration("flag").await;
    let qualified = f.qualify(&declaration, root).await;
    f.base.set(id, root, json!(true)).await;

    f.record(&declaration, root, None, &qualified, None)
        .await
        .expect("recorded");

    assert_eq!(
        f.state("gear", "flag", b).await.map(|s| s.expected_value),
        Some("true".to_owned())
    );
}

#[tokio::test]
async fn a_change_that_qualified_nobody_writes_nothing() {
    let f = Fixture::new().await;
    let root = f.base.tree.root;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let declaration = f.declaration("flag").await;
    let written = f
        .record(&declaration, root, None, &[], None)
        .await
        .expect("recorded");
    assert!(written.is_empty());
}

#[tokio::test]
async fn a_secret_change_without_its_plaintext_is_refused_not_hashed_from_nothing() {
    let f = Fixture::new().await;
    let root = f.base.tree.root;
    let id = f
        .base
        .declare_typed("pw", scope_class::CASCADING, json!(""), SECRET, "secret")
        .await;
    f.base.set_secret(id, root, "ref-1").await;
    f.subscribe("gear", "pw", root).await;
    let declaration = f.declaration("pw").await;
    let qualified = f.qualify(&declaration, root).await;

    let outcome = f.record(&declaration, root, None, &qualified, None).await;

    assert!(
        matches!(outcome, Err(DomainError::Internal { .. })),
        "{outcome:?}"
    );
    assert!(f.state("gear", "pw", root).await.is_none());
}

#[tokio::test]
async fn a_subscription_to_a_retired_key_gets_its_state_from_the_first_change_after_reactivation() {
    // A retired key registers (the subscription is kept, its value absent for
    // the consumer); once the declaration is reactivated, the key's next change
    // writes the subscription's state like any other.
    use crate::domain::declaration::DeclarationStatus;
    let f = Fixture::new().await;
    let root = f.base.tree.root;
    let id = f
        .base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    {
        let conn = f.base.db.conn().expect("connection");
        DeclarationRepo
            .set_status(
                &conn,
                &AccessScope::allow_all(),
                id,
                DeclarationStatus::Retired,
                None,
            )
            .await
            .expect("retired");
    }
    f.subscribe("gear", "flag", root).await;
    assert!(
        f.state("gear", "flag", root).await.is_none(),
        "nothing to await while retired"
    );
    {
        let conn = f.base.db.conn().expect("connection");
        DeclarationRepo
            .set_status(
                &conn,
                &AccessScope::allow_all(),
                id,
                DeclarationStatus::Active,
                None,
            )
            .await
            .expect("reactivated");
    }
    let declaration = f.declaration("flag").await;
    let qualified = f.qualify(&declaration, root).await;
    f.base.set(id, root, json!(true)).await;

    f.record(&declaration, root, None, &qualified, None)
        .await
        .expect("recorded");

    let state = f
        .state("gear", "flag", root)
        .await
        .expect("the first change after reactivation writes the state");
    assert_eq!(state.expected_value, "true");
}

/// A secret `pw` set at the root, with the qualified subscriptions of a change
/// there re-read for every change: every write of a `secret`-trait key counts.
struct SecretKey {
    declaration: Declaration,
    plaintext: SecretString,
}

impl SecretKey {
    async fn declared(f: &Fixture) -> Self {
        let id = f
            .base
            .declare_typed("pw", scope_class::CASCADING, json!(""), SECRET, "secret")
            .await;
        f.base.set_secret(id, f.base.tree.root, "ref-1").await;
        Self {
            declaration: f.declaration("pw").await,
            plaintext: SecretString::from("hunter2"),
        }
    }

    /// One change of the key at the root under `change_ref`.
    async fn change(&self, f: &Fixture, change_ref: Uuid) -> Vec<SubscriberTenant> {
        let root = f.base.tree.root;
        let qualified = f.qualify(&self.declaration, root).await;
        f.record_as(
            change_ref,
            &self.declaration,
            root,
            None,
            &qualified,
            Some(&self.plaintext),
        )
        .await
        .expect("recorded")
    }
}

fn change_ref(n: u128) -> Uuid {
    Uuid::from_u128(0xc4a0 + n)
}

#[tokio::test]
async fn a_change_writes_one_awaiting_state_per_expected_instance() {
    // Level 1 row *A change writes one row per expected instance*.
    let f = Fixture::new().await;
    let (root, a) = (f.base.tree.root, f.base.tree.a);
    let id = f
        .base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let instances = [
        f.subscribe_instance("gear", 1, "flag", a).await,
        f.subscribe_instance("gear", 2, "flag", a).await,
        f.subscribe_instance("gear", 3, "flag", root).await,
        f.subscribe_instance("other", 4, "flag", a).await,
    ];
    let declaration = f.declaration("flag").await;
    let qualified = f.qualify(&declaration, root).await;
    f.base.set(id, root, json!(true)).await;

    let written = f
        .record_as(change_ref(1), &declaration, root, None, &qualified, None)
        .await
        .expect("recorded");

    let mut expected = [
        SubscriberTenant {
            subscriber: "gear".to_owned(),
            tenant: tenant_text(root),
        },
        SubscriberTenant {
            subscriber: "gear".to_owned(),
            tenant: tenant_text(a),
        },
        SubscriberTenant {
            subscriber: "other".to_owned(),
            tenant: tenant_text(a),
        },
    ];
    expected.sort_by(|x, y| (&x.subscriber, &x.tenant).cmp(&(&y.subscriber, &y.tenant)));
    assert_eq!(
        written, expected,
        "each pair once, however many of its instances hold a state"
    );
    for instance in &instances {
        let state = f.state_of(instance).await.expect("one state per instance");
        assert_eq!(state.status, ActivationStatus::Awaiting);
        assert_eq!(state.change_ref, change_ref(1), "the change's audit id");
        assert_eq!(state.expected_value, "true");
        assert_eq!(state.deliveries, 0);
        assert_eq!(state.silent_since, None);
    }
}

#[tokio::test]
async fn a_secret_change_expects_each_instance_its_tenants_hash() {
    let f = Fixture::new().await;
    let (root, a) = (f.base.tree.root, f.base.tree.a);
    let at_root = f.subscribe_instance("gear", 1, "pw", root).await;
    let at_a = [
        f.subscribe_instance("gear", 2, "pw", a).await,
        f.subscribe_instance("gear", 3, "pw", a).await,
    ];
    let key = SecretKey::declared(&f).await;

    key.change(&f, change_ref(1)).await;

    let hash = |tenant: Uuid| {
        ExpectedValue::of_secret(&key.declaration.key, &tenant_text(tenant), &key.plaintext)
            .expect("hashed")
            .into_string()
    };
    assert_ne!(hash(root), hash(a), "salted by the tenant");
    assert_eq!(
        f.state_of(&at_root).await.unwrap().expected_value,
        hash(root)
    );
    for instance in &at_a {
        assert_eq!(f.state_of(instance).await.unwrap().expected_value, hash(a));
    }
}

#[tokio::test]
async fn the_expected_set_leaves_out_silent_instances_and_takes_all_when_all_are_silent() {
    // Level 1 row *Expected set leaves out silent instances*.
    let f = Fixture::new().await;
    let root = f.base.tree.root;
    let one = f.subscribe_instance("gear", 1, "pw", root).await;
    let two = f.subscribe_instance("gear", 2, "pw", root).await;
    let lone = f.subscribe_instance("other", 3, "pw", root).await;
    let key = SecretKey::declared(&f).await;
    key.change(&f, change_ref(1)).await;
    f.silence(&two, change_ref(1)).await;
    f.silence(&lone, change_ref(1)).await;

    let written = key.change(&f, change_ref(2)).await;

    assert_eq!(written.len(), 2, "both subscribers are expected");
    assert_eq!(
        f.state_of(&one).await.map(|s| (s.change_ref, s.status)),
        Some((change_ref(2), ActivationStatus::Awaiting))
    );
    assert!(
        f.state_of(&two).await.is_none(),
        "left out while a sibling is expected: its state of the earlier change is deleted"
    );
    assert!(
        f.subscription_of(&two).await.silent_since.is_some(),
        "a change never touches a subscription"
    );
    assert_eq!(
        f.state_of(&lone).await.map(|s| (s.change_ref, s.status)),
        Some((change_ref(2), ActivationStatus::Awaiting)),
        "a subscriber is never expected by zero instances"
    );

    // Every instance of `gear` is silent now: the change expects all of them.
    f.silence(&one, change_ref(2)).await;
    key.change(&f, change_ref(3)).await;
    for instance in [&one, &two] {
        let state = f.state_of(instance).await.expect("expected again");
        assert_eq!(
            (state.change_ref, state.status),
            (change_ref(3), ActivationStatus::Awaiting)
        );
    }
}

#[tokio::test]
async fn a_newer_change_resets_a_silent_state_to_awaiting() {
    // Level 1 row *Overwrite on a newer change*, on a `silent` state.
    let f = Fixture::new().await;
    let root = f.base.tree.root;
    let only = f.subscribe_instance("gear", 1, "pw", root).await;
    let key = SecretKey::declared(&f).await;
    key.change(&f, change_ref(1)).await;
    f.silence(&only, change_ref(1)).await;
    let silent = f.state_of(&only).await.expect("kept");
    assert_eq!(silent.status, ActivationStatus::Silent);
    assert_eq!(silent.deliveries, 1);
    assert!(silent.silent_since.is_some() && silent.last_notified_at.is_some());

    key.change(&f, change_ref(2)).await;

    let state = f.state_of(&only).await.expect("overwritten");
    assert_eq!(state.status, ActivationStatus::Awaiting);
    assert_eq!(state.change_ref, change_ref(2));
    assert_eq!(state.deliveries, 0);
    assert_eq!(
        (
            state.silent_since,
            state.last_notified_at,
            state.applied_value,
            state.detail,
            state.reported_at
        ),
        (None, None, None, None, None)
    );
    assert!(state.changed_at >= silent.changed_at);
}
