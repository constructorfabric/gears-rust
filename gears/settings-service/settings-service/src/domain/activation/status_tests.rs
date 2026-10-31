//! The read half of the Recorder, `activation_status`, over `SQLite`: the
//! answer order after the caller's `read` decision (which the route takes
//! first and these tests hand over as the scope), the four indistinguishable
//! `NotFound` cases, and the fail-closed `Unavailable` of every step that asks
//! the Tenant Resolver or the database.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use toolkit_db::secure::DBRunner;
use toolkit_security::{AccessScope, InScopeFilter, ScopeConstraint, ScopeFilter};
use uuid::Uuid;

use crate::domain::access::TenantAccess;
use crate::domain::activation::publisher::{StateRow, StateWrite};
use crate::domain::activation::recorder::Transition;
use crate::domain::activation::testing::RecordingNotifyMetrics;
use crate::domain::activation::{
    ActivationRecorder, ActivationRepository, ActivationState, ActivationStatus, SubscriberTenant,
    Subscription, tenant_text,
};
use crate::domain::category::visibility::DOMAIN_PROPERTY;
use crate::domain::error::DomainError;
use crate::gear::ConcreteRecorder;
use crate::infra::storage::activation_repo::ActivationRepo;
use crate::infra::storage::audit_store::AuditStore;
use crate::test_support::{RestHarness, context_for, sqlite_provider_unmigrated};

async fn seed(
    h: &RestHarness,
    subscriber: &str,
    name: &str,
    tenant: Uuid,
    change: u128,
    settled: Option<(ActivationStatus, Option<&str>)>,
) {
    let conn = h.inner.db.conn().expect("connection");
    let key = h.inner.key(name).to_string();
    let tenant = tenant_text(tenant);
    let subscription = ActivationRepo
        .register(&conn, subscriber, &key, &tenant)
        .await
        .expect("registered");
    ActivationRepo
        .write_states(
            &conn,
            &StateWrite {
                key: key.clone(),
                change_ref: Uuid::from_u128(change),
                rows: vec![StateRow {
                    subscription_id: subscription.id,
                    tenant: tenant.clone(),
                    expected_value: "EXPECTED".to_owned(),
                }],
            },
        )
        .await
        .expect("written");
    if let Some((status, detail)) = settled {
        ActivationRepo
            .report(
                &conn,
                &Transition {
                    subscriber: subscriber.to_owned(),
                    key,
                    tenant,
                    change_ref: Uuid::from_u128(change),
                    status,
                    applied_value: Some("APPLIED".to_owned()),
                    detail: detail.map(str::to_owned),
                },
            )
            .await
            .expect("settled");
    }
}

async fn status(
    h: &RestHarness,
    caller: Uuid,
    scope: &AccessScope,
    name: &str,
    tenant: Option<Uuid>,
) -> Result<crate::domain::activation::ActivationRead, DomainError> {
    h.recorder
        .activation_status(
            &h.inner.resolver,
            &context_for(caller),
            scope,
            &h.inner.key(name),
            tenant,
        )
        .await
}

#[tokio::test]
async fn the_items_are_the_states_in_subscriber_order_without_a_value() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    seed(
        &h,
        "gear-b",
        "proxy",
        a,
        2,
        Some((ActivationStatus::Succeeded, Some("ignored"))),
    )
    .await;
    seed(
        &h,
        "gear-c",
        "proxy",
        a,
        3,
        Some((ActivationStatus::Failed, Some("pool full"))),
    )
    .await;
    seed(&h, "gear-a", "proxy", a, 1, None).await;
    seed(&h, "gear-a", "proxy", h.inner.tree.b, 9, None).await;

    let read = status(&h, a, &AccessScope::allow_all(), "proxy", None)
        .await
        .expect("read");
    assert_eq!(read.tenant, a, "the caller's own tenant by default");
    let seen: Vec<_> = read
        .items
        .iter()
        .map(|i| {
            (
                i.subscriber.as_str(),
                i.status,
                i.reported_at.is_some(),
                i.detail.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("gear-a", ActivationStatus::Awaiting, false, None),
            ("gear-b", ActivationStatus::Succeeded, true, None),
            ("gear-c", ActivationStatus::Failed, true, Some("pool full")),
        ]
    );
    assert_eq!(read.items[0].change_ref, Uuid::from_u128(1));
}

#[tokio::test]
async fn an_unsubscribed_key_has_no_items_and_a_retired_one_is_readable() {
    let h = RestHarness::new().await;
    let declaration = h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    assert!(
        status(
            &h,
            a,
            &AccessScope::allow_all(),
            "proxy",
            Some(h.inner.tree.b)
        )
        .await
        .expect("read")
        .items
        .is_empty()
    );
    seed(&h, "gear-a", "proxy", a, 1, None).await;
    h.inner.retire(declaration).await;
    assert_eq!(
        status(&h, a, &AccessScope::allow_all(), "proxy", None)
            .await
            .expect("read")
            .items
            .len(),
        1
    );
}

#[tokio::test]
async fn the_four_cases_are_one_not_found() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let walled = h.inner.declare("walled", "cascading", json!(true)).await;
    let hidden = h.inner.declare("hidden", "cascading", json!(true)).await;
    h.inner.bind_domain(walled, "commercial").await;
    h.restrict(hidden, h.inner.tree.a, TenantAccess::Hidden)
        .await;
    let a = h.inner.tree.a;
    let below = Uuid::new_v4();
    h.inner.hierarchy.add_tenant(below, h.inner.tree.s);
    let infrastructure = AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::In(
        InScopeFilter::new(DOMAIN_PROPERTY, vec!["infrastructure".into()]),
    )]));
    let all = AccessScope::allow_all();

    let cases = [
        status(&h, a, &all, "proxy", Some(h.inner.tree.c)).await,
        status(&h, a, &all, "proxy", Some(h.inner.tree.s)).await,
        status(&h, a, &all, "proxy", Some(below)).await,
        status(&h, a, &all, "proxy", Some(Uuid::new_v4())).await,
        status(&h, a, &all, "undeclared", None).await,
        status(&h, a, &all, "hidden", None).await,
        status(&h, a, &infrastructure, "walled", None).await,
    ];
    for case in &cases {
        assert!(
            matches!(
                case,
                Err(DomainError::NotFound {
                    resource: "declaration"
                })
            ),
            "{case:?}"
        );
    }
    // An undomained declaration stays visible to a domain-narrowed caller.
    assert!(status(&h, a, &infrastructure, "proxy", None).await.is_ok());
}

#[tokio::test]
async fn a_tenant_the_resolver_forgets_between_its_two_questions_is_the_one_not_found() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let (a, b) = (h.inner.tree.a, h.inner.tree.b);
    // Inside the subtree for the first question, unknown to the second.
    h.inner.hierarchy.forget_for_standalone(b);
    let refused = status(&h, a, &AccessScope::allow_all(), "proxy", Some(b)).await;
    assert!(
        matches!(
            refused,
            Err(DomainError::NotFound {
                resource: "declaration"
            })
        ),
        "{refused:?}"
    );
}

#[tokio::test]
async fn a_resolver_that_cannot_answer_is_unavailable_and_decided_before_not_found() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    h.inner.hierarchy.set_membership_unavailable(true);
    let all = AccessScope::allow_all();
    // The subtree test of a tenant that is in fact outside the subtree: the
    // resolver's failure is decided before the `NotFound` it would have been.
    for tenant in [Some(h.inner.tree.c), Some(h.inner.tree.b)] {
        let refused = status(&h, a, &all, "proxy", tenant).await;
        assert!(
            matches!(refused, Err(DomainError::Unavailable { .. })),
            "{refused:?}"
        );
    }
    // The effective-access evaluation, for the caller's own tenant.
    h.inner.hierarchy.set_membership_unavailable(false);
    h.inner.hierarchy.set_unavailable(true);
    let own = status(&h, a, &all, "proxy", None).await;
    assert!(
        matches!(own, Err(DomainError::Unavailable { .. })),
        "{own:?}"
    );
}

#[tokio::test]
async fn a_database_that_cannot_answer_is_unavailable_never_not_found() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    let ctx = context_for(a);
    let key = h.inner.key("proxy");
    let all = AccessScope::allow_all();

    // At the declaration read: a database whose tables are not there.
    let broken: ConcreteRecorder = ActivationRecorder::new(
        sqlite_provider_unmigrated().await,
        ActivationRepo,
        AuditStore,
        Arc::new(RecordingNotifyMetrics::default()),
    );
    let at_declaration = broken
        .activation_status(&h.inner.resolver, &ctx, &all, &key, None)
        .await;
    assert!(
        matches!(at_declaration, Err(DomainError::Unavailable { .. })),
        "{at_declaration:?}"
    );

    // At the activation read: the declaration is read, the states are not.
    let states_down = ActivationRecorder::new(
        Arc::clone(&h.inner.db),
        StatesDown,
        AuditStore,
        Arc::new(RecordingNotifyMetrics::default()),
    );
    let at_states = states_down
        .activation_status(&h.inner.resolver, &ctx, &all, &key, None)
        .await;
    assert!(
        matches!(at_states, Err(DomainError::Unavailable { .. })),
        "{at_states:?}"
    );
}

#[tokio::test]
async fn reading_writes_nothing() {
    let (h, recorder) = RestHarness::recorded().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    seed(&h, "gear-a", "proxy", a, 1, None).await;
    recorder.clear();
    status(&h, a, &AccessScope::allow_all(), "proxy", None)
        .await
        .expect("read");
    let kinds: Vec<_> = recorder.events().iter().map(|e| e.kind).collect();
    assert!(
        kinds
            .iter()
            .all(|kind| *kind == toolkit_db::test_support::QueryKind::Select),
        "{}",
        recorder.dump()
    );
}

/// A repository whose state read cannot answer and which otherwise is the real one.
struct StatesDown;

#[async_trait]
impl ActivationRepository for StatesDown {
    async fn register<C: DBRunner>(
        &self,
        conn: &C,
        subscriber: &str,
        key: &str,
        tenant: &str,
    ) -> Result<Subscription, DomainError> {
        ActivationRepo.register(conn, subscriber, key, tenant).await
    }

    async fn of_key<C: DBRunner>(
        &self,
        conn: &C,
        key: &str,
    ) -> Result<Vec<Subscription>, DomainError> {
        ActivationRepo.of_key(conn, key).await
    }

    async fn find<C: DBRunner>(
        &self,
        conn: &C,
        subscriber: &str,
        key: &str,
        tenant: &str,
    ) -> Result<Option<Subscription>, DomainError> {
        ActivationRepo.find(conn, subscriber, key, tenant).await
    }

    async fn retire<C: DBRunner>(
        &self,
        conn: &C,
        subscriber: &str,
        key: &str,
        tenant: &str,
    ) -> Result<bool, DomainError> {
        ActivationRepo.retire(conn, subscriber, key, tenant).await
    }

    async fn write_states<C: DBRunner>(
        &self,
        conn: &C,
        write: &StateWrite,
    ) -> Result<Vec<SubscriberTenant>, DomainError> {
        ActivationRepo.write_states(conn, write).await
    }

    async fn state<C: DBRunner>(
        &self,
        conn: &C,
        subscriber: &str,
        key: &str,
        tenant: &str,
    ) -> Result<Option<ActivationState>, DomainError> {
        ActivationRepo.state(conn, subscriber, key, tenant).await
    }

    async fn states_of<C: DBRunner>(
        &self,
        _conn: &C,
        _key: &str,
        _tenant: &str,
    ) -> Result<Vec<ActivationState>, DomainError> {
        Err(DomainError::Unavailable {
            detail: "the activation store is down".to_owned(),
        })
    }

    async fn report<C: DBRunner>(
        &self,
        conn: &C,
        transition: &Transition,
    ) -> Result<Option<time::OffsetDateTime>, DomainError> {
        ActivationRepo.report(conn, transition).await
    }
}
