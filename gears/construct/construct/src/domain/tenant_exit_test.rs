//! Tenant exit over in-memory tables, a scripted tenant resolver and a
//! scripted graph storage offboarding.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use uuid::Uuid;

use crate::domain::deletion::DeletionEvent;
use crate::domain::error::DomainError;
use crate::domain::tenant_exit::{Exit, GraphExit, TenantExit, TenantRows, TenantStatus};
use crate::test_support::{
    MemoryDeletionStore, NoTenantResolver, RecordingDeletionEvents, ScriptedOffboarding,
    ScriptedStatuses, tenant_exit_over,
};

struct Fixture {
    tenant: Uuid,
    store: Arc<MemoryDeletionStore>,
    statuses: Arc<ScriptedStatuses>,
    graph: Arc<ScriptedOffboarding>,
    events: Arc<RecordingDeletionEvents>,
    exit: TenantExit,
}

/// A tenant Construct keeps, deleted on the platform, with rows in every
/// table, run as a processing identity.
fn deleted_tenant() -> Fixture {
    let tenant = Uuid::new_v4();
    let store = Arc::new(MemoryDeletionStore::default());
    store.tenants.lock().unwrap().insert(tenant);
    *store.tenant_rows.lock().unwrap() = TenantRows {
        subject_settings: 2,
        review_requests: 3,
        record_ids: 4,
    };
    let statuses = Arc::new(ScriptedStatuses::default());
    statuses.set(tenant, TenantStatus::Deleted);
    let graph = Arc::new(ScriptedOffboarding::default());
    let events = Arc::new(RecordingDeletionEvents::default());
    let exit = tenant_exit_over(
        Some(Uuid::new_v4()),
        Arc::clone(&store),
        Arc::clone(&statuses) as _,
        Arc::clone(&graph),
        Arc::clone(&events),
    );
    Fixture {
        tenant,
        store,
        statuses,
        graph,
        events,
        exit,
    }
}

#[tokio::test]
async fn a_deleted_tenant_is_asked_about_as_construct_in_that_tenant() {
    let f = deleted_tenant();

    assert!(f.exit.deleted(f.tenant).await.unwrap());
    assert_eq!(
        f.statuses.asked(),
        [(f.tenant, f.tenant)],
        "the resolver is asked about the tenant, by Construct acting in it"
    );
}

#[tokio::test]
async fn a_present_or_unknown_tenant_is_not_deleted() {
    let f = deleted_tenant();
    for status in [TenantStatus::Present, TenantStatus::Unknown] {
        f.statuses.set(f.tenant, status);
        assert!(!f.exit.deleted(f.tenant).await.unwrap(), "{status:?}");
    }
}

#[tokio::test]
async fn without_a_resolver_or_an_identity_the_question_is_not_configured() {
    let f = deleted_tenant();
    let without_resolver = tenant_exit_over(
        Some(Uuid::new_v4()),
        Arc::clone(&f.store),
        Arc::new(NoTenantResolver),
        Arc::clone(&f.graph),
        Arc::clone(&f.events),
    );
    let without_identity = tenant_exit_over(
        None,
        Arc::clone(&f.store),
        Arc::clone(&f.statuses) as _,
        Arc::clone(&f.graph),
        Arc::clone(&f.events),
    );

    for exit in [without_resolver, without_identity] {
        assert!(matches!(
            exit.deleted(f.tenant).await,
            Err(DomainError::NotConfigured(_))
        ));
    }
}

#[tokio::test]
async fn an_exit_removes_the_rows_offboards_the_graph_and_forgets_the_tenant() {
    let f = deleted_tenant();

    assert_eq!(f.exit.exit(f.tenant).await.unwrap(), Exit::Done);

    assert_eq!(*f.store.emptied.lock().unwrap(), [f.tenant]);
    assert_eq!(
        f.graph.asked(),
        [(f.tenant, f.tenant)],
        "graph storage is asked as Construct in the tenant"
    );
    assert_eq!(*f.store.forgotten.lock().unwrap(), [f.tenant]);
    assert!(!f.store.tenants.lock().unwrap().contains(&f.tenant));
    assert_eq!(
        f.events.taken(),
        [
            DeletionEvent::TenantRowsRemoved {
                tenant_id: f.tenant,
                subject_settings: 2,
                review_requests: 3,
                record_ids: 4,
            },
            DeletionEvent::TenantExited {
                tenant_id: f.tenant
            },
        ]
    );
}

#[tokio::test]
async fn while_graph_storage_is_removing_the_graph_the_tenant_is_kept_for_the_next_run() {
    let f = deleted_tenant();
    f.graph.answer(Ok(GraphExit::UnderWay));

    assert_eq!(f.exit.exit(f.tenant).await.unwrap(), Exit::UnderWay);
    assert!(f.store.forgotten.lock().unwrap().is_empty());
    assert!(f.store.tenants.lock().unwrap().contains(&f.tenant));

    // The next run finds nothing left in the tables, and the graph done.
    *f.store.tenant_rows.lock().unwrap() = TenantRows::default();
    assert_eq!(f.exit.exit(f.tenant).await.unwrap(), Exit::Done);
    assert_eq!(*f.store.emptied.lock().unwrap(), [f.tenant, f.tenant]);
    assert_eq!(*f.store.forgotten.lock().unwrap(), [f.tenant]);
    assert_eq!(
        f.events
            .taken()
            .into_iter()
            .filter(|event| matches!(event, DeletionEvent::TenantRowsRemoved { .. }))
            .count(),
        1,
        "a run that removed no row records none"
    );
}

#[tokio::test]
async fn a_graph_storage_failure_keeps_the_tenant_for_the_next_run() {
    let f = deleted_tenant();
    f.graph.answer(Err(DomainError::Unavailable(
        "graph storage is down".to_owned(),
    )));

    assert!(matches!(
        f.exit.exit(f.tenant).await,
        Err(DomainError::Unavailable(_))
    ));
    assert_eq!(*f.store.emptied.lock().unwrap(), [f.tenant]);
    assert!(f.store.forgotten.lock().unwrap().is_empty());

    assert_eq!(f.exit.exit(f.tenant).await.unwrap(), Exit::Done);
    assert_eq!(*f.store.forgotten.lock().unwrap(), [f.tenant]);
}

#[tokio::test]
async fn a_table_failure_stops_before_graph_storage_is_asked() {
    let f = deleted_tenant();
    f.store.emptying_down.store(true, Ordering::SeqCst);

    assert!(matches!(
        f.exit.exit(f.tenant).await,
        Err(DomainError::Unavailable(_))
    ));
    assert!(f.graph.asked().is_empty());
    assert!(f.store.forgotten.lock().unwrap().is_empty());
    assert!(f.events.taken().is_empty());
}
