//! The Subscription Manager over `SQLite`: the registration decisions, the
//! per-instance insert with its cap and re-arm, the unsubscribe, and which
//! subscriptions a change qualifies.

use std::sync::Arc;

use serde_json::json;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::domain::access::{AccessRepository, RestrictionDraft, TenantAccess};
use crate::domain::activation::publisher::{StateRow, StateWrite};
use crate::domain::activation::testing::{
    LogBuf, ONE_INSTANCE, RecordingNotifyMetrics, capture, logged, one,
};
use crate::domain::activation::{
    ActivationRepository, ActivationState, ActivationStatus, Admission, InstanceKey,
    MAX_INSTANCES_PER_SUBSCRIPTION, ReadGate, Registration, Subscription, SubscriptionManager,
    subscriber_of, tenant_text, unsubscribing,
};
use crate::domain::category::DomainVisibility;
use crate::domain::declaration::{DeclarationRepository, DeclarationStatus};
use crate::domain::error::DomainError;
use crate::domain::resolution::scope_class;
use crate::infra::storage::access_repo::AccessRepo;
use crate::infra::storage::activation_repo::ActivationRepo;
use crate::infra::storage::declaration_repo::DeclarationRepo;
use crate::infra::storage::value_repo::ValueRepo;
use crate::test_support::{FakeReadGate, ReadAnswer, ResolutionHarness};

type Manager = SubscriptionManager<DeclarationRepo, ValueRepo, AccessRepo, ActivationRepo>;

struct Fixture {
    base: ResolutionHarness,
    gate: Arc<FakeReadGate>,
    metrics: Arc<RecordingNotifyMetrics>,
    manager: Manager,
}

impl Fixture {
    async fn new() -> Self {
        let base = ResolutionHarness::new().await;
        let gate = FakeReadGate::allowing();
        let metrics = Arc::new(RecordingNotifyMetrics::default());
        let manager = SubscriptionManager::new(
            Arc::clone(&base.resolver),
            ActivationRepo,
            Arc::clone(&gate) as Arc<dyn ReadGate>,
            Arc::clone(&metrics) as Arc<dyn crate::domain::activation::NotifyMetrics>,
        );
        Self {
            base,
            gate,
            metrics,
            manager,
        }
    }

    fn ctx(tenant: Uuid) -> SecurityContext {
        crate::test_support::service_context_for(tenant)
    }

    async fn admit(
        &self,
        caller: Uuid,
        name: &str,
        tenant: Uuid,
    ) -> Result<Admission, DomainError> {
        self.admit_instance(caller, name, tenant, Some(ONE_INSTANCE), None)
            .await
    }

    async fn admit_instance(
        &self,
        caller: Uuid,
        name: &str,
        tenant: Uuid,
        instance_id: Option<Uuid>,
        label: Option<&str>,
    ) -> Result<Admission, DomainError> {
        self.admit_as(&Self::ctx(caller), name, tenant, instance_id, label)
            .await
    }

    /// Admit under exactly this security context.
    async fn admit_as(
        &self,
        ctx: &SecurityContext,
        name: &str,
        tenant: Uuid,
        instance_id: Option<Uuid>,
        label: Option<&str>,
    ) -> Result<Admission, DomainError> {
        let conn = self.base.db.conn().expect("connection");
        self.manager
            .admit(&conn, ctx, &self.base.key(name), tenant, instance_id, label)
            .await
    }

    /// The registering transaction and its conclusion, as the service runs
    /// them around the lock.
    async fn register(&self, admission: &Admission) -> Result<Subscription, DomainError> {
        let conn = self.base.db.conn().expect("connection");
        let registration = self.manager.register_in(&conn, admission).await?;
        self.manager.conclude(registration)
    }

    /// Admit and register `instance_id` of the root caller for `name` at the
    /// root.
    async fn register_instance(
        &self,
        name: &str,
        instance_id: Uuid,
        label: Option<&str>,
    ) -> Result<Subscription, DomainError> {
        let root = self.base.tree.root;
        let admission = self
            .admit_instance(root, name, root, Some(instance_id), label)
            .await
            .expect("admitted");
        self.register(&admission).await
    }

    /// The identity of the root caller's `instance_id` for `name` at the root.
    fn instance(&self, name: &str, instance_id: Uuid) -> InstanceKey {
        let root = self.base.tree.root;
        InstanceKey {
            subscriber: subscriber_of(&Self::ctx(root)),
            instance_id,
            key: self.base.key(name).as_str().to_owned(),
            tenant: tenant_text(root),
        }
    }

    /// The states of one change for these subscriptions, as a change writes
    /// them; returns the change's `change_ref`.
    async fn change(&self, name: &str, subscriptions: &[&Subscription]) -> Uuid {
        let conn = self.base.db.conn().expect("connection");
        let change_ref = Uuid::now_v7();
        ActivationRepo
            .write_states(
                &conn,
                &StateWrite {
                    key: self.base.key(name).as_str().to_owned(),
                    change_ref,
                    rows: subscriptions
                        .iter()
                        .map(|subscription| StateRow {
                            subscription_id: subscription.id,
                            tenant: subscription.tenant.clone(),
                            expected_value: "true".to_owned(),
                        })
                        .collect(),
                },
            )
            .await
            .expect("states written");
        change_ref
    }

    /// Call the instance silent on `change_ref`, its state and its
    /// subscription, as the delivery loop does.
    async fn call_silent(&self, instance: &InstanceKey, change_ref: Uuid) {
        let conn = self.base.db.conn().expect("connection");
        let at = time::OffsetDateTime::now_utc();
        assert!(
            ActivationRepo
                .call_silent(&conn, instance, change_ref, at)
                .await
                .expect("called silent")
        );
        assert!(
            ActivationRepo
                .mark_silent(&conn, instance, at)
                .await
                .expect("marked")
        );
    }

    async fn state(&self, instance: &InstanceKey) -> Option<ActivationState> {
        let conn = self.base.db.conn().expect("connection");
        ActivationRepo.state(&conn, instance).await.expect("state")
    }

    async fn subscription(&self, instance: &InstanceKey) -> Option<Subscription> {
        let conn = self.base.db.conn().expect("connection");
        ActivationRepo.find(&conn, instance).await.expect("lookup")
    }

    async fn registrations(&self, name: &str) -> u64 {
        let conn = self.base.db.conn().expect("connection");
        let root = self.base.tree.root;
        ActivationRepo
            .registrations(
                &conn,
                &subscriber_of(&Self::ctx(root)),
                self.base.key(name).as_str(),
                &tenant_text(root),
            )
            .await
            .expect("count")
    }

    /// Bind a declaration to an administrative domain.
    async fn bind_domain(&self, id: Uuid, domain: &str) {
        use crate::domain::declaration::DeclarationMetadata;
        let conn = self.base.db.conn().expect("connection");
        DeclarationRepo
            .update_metadata(
                &conn,
                &AccessScope::allow_all(),
                id,
                DeclarationMetadata {
                    mode: "standard".to_owned(),
                    display_name: None,
                    description: None,
                    domain_affinity: Some(domain.to_owned()),
                    licence_feature: None,
                    data_classification: "public".to_owned(),
                    requires_step_up: false,
                    anonymous_exposable: false,
                },
                None,
                true,
            )
            .await
            .expect("metadata");
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

    async fn qualified(&self, name: &str, written: Uuid) -> Result<Vec<String>, DomainError> {
        let conn = self.base.db.conn().expect("connection");
        let declaration = DeclarationRepo
            .find_by_key(
                &conn,
                &AccessScope::allow_all(),
                self.base.key(name).as_str(),
            )
            .await
            .expect("lookup")
            .expect("declared");
        let mut tenants: Vec<String> = self
            .manager
            .resolve_subscribers(&conn, &declaration, written)
            .await?
            .into_iter()
            .map(|q| q.subscription.tenant)
            .collect();
        tenants.sort();
        Ok(tenants)
    }

    async fn state_of(
        &self,
        subscriber: &str,
        name: &str,
        tenant: Uuid,
    ) -> Option<ActivationState> {
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

    /// An activation state a change would have written.
    async fn seed_state(&self, subscriber: &str, name: &str, tenant: Uuid) {
        let conn = self.base.db.conn().expect("connection");
        let key = self.base.key(name);
        let subscription = ActivationRepo
            .find(&conn, &one(subscriber, key.as_str(), &tenant_text(tenant)))
            .await
            .expect("lookup")
            .expect("registered");
        ActivationRepo
            .write_states(
                &conn,
                &StateWrite {
                    key: key.as_str().to_owned(),
                    change_ref: Uuid::new_v4(),
                    rows: vec![StateRow {
                        subscription_id: subscription.id,
                        tenant: tenant_text(tenant),
                        expected_value: "true".to_owned(),
                    }],
                },
            )
            .await
            .expect("state written");
    }
}

#[tokio::test]
async fn a_denied_read_decision_refuses_before_the_declaration_is_looked_at() {
    let f = Fixture::new().await;
    f.gate.answer(ReadAnswer::Deny);
    // The key is not even declared: the denial is the answer, not a 404.
    let outcome = f
        .admit(f.base.tree.root, "undeclared", f.base.tree.root)
        .await;
    assert!(
        matches!(outcome, Err(DomainError::Unauthorized { .. })),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn a_decision_that_cannot_be_had_is_unavailable_not_a_denial() {
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    f.gate.answer(ReadAnswer::Down);
    let outcome = f.admit(f.base.tree.root, "flag", f.base.tree.root).await;
    assert!(
        matches!(outcome, Err(DomainError::Unavailable { .. })),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn a_key_with_no_declaration_is_not_found_and_a_retired_one_is_admitted() {
    let f = Fixture::new().await;
    let root = f.base.tree.root;
    assert!(matches!(
        f.admit(root, "undeclared", root).await,
        Err(DomainError::NotFound { .. })
    ));

    let id = f
        .base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
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
    let admission = f
        .admit(root, "flag", root)
        .await
        .expect("a retired key registers");
    assert_eq!(admission.key, f.base.key("flag").as_str());
}

#[tokio::test]
async fn a_key_hidden_at_the_tenant_or_outside_the_callers_domains_is_not_found() {
    let f = Fixture::new().await;
    let (root, a) = (f.base.tree.root, f.base.tree.a);
    let id = f
        .base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let conn = f.base.db.conn().expect("connection");
    AccessRepo
        .upsert(
            &conn,
            &AccessScope::allow_all(),
            RestrictionDraft {
                declaration_id: id,
                tenant_id: a,
                access: TenantAccess::Hidden,
                set_by: "admin".to_owned(),
                set_by_tenant: None,
            },
            None,
        )
        .await
        .expect("hidden at a");
    assert!(matches!(
        f.admit(root, "flag", a).await,
        Err(DomainError::NotFound { .. })
    ));
    f.admit(root, "flag", root)
        .await
        .expect("visible where it is not hidden");

    // A declaration bound to a domain the caller's decision does not name.
    let bound = f
        .base
        .declare("bound", scope_class::CASCADING, json!(false))
        .await;
    f.bind_domain(bound, "tenants").await;
    f.gate
        .answer(ReadAnswer::Allow(DomainVisibility::Restricted(vec![
            "billing".to_owned(),
        ])));
    assert!(matches!(
        f.admit(root, "bound", root).await,
        Err(DomainError::NotFound { .. })
    ));
}

#[tokio::test]
async fn a_tenant_outside_the_callers_subtree_is_not_found() {
    let f = Fixture::new().await;
    let tree = &f.base.tree;
    let (own, below, sibling, sealed) = (tree.a, tree.b, tree.c, tree.s);
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    // `a` serves its own subtree...
    f.admit(own, "flag", own).await.expect("its own tenant");
    f.admit(own, "flag", below).await.expect("a descendant");
    // ...not a sibling branch, nor a standalone tenant sealed from it.
    assert!(matches!(
        f.admit(own, "flag", sibling).await,
        Err(DomainError::NotFound { .. })
    ));
    assert!(matches!(
        f.admit(own, "flag", sealed).await,
        Err(DomainError::NotFound { .. })
    ));
}

#[tokio::test]
async fn the_subscriber_comes_from_the_context_and_the_tenant_is_a_bare_lowercase_uuid() {
    let f = Fixture::new().await;
    let root = f.base.tree.root;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let admission = f.admit(root, "flag", root).await.expect("admitted");
    assert_eq!(admission.subscriber, subscriber_of(&Fixture::ctx(root)));
    assert_eq!(admission.tenant, root.as_hyphenated().to_string());
    assert_eq!(admission.tenant, admission.tenant.to_lowercase());
}

/// A context of `tenant` whose subject carries `subject_type`, or none.
fn labelled(tenant: Uuid, subject_type: Option<&str>) -> SecurityContext {
    let mut builder = SecurityContext::builder()
        .subject_id(Uuid::new_v5(&Uuid::NAMESPACE_X500, tenant.as_bytes()))
        .subject_tenant_id(tenant);
    if let Some(label) = subject_type {
        builder = builder.subject_type(label);
    }
    builder.build().expect("context")
}

#[tokio::test]
async fn a_person_is_refused_before_anything_is_decided_or_read() {
    let f = Fixture::new().await;
    let root = f.base.tree.root;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    // Both spellings of an interactive subject the gear recognises.
    for person in [
        crate::test_support::context_for(root),
        labelled(root, Some("user")),
    ] {
        let outcome = f
            .admit_as(&person, "flag", root, Some(ONE_INSTANCE), None)
            .await;
        assert!(
            matches!(outcome, Err(DomainError::Unauthorized { .. })),
            "{outcome:?}"
        );
        // Even an undeclared key answers the same refusal: no declaration was
        // looked at, and no call without an instance got that far either.
        let undeclared = f.admit_as(&person, "undeclared", root, None, None).await;
        assert!(
            matches!(undeclared, Err(DomainError::Unauthorized { .. })),
            "{undeclared:?}"
        );
        let unsubscribe = unsubscribing(&person, &f.base.key("flag"), root, Some(ONE_INSTANCE));
        assert!(
            matches!(unsubscribe, Err(DomainError::Unauthorized { .. })),
            "{unsubscribe:?}"
        );
    }
    assert_eq!(
        f.gate.asked.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the read decision was never asked"
    );
    let conn = f.base.db.conn().expect("connection");
    let rows = ActivationRepo
        .of_key(&conn, f.base.key("flag").as_str())
        .await
        .expect("read");
    assert!(rows.is_empty(), "nothing registered: {rows:?}");
}

#[tokio::test]
async fn a_service_subject_and_an_unlabelled_one_are_admitted() {
    let f = Fixture::new().await;
    let root = f.base.tree.root;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    for caller in [
        crate::test_support::service_context_for(root),
        // A gear that builds its context without a subject type.
        labelled(root, None),
    ] {
        let admission = f
            .admit_as(&caller, "flag", root, Some(ONE_INSTANCE), None)
            .await
            .expect("admitted");
        assert_eq!(admission.subscriber, subscriber_of(&caller));
        let unsubscribe = unsubscribing(&caller, &f.base.key("flag"), root, Some(ONE_INSTANCE))
            .expect("an unsubscribe is taken");
        assert_eq!(unsubscribe.subscriber, subscriber_of(&caller));
    }
}

#[tokio::test]
async fn registering_the_same_triple_twice_keeps_one_row() {
    let f = Fixture::new().await;
    let root = f.base.tree.root;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let admission = f.admit(root, "flag", root).await.expect("admitted");
    let conn = f.base.db.conn().expect("connection");
    let first = f.register(&admission).await.expect("first");
    let second = f.register(&admission).await.expect("second");
    assert_eq!(first.id, second.id, "the existing row is kept");
    let all = ActivationRepo
        .of_key(&conn, f.base.key("flag").as_str())
        .await
        .expect("read");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].tenant, admission.tenant);
}

#[tokio::test]
async fn an_unsubscribe_deletes_the_subscription_and_its_state_and_only_the_callers() {
    let f = Fixture::new().await;
    let (root, a) = (f.base.tree.root, f.base.tree.a);
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    f.subscribe("gear", "flag", root).await;
    f.subscribe("other", "flag", root).await;
    f.subscribe("gear", "flag", a).await;
    for (subscriber, tenant) in [("gear", root), ("other", root), ("gear", a)] {
        f.seed_state(subscriber, "flag", tenant).await;
    }
    let key = f.base.key("flag");
    let conn = f.base.db.conn().expect("connection");

    f.manager
        .unsubscribe_in(&conn, &one("gear", key.as_str(), &tenant_text(root)))
        .await
        .expect("unsubscribed");

    assert!(
        f.state_of("gear", "flag", root).await.is_none(),
        "its state goes with it"
    );
    assert!(
        f.state_of("other", "flag", root).await.is_some(),
        "another subscriber's stays"
    );
    assert!(
        f.state_of("gear", "flag", a).await.is_some(),
        "another tenant's stays"
    );
    assert!(
        ActivationRepo
            .find(&conn, &one("gear", key.as_str(), &tenant_text(root)))
            .await
            .expect("lookup")
            .is_none()
    );

    // A key with no subscription of the caller's: not found, nothing deleted.
    let none = f
        .manager
        .unsubscribe_in(&conn, &one("gear", key.as_str(), &tenant_text(root)))
        .await;
    assert!(
        matches!(none, Err(DomainError::NotFound { .. })),
        "{none:?}"
    );
    let foreign = f
        .manager
        .unsubscribe_in(&conn, &one("stranger", key.as_str(), &tenant_text(a)))
        .await;
    assert!(matches!(foreign, Err(DomainError::NotFound { .. })));
    assert!(
        f.state_of("gear", "flag", a).await.is_some(),
        "nothing of another's deleted"
    );
}

#[tokio::test]
async fn subscriptions_qualify_by_scope_class() {
    let f = Fixture::new().await;
    let (root, a, b, c) = (
        f.base.tree.root,
        f.base.tree.a,
        f.base.tree.b,
        f.base.tree.c,
    );
    f.base
        .declare("local", scope_class::LOCAL, json!(false))
        .await;
    f.base
        .declare("global", scope_class::GLOBAL, json!(false))
        .await;
    f.base
        .declare("cascading", scope_class::CASCADING, json!(false))
        .await;
    for name in ["local", "global", "cascading"] {
        for tenant in [root, a, b, c] {
            f.subscribe("gear", name, tenant).await;
        }
    }
    let texts = |tenants: &[Uuid]| -> Vec<String> {
        let mut texts: Vec<String> = tenants.iter().map(|t| tenant_text(*t)).collect();
        texts.sort();
        texts
    };

    // `local`: the written tenant only.
    assert_eq!(
        f.qualified("local", a).await.expect("qualified"),
        texts(&[a])
    );
    // `global`: every subscription of the key, whatever was written.
    assert_eq!(
        f.qualified("global", root).await.expect("qualified"),
        texts(&[root, a, b, c])
    );
    // `cascading`: the written scope and its descendants, by each tenant's own
    // chain — `b` is below `a`, `c` is a sibling branch.
    assert_eq!(
        f.qualified("cascading", a).await.expect("qualified"),
        texts(&[a, b])
    );
    assert_eq!(
        f.qualified("cascading", root).await.expect("qualified"),
        texts(&[root, a, b, c])
    );
    assert_eq!(
        f.qualified("cascading", c).await.expect("qualified"),
        texts(&[c])
    );
}

#[tokio::test]
async fn a_cascading_qualification_keeps_each_tenants_chain_and_fails_closed_without_one() {
    let f = Fixture::new().await;
    let (root, a, b) = (f.base.tree.root, f.base.tree.a, f.base.tree.b);
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    f.subscribe("gear", "flag", b).await;
    f.subscribe("other", "flag", b).await;
    let conn = f.base.db.conn().expect("connection");
    let declaration = DeclarationRepo
        .find_by_key(
            &conn,
            &AccessScope::allow_all(),
            f.base.key("flag").as_str(),
        )
        .await
        .expect("lookup")
        .expect("declared");
    let qualified = f
        .manager
        .resolve_subscribers(&conn, &declaration, a)
        .await
        .expect("qualified");
    assert_eq!(qualified.len(), 2);
    assert!(qualified.iter().all(|q| q.chain == vec![root, a, b]));
    assert_eq!(
        f.base
            .hierarchy
            .chain_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "one chain per distinct subscription tenant"
    );

    f.base.hierarchy.set_unavailable(true);
    let outcome = f.manager.resolve_subscribers(&conn, &declaration, a).await;
    assert!(
        matches!(outcome, Err(DomainError::Unavailable { .. })),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn a_key_nobody_subscribed_to_qualifies_nobody_and_asks_no_resolver() {
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    assert!(
        f.qualified("flag", f.base.tree.root)
            .await
            .expect("qualified")
            .is_empty()
    );
    assert_eq!(
        f.base
            .hierarchy
            .chain_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn a_subscription_whose_tenant_does_not_exist_is_skipped_with_one_warn_not_a_failed_write() {
    let logs = LogBuf::default();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .finish(),
    );
    let f = Fixture::new().await;
    let (root, b) = (f.base.tree.root, f.base.tree.b);
    let ghost = Uuid::new_v4();
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    f.subscribe("gear", "flag", b).await;
    f.subscribe("ghost-gear", "flag", ghost).await;

    let qualified = f.qualified("flag", root).await.expect("qualified");

    assert_eq!(
        qualified,
        vec![tenant_text(b)],
        "the deleted tenant's is out"
    );
    let text = logs.text();
    let warns: Vec<&str> = text.lines().filter(|l| l.contains("WARN")).collect();
    assert_eq!(warns.len(), 1, "{text}");
    for named in [
        "ghost-gear",
        f.base.key("flag").as_str(),
        &ghost.to_string(),
    ] {
        assert!(warns[0].contains(named), "names `{named}`: {text}");
    }
}

#[tokio::test]
async fn a_resolver_that_cannot_answer_for_one_of_several_tenants_fails_the_write_closed() {
    let f = Fixture::new().await;
    let (root, a, b) = (f.base.tree.root, f.base.tree.a, f.base.tree.b);
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    f.subscribe("gear", "flag", a).await;
    f.subscribe("gear", "flag", b).await;
    f.subscribe("ghost-gear", "flag", Uuid::new_v4()).await;
    f.base.hierarchy.set_unavailable(true);

    let outcome = f.qualified("flag", root).await;

    assert!(
        matches!(outcome, Err(DomainError::Unavailable { .. })),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn many_distinct_subscription_tenants_are_each_resolved_once() {
    let f = Fixture::new().await;
    let root = f.base.tree.root;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let tenants: Vec<Uuid> = (0..20).map(|_| Uuid::new_v4()).collect();
    for tenant in &tenants {
        f.base.hierarchy.add_tenant(*tenant, root);
        f.subscribe("gear", "flag", *tenant).await;
        f.subscribe("other-gear", "flag", *tenant).await;
    }

    let qualified = f.qualified("flag", root).await.expect("qualified");

    assert_eq!(qualified.len(), 40);
    assert_eq!(f.base.hierarchy.chain_calls(), 20, "one chain per tenant");
    const { assert!(crate::domain::activation::subscriptions::CHAIN_CONCURRENCY <= 8) };
}

#[tokio::test]
async fn a_registration_after_the_declaration_was_purged_is_not_found_and_inserts_nothing() {
    let f = Fixture::new().await;
    let id = f
        .base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let root = f.base.tree.root;
    let admission = f.admit(root, "flag", root).await.expect("admitted");
    assert_eq!(admission.declaration_id, id);
    // The purge lands between the admission and the lock.
    let conn = f.base.db.conn().expect("connection");
    let declaration = f.base.load_declaration(id).await;
    DeclarationRepo
        .delete(&conn, &AccessScope::allow_all(), id, declaration.updated_at)
        .await
        .expect("purged");

    let outcome = f.manager.register_in(&conn, &admission).await;

    assert!(
        matches!(
            outcome,
            Err(DomainError::NotFound {
                resource: "declaration"
            })
        ),
        "{outcome:?}"
    );
    assert!(
        ActivationRepo
            .of_key(&conn, f.base.key("flag").as_str())
            .await
            .expect("read")
            .is_empty()
    );
}

#[tokio::test]
async fn a_call_without_an_instance_id_is_refused_as_invalid_input_before_any_decision() {
    let f = Fixture::new().await;
    let root = f.base.tree.root;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    // Even a denying decision point is not asked: the call is malformed first.
    f.gate.answer(ReadAnswer::Deny);

    let registration = f
        .admit_instance(root, "flag", root, None, Some("host"))
        .await;
    let unsubscribe = unsubscribing(&Fixture::ctx(root), &f.base.key("flag"), root, None);

    for outcome in [registration.map(|_| ()), unsubscribe.map(|_| ())] {
        assert!(
            matches!(
                &outcome,
                Err(DomainError::Validation { field, code, .. })
                    if field == "instance_id" && *code == crate::field::INSTANCE_ID_REQUIRED
            ),
            "{outcome:?}"
        );
    }
    assert_eq!(f.registrations("flag").await, 0, "nothing registered");
}

#[tokio::test]
async fn each_instance_registers_its_own_subscription_and_a_repeat_refreshes_its_label() {
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let (one_id, two_id) = (Uuid::now_v7(), Uuid::now_v7());

    let one_first = f
        .register_instance("flag", one_id, Some("host-a"))
        .await
        .expect("first instance");
    let two = f
        .register_instance("flag", two_id, None)
        .await
        .expect("second instance");
    assert_ne!(one_first.id, two.id, "one subscription per instance");
    assert_eq!((one_first.instance_id, two.instance_id), (one_id, two_id));
    assert_eq!(f.registrations("flag").await, 2);

    let one_again = f
        .register_instance("flag", one_id, Some("host-b"))
        .await
        .expect("repeat");
    assert_eq!(one_again.id, one_first.id, "the row is kept");
    assert_eq!(one_again.created_at, one_first.created_at);
    assert_eq!(one_again.instance_label.as_deref(), Some("host-b"));
    assert_eq!(
        f.subscription(&f.instance("flag", one_id))
            .await
            .expect("registered")
            .instance_label
            .as_deref(),
        Some("host-b"),
        "the label is refreshed in the row"
    );
    assert_eq!(f.registrations("flag").await, 2);
    assert!(
        f.metrics.evictions.lock().expect("metrics").is_empty(),
        "no cap decision under the cap"
    );
}

#[tokio::test]
async fn a_label_longer_than_128_characters_is_truncated_by_characters() {
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    // Two-byte characters: a cut by bytes would split one and keep 64.
    let long = "\u{e9}".repeat(200);

    let subscription = f
        .register_instance("flag", Uuid::now_v7(), Some(&long))
        .await
        .expect("registered");

    let stored = subscription.instance_label.expect("labelled");
    assert_eq!(stored.chars().count(), 128);
    assert_eq!(stored, "\u{e9}".repeat(128));
}

#[tokio::test]
async fn a_registration_creates_no_state_so_an_instance_after_a_change_is_outside_it() {
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let early = f
        .register_instance("flag", Uuid::now_v7(), None)
        .await
        .expect("early");
    f.change("flag", &[&early]).await;

    let late_id = Uuid::now_v7();
    f.register_instance("flag", late_id, None)
        .await
        .expect("late");
    // A repeat registration of the late instance creates none either.
    f.register_instance("flag", late_id, None)
        .await
        .expect("repeat");

    assert!(f.state(&f.instance("flag", late_id)).await.is_none());
    assert!(
        f.state(&early.instance_key()).await.is_some(),
        "the early instance's state is untouched"
    );
}

#[tokio::test]
async fn a_repeat_registration_re_arms_its_silent_state_and_ends_its_silence() {
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let quiet_id = Uuid::now_v7();
    let quiet = f
        .register_instance("flag", quiet_id, None)
        .await
        .expect("quiet");
    let sibling = f
        .register_instance("flag", Uuid::now_v7(), None)
        .await
        .expect("sibling");
    let change_ref = f.change("flag", &[&quiet, &sibling]).await;
    let instance = f.instance("flag", quiet_id);
    f.call_silent(&instance, change_ref).await;

    let again = f
        .register_instance("flag", quiet_id, None)
        .await
        .expect("re-entry");

    assert_eq!(again.id, quiet.id);
    assert!(again.silent_since.is_none(), "the answer shows it cleared");
    assert!(
        f.subscription(&instance)
            .await
            .expect("registered")
            .silent_since
            .is_none(),
        "the subscription's silence ended"
    );
    let state = f.state(&instance).await.expect("state kept");
    assert_eq!(state.status, ActivationStatus::Awaiting, "re-armed");
    assert_eq!(state.change_ref, change_ref, "of the current change");
    assert_eq!(state.deliveries, 0);
    assert!(state.silent_since.is_none());
    assert!(state.last_notified_at.is_none());
    let untouched = f.state(&sibling.instance_key()).await.expect("sibling");
    assert_eq!(untouched.status, ActivationStatus::Awaiting);
    assert_eq!(untouched.change_ref, change_ref);
}

#[tokio::test]
async fn a_repeat_registration_leaves_a_state_that_is_not_silent_alone() {
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let id = Uuid::now_v7();
    let subscription = f.register_instance("flag", id, None).await.expect("one");
    f.change("flag", &[&subscription]).await;
    let before = f.state(&f.instance("flag", id)).await.expect("state");

    f.register_instance("flag", id, None).await.expect("repeat");

    assert_eq!(f.state(&f.instance("flag", id)).await, Some(before));
}

#[tokio::test]
async fn an_instance_left_out_as_silent_ends_its_silence_and_gets_no_state() {
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let quiet_id = Uuid::now_v7();
    let quiet = f
        .register_instance("flag", quiet_id, None)
        .await
        .expect("quiet");
    let sibling = f
        .register_instance("flag", Uuid::now_v7(), None)
        .await
        .expect("sibling");
    let first = f.change("flag", &[&quiet, &sibling]).await;
    let instance = f.instance("flag", quiet_id);
    f.call_silent(&instance, first).await;
    // The next change leaves the silent instance out: its state is deleted.
    f.change("flag", &[&quiet, &sibling]).await;
    assert!(f.state(&instance).await.is_none(), "left out");

    f.register_instance("flag", quiet_id, None)
        .await
        .expect("re-entry");

    assert!(
        f.subscription(&instance)
            .await
            .expect("registered")
            .silent_since
            .is_none()
    );
    assert!(
        f.state(&instance).await.is_none(),
        "expected from the next change, not this one"
    );
}

/// Register `count` instances of the root caller for `name`, oldest first.
async fn fill(f: &Fixture, name: &str, count: u64) -> Vec<Subscription> {
    let mut registered = Vec::new();
    for _ in 0..count {
        registered.push(
            f.register_instance(name, Uuid::now_v7(), None)
                .await
                .expect("registered"),
        );
    }
    registered
}

#[tokio::test]
async fn a_new_instance_past_the_cap_evicts_the_oldest_silent_or_stateless_one() {
    let (logs, _guard) = capture();
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let held = fill(&f, "flag", MAX_INSTANCES_PER_SUBSCRIPTION).await;
    // Every instance but #5 and #9 is expected by the current change; #2 is
    // then called silent. Evictable, oldest first: #2, #5, #9.
    let expected: Vec<&Subscription> = held
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 5 && *i != 9)
        .map(|(_, s)| s)
        .collect();
    let change_ref = f.change("flag", &expected).await;
    f.call_silent(&held[2].instance_key(), change_ref).await;

    for victim in [2_usize, 5, 9] {
        let newcomer = Uuid::now_v7();
        let registered = f
            .register_instance("flag", newcomer, None)
            .await
            .expect("registered past the cap");
        assert_eq!(registered.instance_id, newcomer);
        let evicted = held[victim].instance_key();
        assert!(
            f.subscription(&evicted).await.is_none(),
            "#{victim} evicted"
        );
        assert!(f.state(&evicted).await.is_none(), "with its state");
        assert_eq!(
            f.registrations("flag").await,
            MAX_INSTANCES_PER_SUBSCRIPTION
        );
    }
    assert_eq!(
        *f.metrics.evictions.lock().expect("metrics"),
        vec!["evicted"; 3]
    );
    let text = logged(&logs);
    let warns: Vec<&str> = text.lines().filter(|l| l.contains("WARN")).collect();
    assert_eq!(warns.len(), 3, "{text}");
    for (warn, victim) in warns.iter().zip([2_usize, 5, 9]) {
        assert!(
            warn.contains(&held[victim].instance_id.to_string()),
            "names the evicted instance: {warn}"
        );
        assert!(warn.contains(f.base.key("flag").as_str()), "{warn}");
    }

    // A repeat registration of a registered instance at the cap evicts nothing.
    f.register_instance("flag", held[0].instance_id, None)
        .await
        .expect("repeat at the cap");
    assert_eq!(
        f.registrations("flag").await,
        MAX_INSTANCES_PER_SUBSCRIPTION
    );
    assert_eq!(f.metrics.evictions.lock().expect("metrics").len(), 3);
    for kept in [0_usize, 1, 3, 63] {
        assert!(f.subscription(&held[kept].instance_key()).await.is_some());
    }
}

#[tokio::test]
async fn a_new_instance_past_the_cap_with_none_evictable_is_unavailable_and_registers_nothing() {
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let held = fill(&f, "flag", MAX_INSTANCES_PER_SUBSCRIPTION).await;
    let every: Vec<&Subscription> = held.iter().collect();
    f.change("flag", &every).await;
    let newcomer = Uuid::now_v7();
    let root = f.base.tree.root;
    let admission = f
        .admit_instance(root, "flag", root, Some(newcomer), None)
        .await
        .expect("admitted");

    let conn = f.base.db.conn().expect("connection");
    let registration = f
        .manager
        .register_in(&conn, &admission)
        .await
        .expect("the transaction ends");
    assert_eq!(registration, Registration::Refused);
    let outcome = f.manager.conclude(registration);

    assert!(
        matches!(outcome, Err(DomainError::Unavailable { .. })),
        "{outcome:?}"
    );
    assert!(
        f.subscription(&f.instance("flag", newcomer))
            .await
            .is_none()
    );
    assert_eq!(
        f.registrations("flag").await,
        MAX_INSTANCES_PER_SUBSCRIPTION
    );
    for subscription in &held {
        assert!(
            f.state(&subscription.instance_key()).await.is_some(),
            "nothing evicted"
        );
    }
    assert_eq!(
        *f.metrics.evictions.lock().expect("metrics"),
        vec!["refused"]
    );
}

#[tokio::test]
async fn the_cap_counts_per_subscriber_key_and_tenant() {
    let f = Fixture::new().await;
    let (root, a) = (f.base.tree.root, f.base.tree.a);
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let held = fill(&f, "flag", MAX_INSTANCES_PER_SUBSCRIPTION).await;
    let every: Vec<&Subscription> = held.iter().collect();
    f.change("flag", &every).await;

    // The same subscriber at another tenant is under no cap of the root's.
    let elsewhere = f
        .admit_instance(root, "flag", a, Some(Uuid::now_v7()), None)
        .await
        .expect("admitted");
    f.register(&elsewhere).await.expect("another tenant");
    assert!(f.metrics.evictions.lock().expect("metrics").is_empty());
}

#[tokio::test]
async fn an_unsubscribe_deletes_its_own_instance_and_leaves_a_sibling_untouched() {
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let gone = f
        .register_instance("flag", Uuid::now_v7(), None)
        .await
        .expect("one");
    let sibling = f
        .register_instance("flag", Uuid::now_v7(), None)
        .await
        .expect("two");
    f.change("flag", &[&gone, &sibling]).await;
    let sibling_state = f.state(&sibling.instance_key()).await;
    let conn = f.base.db.conn().expect("connection");

    f.manager
        .unsubscribe_in(&conn, &gone.instance_key())
        .await
        .expect("unsubscribed");

    assert!(f.subscription(&gone.instance_key()).await.is_none());
    assert!(f.state(&gone.instance_key()).await.is_none());
    assert_eq!(
        f.subscription(&sibling.instance_key()).await,
        Some(sibling.clone())
    );
    assert_eq!(f.state(&sibling.instance_key()).await, sibling_state);
}

#[tokio::test]
async fn an_unsubscribe_of_an_instance_the_subscriber_never_registered_is_not_found() {
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let held = f
        .register_instance("flag", Uuid::now_v7(), None)
        .await
        .expect("registered");
    f.change("flag", &[&held]).await;
    let conn = f.base.db.conn().expect("connection");

    let stranger = f.instance("flag", Uuid::now_v7());
    let outcome = f.manager.unsubscribe_in(&conn, &stranger).await;
    assert!(
        matches!(outcome, Err(DomainError::NotFound { .. })),
        "{outcome:?}"
    );
    // Another subscriber naming the registered instance's id finds nothing.
    let foreign = InstanceKey {
        subscriber: "stranger".to_owned(),
        ..held.instance_key()
    };
    let outcome = f.manager.unsubscribe_in(&conn, &foreign).await;
    assert!(
        matches!(outcome, Err(DomainError::NotFound { .. })),
        "{outcome:?}"
    );
    assert!(f.subscription(&held.instance_key()).await.is_some());
    assert!(
        f.state(&held.instance_key()).await.is_some(),
        "nothing deleted"
    );
}

#[tokio::test]
async fn every_instance_of_a_subscriber_qualifies_on_its_own() {
    let f = Fixture::new().await;
    f.base
        .declare("flag", scope_class::LOCAL, json!(false))
        .await;
    let root = f.base.tree.root;
    for _ in 0..3 {
        f.register_instance("flag", Uuid::now_v7(), None)
            .await
            .expect("registered");
    }

    let qualified = f.qualified("flag", root).await.expect("qualified");

    assert_eq!(qualified, vec![tenant_text(root); 3]);
}
