//! Force competing admissions beyond their initial reads using PostgreSQL locks.
use super::super::tests::{isolated_url, store_at};
use super::*;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement, TransactionTrait};

async fn both_waiting_on(admin: &sea_orm::DatabaseConnection, apps: &[String; 2], table: &str) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let row = admin.query_one_raw(Statement::from_sql_and_values(DatabaseBackend::Postgres,
                "SELECT COUNT(*) AS blocked FROM pg_stat_activity WHERE application_name IN ($1, $2) AND wait_event_type = 'Lock' AND query LIKE '%' || $3 || '%'",
                [apps[0].clone().into(), apps[1].clone().into(), table.to_owned().into()]))
                .await.unwrap().unwrap();
            let blocked: i64 = row.try_get("", "blocked").unwrap();
            if blocked == 2 { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("both independent transactions must reach the guarded database boundary");
}
fn named_url(url: &str, name: &str) -> String {
    format!(
        "{url}{}application_name={name}",
        if url.contains('?') { "&" } else { "?" }
    )
}
fn pending_candidate(seed: &Journal) -> Journal {
    let mut value = seed.clone();
    value.run.id = RunId(Uuid::new_v4());
    value.registration_contract = None;
    value
}
async fn racing_starts(
    first: Arc<JournalStore>,
    second: Arc<JournalStore>,
    seed: &Journal,
    options: StartOptions,
) -> [tokio::task::JoinHandle<Result<StartResult, crate::domain::error::DomainError>>; 2] {
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let handles = [first, second].map(|store| {
        let barrier = barrier.clone();
        let candidate = pending_candidate(seed);
        let options = options.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            store
                .start(AccessScope::allow_all(), candidate, options)
                .await
        })
    });
    barrier.wait().await;
    handles
}
async fn finish(
    handles: [tokio::task::JoinHandle<Result<StartResult, crate::domain::error::DomainError>>; 2],
) -> [StartResult; 2] {
    let [left, right] = handles;
    tokio::time::timeout(Duration::from_secs(20), async {
        [left.await.unwrap().unwrap(), right.await.unwrap().unwrap()]
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn independent_pools_retry_idempotency_unique_conflict_after_both_replay_misses() {
    let database = isolated_url().await;
    let prefix = format!("idempotency-{}", Uuid::new_v4().simple());
    let apps = [format!("{prefix}-a"), format!("{prefix}-b")];
    let first = Arc::new(store_at(&named_url(&database, &apps[0])).await);
    let second = Arc::new(store_at(&named_url(&database, &apps[1])).await);
    let definition = execution_definition();
    first.ensure_contract(definition.contract()).await.unwrap();
    let admin = sea_orm::Database::connect(&*database).await.unwrap();
    let blocker = admin.begin().await.unwrap();
    blocker
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT name FROM durable_definitions WHERE name = $1 FOR UPDATE",
            [definition.name.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    let seed = journal();
    let handles = racing_starts(
        first.clone(),
        second,
        &seed,
        StartOptions {
            idempotency_key: Some("same-request".into()),
            ..Default::default()
        },
    )
    .await;
    // replay() precedes the shared catalog lock, so both blocked transactions
    // have already observed that the idempotency key is absent.
    both_waiting_on(&admin, &apps, "durable_definitions").await;
    blocker.commit().await.unwrap();
    let [left, right] = finish(handles).await;
    assert_eq!(left.run_id, right.run_id);
    assert_eq!(left.requested_generation, 1);
    let scope = AccessScope::allow_all();
    let page = first
        .list_progress(&scope, &all_query(DateTime::from_timestamp(0, 0).unwrap()))
        .await
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, left.run_id);
    let deliveries = first.deliveries(&scope, Utc::now(), 100).await.unwrap();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].run_id, left.run_id);
    let stored = first.get(&scope, left.run_id).await.unwrap().unwrap();
    assert_eq!(stored.run.owner, seed.run.owner);
    assert!(stored.run.activities.iter().all(|step| step.attempts == 0));
}

#[tokio::test]
async fn independent_pools_retry_coalescing_slot_creation_unique_conflict() {
    let database = isolated_url().await;
    let prefix = format!("coalesce-{}", Uuid::new_v4().simple());
    let apps = [format!("{prefix}-a"), format!("{prefix}-b")];
    let first = Arc::new(store_at(&named_url(&database, &apps[0])).await);
    let second = Arc::new(store_at(&named_url(&database, &apps[1])).await);
    first
        .ensure_contract(execution_definition().contract())
        .await
        .unwrap();
    let admin = sea_orm::Database::connect(&*database).await.unwrap();
    let blocker = admin.begin().await.unwrap();
    blocker
        .execute_unprepared("LOCK TABLE durable_coalescing IN SHARE MODE")
        .await
        .unwrap();
    let handles = racing_starts(
        first.clone(),
        second,
        &journal(),
        StartOptions {
            coalescing_key: Some("same-slot".into()),
            ..Default::default()
        },
    )
    .await;
    // SHARE permits the missing-slot SELECT, but blocks each subsequent INSERT.
    both_waiting_on(&admin, &apps, "durable_coalescing").await;
    blocker.commit().await.unwrap();
    let [left, right] = finish(handles).await;
    assert_eq!(left.run_id, right.run_id);
    assert_eq!(left.requested_generation, 1);
    assert_eq!(right.requested_generation, 1);
    assert_ne!(left.coalesced, right.coalesced);
    let scope = AccessScope::allow_all();
    assert_eq!(
        first
            .list_progress(&scope, &all_query(DateTime::from_timestamp(0, 0).unwrap()))
            .await
            .unwrap()
            .total,
        1
    );
    let deliveries = first.deliveries(&scope, Utc::now(), 100).await.unwrap();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].run_id, left.run_id);
}

#[tokio::test]
async fn independent_pools_retry_successor_cas_and_park_exactly_one_successor() {
    let database = isolated_url().await;
    let prefix = format!("successor-{}", Uuid::new_v4().simple());
    let apps = [format!("{prefix}-a"), format!("{prefix}-b")];
    let first = Arc::new(store_at(&named_url(&database, &apps[0])).await);
    let second = Arc::new(store_at(&named_url(&database, &apps[1])).await);
    let definition = execution_definition();
    first.ensure_contract(definition.contract()).await.unwrap();
    let seed = journal();
    let options = StartOptions {
        coalescing_key: Some("same-slot".into()),
        ..Default::default()
    };
    let active = first
        .start(
            AccessScope::allow_all(),
            pending_candidate(&seed),
            options.clone(),
        )
        .await
        .unwrap();
    let mut claimed = first
        .get(&AccessScope::allow_all(), active.run_id)
        .await
        .unwrap()
        .unwrap();
    claimed
        .claim(
            &definition.contract(),
            claimed.delivery_generation,
            Utc::now(),
            Duration::from_secs(120),
        )
        .unwrap()
        .unwrap();
    first
        .save(AccessScope::allow_all(), claimed.revision, claimed, false)
        .await
        .unwrap();
    let admin = sea_orm::Database::connect(&*database).await.unwrap();
    let blocker = admin.begin().await.unwrap();
    blocker
        .execute_unprepared("LOCK TABLE durable_coalescing IN SHARE MODE")
        .await
        .unwrap();
    let handles = racing_starts(first.clone(), second, &seed, options).await;
    // Both transactions read successor=None before their slot UPDATE is blocked.
    both_waiting_on(&admin, &apps, "durable_coalescing").await;
    blocker.commit().await.unwrap();
    let [left, right] = finish(handles).await;
    assert_eq!(left.run_id, right.run_id);
    assert_ne!(left.run_id, active.run_id);
    assert_eq!(left.requested_generation, 2);
    assert_eq!(right.requested_generation, 2);
    assert_ne!(left.coalesced, right.coalesced);
    let scope = AccessScope::allow_all();
    assert_eq!(
        first
            .list_progress(&scope, &all_query(DateTime::from_timestamp(0, 0).unwrap()))
            .await
            .unwrap()
            .total,
        2
    );
    let successor = first.get(&scope, left.run_id).await.unwrap().unwrap();
    assert_eq!(successor.run.status, RunStatus::Queued);
    assert!(successor.run.next_attempt_at.is_none());
    assert!(
        successor
            .run
            .activities
            .iter()
            .all(|step| step.attempts == 0)
    );
    let current = first.get(&scope, active.run_id).await.unwrap().unwrap();
    assert_eq!(current.run.status, RunStatus::Running);
    assert_eq!(current.run.activities[0].attempts, 1);
    let deliveries = first.deliveries(&scope, Utc::now(), 100).await.unwrap();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].run_id, active.run_id);
}
