//! `record_change` called as the commit calls it: over the rows the change left
//! and the pre-image it replaced, with the subscriptions that qualified.

use std::sync::Arc;

use secrecy::SecretString;
use serde_json::json;
use toolkit_db::secure::SecureDeleteExt;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::activation::{
    ActivationPublisher, ActivationRepository, ActivationState, Qualified, ReadGate,
    RecordedChange, SubscriberTenant, SubscriptionManager, tenant_text,
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
                subscriber,
                self.base.key(name).as_str(),
                &tenant_text(tenant),
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
                subscriber,
                self.base.key(name).as_str(),
                &tenant_text(tenant),
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
        let conn = self.base.db.conn().expect("connection");
        self.publisher
            .record_change(
                &conn,
                &RecordedChange {
                    declaration,
                    written,
                    root: self.base.tree.root,
                    pre_image,
                    change_ref: Uuid::new_v4(),
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
