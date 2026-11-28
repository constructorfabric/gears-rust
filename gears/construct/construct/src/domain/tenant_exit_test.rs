//! Tenant exit over in-memory tables, a scripted tenant resolver and a
//! scripted graph storage offboarding.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::deletion::DeletionEvent;
use crate::domain::error::DomainError;
use crate::domain::tenant_exit::{Exit, GraphExit, Kept, TenantExit, TenantRows, TenantStatus};
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
    store.keep([tenant]);
    *store.tenant_rows.lock().unwrap() = TenantRows {
        subject_settings: 2,
        subject_schemas: 1,
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

impl Fixture {
    /// Nothing of the tenant's was removed, offboarded or forgotten.
    fn untouched(&self) -> bool {
        self.store.emptied().is_empty()
            && self.store.forgotten().is_empty()
            && self.store.keeps(self.tenant)
            && self.events.taken().is_empty()
    }
}

#[tokio::test]
async fn a_deleted_tenant_is_asked_about_as_construct_in_that_tenant() {
    let f = deleted_tenant();

    assert!(matches!(f.exit.run(f.tenant).await, Ok(Exit::Done)));
    assert_eq!(
        f.statuses.asked(),
        [(f.tenant, f.tenant)],
        "the resolver is asked about the tenant, by Construct acting in it"
    );
}

#[tokio::test]
async fn a_present_or_unknown_tenant_is_left_untouched() {
    for (status, kept) in [
        (TenantStatus::Present, Kept::Present),
        (TenantStatus::Unknown, Kept::Unknown),
    ] {
        let f = deleted_tenant();
        f.statuses.set(f.tenant, status);

        let ended = f.exit.run(f.tenant).await;

        assert!(
            matches!(&ended, Ok(Exit::Kept(why)) if *why == kept),
            "{status:?}: {ended:?}"
        );
        assert!(!ended.unwrap().tenant_deleted());
        assert!(f.graph.asked().is_empty(), "{status:?}");
        assert!(f.untouched(), "{status:?}");
    }
}

#[tokio::test]
async fn without_a_resolver_or_an_identity_nothing_is_touched() {
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
            exit.run(f.tenant).await,
            Err(DomainError::NotConfigured(_))
        ));
    }
    assert!(f.graph.asked().is_empty());
    assert!(f.untouched());
}

#[tokio::test]
async fn a_resolver_that_fails_leaves_the_tenant_untouched() {
    let f = deleted_tenant();
    f.statuses.fail(f.tenant);

    assert!(matches!(
        f.exit.run(f.tenant).await,
        Err(DomainError::Unavailable(_))
    ));
    assert!(f.graph.asked().is_empty());
    assert!(f.untouched());
}

#[tokio::test]
async fn an_exit_offboards_the_graph_removes_the_rows_in_the_tenants_scope_and_forgets_it() {
    let f = deleted_tenant();

    assert!(matches!(f.exit.run(f.tenant).await, Ok(Exit::Done)));

    assert_eq!(
        f.graph.asked(),
        [(f.tenant, f.tenant)],
        "graph storage is asked as Construct in the tenant"
    );
    let own = AccessScope::for_tenant(f.tenant);
    let emptied = f.store.emptied.lock().unwrap().clone();
    assert_eq!(
        emptied,
        [(f.tenant, own.clone())],
        "in the tenant's own scope"
    );
    let forgotten = f.store.forgotten.lock().unwrap().clone();
    assert_eq!(forgotten, [(f.tenant, own)]);
    assert!(!f.store.keeps(f.tenant));
    assert_eq!(
        f.events.taken(),
        [
            DeletionEvent::TenantRowsRemoved {
                tenant_id: f.tenant,
                subject_settings: 2,
                subject_schemas: 1,
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
async fn while_graph_storage_is_removing_the_graph_the_rows_go_and_the_tenant_is_kept() {
    let f = deleted_tenant();
    f.graph.answer(Ok(GraphExit::UnderWay));

    assert!(matches!(f.exit.run(f.tenant).await, Ok(Exit::UnderWay)));
    assert_eq!(
        f.store.emptied(),
        [f.tenant],
        "the graph is fenced, so the rows go"
    );
    assert!(f.store.forgotten().is_empty());
    assert!(f.store.keeps(f.tenant));

    // The next run finds nothing left in the tables, and the graph done.
    *f.store.tenant_rows.lock().unwrap() = TenantRows::default();
    assert!(matches!(f.exit.run(f.tenant).await, Ok(Exit::Done)));
    assert_eq!(f.store.emptied(), [f.tenant, f.tenant]);
    assert_eq!(f.store.forgotten(), [f.tenant]);
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
async fn a_refused_offboarding_leaves_every_row_in_place() {
    let f = deleted_tenant();
    f.graph.answer(Err(DomainError::internal(
        "graph storage refused Construct's offboarding of the tenant",
    )));

    let ended = f.exit.run(f.tenant).await;

    assert!(
        matches!(ended, Ok(Exit::Stopped(DomainError::Internal(_)))),
        "{ended:?}"
    );
    assert!(
        f.untouched(),
        "the graph is not fenced, so the subjects' settings, their opt-outs and erasure \
         marks, stay"
    );
}

#[tokio::test]
async fn a_graph_storage_that_does_not_answer_is_asked_again_by_the_next_run() {
    let f = deleted_tenant();
    f.graph.answer(Err(DomainError::Unavailable(
        "graph storage is down".to_owned(),
    )));

    assert!(matches!(
        f.exit.run(f.tenant).await,
        Ok(Exit::Stopped(DomainError::Unavailable(_)))
    ));
    assert!(f.untouched());

    assert!(matches!(f.exit.run(f.tenant).await, Ok(Exit::Done)));
    assert_eq!(f.store.forgotten(), [f.tenant]);
}

#[tokio::test]
async fn a_table_failure_after_the_offboarding_keeps_the_tenant_for_the_next_run() {
    let f = deleted_tenant();
    f.store.emptying_down.store(true, Ordering::SeqCst);

    assert!(matches!(
        f.exit.run(f.tenant).await,
        Ok(Exit::Stopped(DomainError::Unavailable(_)))
    ));
    assert_eq!(f.graph.asked().len(), 1);
    assert!(f.store.forgotten().is_empty());
    assert!(f.store.keeps(f.tenant));
    assert!(f.events.taken().is_empty());

    f.store.emptying_down.store(false, Ordering::SeqCst);
    assert!(matches!(f.exit.run(f.tenant).await, Ok(Exit::Done)));
    assert_eq!(f.store.forgotten(), [f.tenant]);
}

#[tokio::test]
async fn a_forget_that_fails_keeps_the_tenant_and_records_no_exit_until_the_next_run() {
    let f = deleted_tenant();
    f.store.forgetting_down.store(true, Ordering::SeqCst);

    let ended = f.exit.run(f.tenant).await;

    assert!(
        matches!(ended, Ok(Exit::Stopped(DomainError::Unavailable(_)))),
        "{ended:?}"
    );
    assert!(f.store.keeps(f.tenant));
    assert!(
        matches!(
            f.events.taken()[..],
            [DeletionEvent::TenantRowsRemoved { .. }]
        ),
        "the rows went, the exit is not recorded"
    );

    f.store.forgetting_down.store(false, Ordering::SeqCst);
    *f.store.tenant_rows.lock().unwrap() = TenantRows::default();
    assert!(matches!(f.exit.run(f.tenant).await, Ok(Exit::Done)));
    assert!(!f.store.keeps(f.tenant));
    assert!(matches!(
        f.events.taken()[..],
        [
            DeletionEvent::TenantRowsRemoved { .. },
            DeletionEvent::TenantExited { .. }
        ]
    ));
}

#[tokio::test]
async fn a_tenant_that_exited_before_finds_nothing_and_records_nothing() {
    let f = deleted_tenant();
    assert!(matches!(f.exit.run(f.tenant).await, Ok(Exit::Done)));
    *f.store.tenant_rows.lock().unwrap() = TenantRows::default();
    let recorded = f.events.taken().len();

    let again = f.exit.run(f.tenant).await;

    assert!(matches!(again, Ok(Exit::Gone)), "{again:?}");
    assert!(again.unwrap().tenant_deleted(), "retention still leaves it");
    assert_eq!(
        f.events.taken().len(),
        recorded,
        "no second exit is recorded"
    );
}
