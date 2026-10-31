//! The Subscription Manager over `SQLite`: the registration decisions, the
//! idempotent insert, the retire, and which subscriptions a change qualifies.

use std::sync::Arc;

use serde_json::json;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::domain::access::{AccessRepository, RestrictionDraft, TenantAccess};
use crate::domain::activation::publisher::{StateRow, StateWrite};
use crate::domain::activation::{
    ActivationRepository, ActivationState, Admission, ReadGate, SubscriptionManager, subscriber_of,
    tenant_text,
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
    manager: Manager,
}

impl Fixture {
    async fn new() -> Self {
        let base = ResolutionHarness::new().await;
        let gate = FakeReadGate::allowing();
        let manager = SubscriptionManager::new(
            Arc::clone(&base.resolver),
            ActivationRepo,
            Arc::clone(&gate) as Arc<dyn ReadGate>,
        );
        Self {
            base,
            gate,
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
        let conn = self.base.db.conn().expect("connection");
        self.manager
            .admit(&conn, &Self::ctx(caller), &self.base.key(name), tenant)
            .await
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
                subscriber,
                self.base.key(name).as_str(),
                &tenant_text(tenant),
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
                subscriber,
                self.base.key(name).as_str(),
                &tenant_text(tenant),
            )
            .await
            .expect("state")
    }

    /// An activation state a change would have written.
    async fn seed_state(&self, subscriber: &str, name: &str, tenant: Uuid) {
        let conn = self.base.db.conn().expect("connection");
        let key = self.base.key(name);
        let subscription = ActivationRepo
            .find(&conn, subscriber, key.as_str(), &tenant_text(tenant))
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

#[tokio::test]
async fn registering_the_same_triple_twice_keeps_one_row() {
    let f = Fixture::new().await;
    let root = f.base.tree.root;
    f.base
        .declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let admission = f.admit(root, "flag", root).await.expect("admitted");
    let conn = f.base.db.conn().expect("connection");
    let first = f
        .manager
        .register_in(&conn, &admission)
        .await
        .expect("first");
    let second = f
        .manager
        .register_in(&conn, &admission)
        .await
        .expect("second");
    assert_eq!(first.id, second.id, "the existing row is kept");
    let all = ActivationRepo
        .of_key(&conn, f.base.key("flag").as_str())
        .await
        .expect("read");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].tenant, admission.tenant);
}

#[tokio::test]
async fn a_retire_deletes_the_subscription_and_its_state_and_only_the_callers() {
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
        .retire_in(&conn, "gear", key.as_str(), &tenant_text(root))
        .await
        .expect("retired");

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
            .find(&conn, "gear", key.as_str(), &tenant_text(root))
            .await
            .expect("lookup")
            .is_none()
    );

    // A key with no subscription of the caller's: not found, nothing deleted.
    let none = f
        .manager
        .retire_in(&conn, "gear", key.as_str(), &tenant_text(root))
        .await;
    assert!(
        matches!(none, Err(DomainError::NotFound { .. })),
        "{none:?}"
    );
    let foreign = f
        .manager
        .retire_in(&conn, "stranger", key.as_str(), &tenant_text(a))
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

#[derive(Clone, Default)]
struct LogBuf(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("log").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuf {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
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
    let text = String::from_utf8_lossy(&logs.0.lock().expect("log")).into_owned();
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
