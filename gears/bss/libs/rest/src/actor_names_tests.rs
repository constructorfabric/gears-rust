//! Name projection, request-local deduplication, system actors and bounded failure behaviour.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use account_management_sdk::{
    self as am, AccountManagementClient, IdpUser, IdpUserFilterField, ListUsersQuery,
};
use async_trait::async_trait;
use toolkit::ClientHub;
use toolkit_canonical_errors::{CanonicalError, resource_error};
use toolkit_odata::filter::{FilterNode, ODataValue};
use toolkit_odata::{Page, PageInfo};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{
    ActorDirectory, ActorFields, ActorName, ActorNames, LOOKUP_BATCH_SIZE, LOOKUP_BUDGET,
    LOOKUP_CONCURRENCY, SYSTEM_LABEL, label, project_name, queried_ids,
};

/// The AM user resource, for the errors a fake directory answers with.
#[resource_error(gts_id!("cf.core.users.user.v1~"))]
struct UserResource;

/// Mutable upstream profiles; only the fake source owns these names.
#[derive(Default)]
struct Directory {
    calls: Mutex<Vec<Vec<Uuid>>>,
    users: Mutex<BTreeMap<Uuid, IdpUser>>,
    /// Lookups in flight now, and the most seen at once.
    in_flight: AtomicUsize,
    peak: AtomicUsize,
}

#[async_trait]
impl ActorDirectory for Directory {
    async fn list_users(
        &self,
        _ctx: &SecurityContext,
        query: ListUsersQuery,
    ) -> Result<Page<IdpUser>, CanonicalError> {
        let ids = query_ids(&query);
        self.calls.lock().unwrap().push(ids.clone());
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        // Let the other queued lookups start before this one answers.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        let users = self.users.lock().unwrap();
        Ok(page(
            ids.iter().filter_map(|id| users.get(id).cloned()).collect(),
            None,
        ))
    }
}

impl Directory {
    /// Every lookup's ids so far, read under the lock and released before any assertion.
    fn calls(&self) -> Vec<Vec<Uuid>> {
        self.calls.lock().unwrap().clone()
    }
}

/// Fail if a lookup ever asks for an unfiltered page or a malformed id set.
fn query_ids(query: &ListUsersQuery) -> Vec<Uuid> {
    let Some(FilterNode::InList {
        field: IdpUserFilterField::Id,
        values,
    }) = &query.filter
    else {
        panic!("expected a typed id-set lookup");
    };
    let ids: Vec<_> = values
        .iter()
        .map(|value| match value {
            ODataValue::Uuid(id) => *id,
            _ => panic!("expected a UUID"),
        })
        .collect();
    assert!(ids.len() <= LOOKUP_BATCH_SIZE);
    assert_eq!(query.pagination.top() as usize, ids.len());
    ids
}

/// One complete or continued provider page.
fn page(items: Vec<IdpUser>, cursor: Option<&str>) -> Page<IdpUser> {
    Page::new(
        items,
        PageInfo {
            next_cursor: cursor.map(str::to_owned),
            prev_cursor: None,
            limit: 200,
        },
    )
}

fn names(directory: Arc<dyn ActorDirectory>) -> ActorNames {
    ActorNames::with_directory(directory, &[])
}

#[test]
fn the_label_is_the_display_name_then_first_and_last_then_the_username() {
    let id = Uuid::from_u128(1);
    let mut user = IdpUser::new(id, "  alice ")
        .with_display_name("  Alice Jones  ")
        .with_first_name(" Alice ")
        .with_last_name(" Smith ")
        .with_email("private@example.test");
    assert_eq!(
        project_name(&user),
        ActorName::Resolved("Alice Jones".into())
    );
    user.display_name = Some(" \t ".into());
    assert_eq!(
        project_name(&user),
        ActorName::Resolved("Alice Smith".into())
    );
    user.first_name = None;
    assert_eq!(project_name(&user), ActorName::Resolved("Smith".into()));
    user.last_name = None;
    assert_eq!(project_name(&user), ActorName::Resolved("alice".into()));
    user.username = " ".into();
    assert_eq!(project_name(&user), ActorName::Unavailable);
}

#[test]
fn only_a_resolved_name_and_the_system_label_reach_the_wire() {
    assert_eq!(ActorName::Resolved("Ann".into()).label(), Some("Ann"));
    assert_eq!(ActorName::System.label(), Some(SYSTEM_LABEL));
    assert_eq!(SYSTEM_LABEL, "System");
    for absent in [
        ActorName::Restricted,
        ActorName::NotFound,
        ActorName::Unavailable,
    ] {
        assert_eq!(absent.label(), None);
    }
}

#[tokio::test]
async fn duplicate_ids_make_one_lookup_and_a_rename_shows_on_the_next_read() {
    let directory = Arc::new(Directory::default());
    let id = Uuid::from_u128(1);
    directory
        .users
        .lock()
        .unwrap()
        .insert(id, IdpUser::new(id, "before"));
    let service = names(directory.clone());
    let ctx = SecurityContext::anonymous();
    let first = service.resolve(&ctx, [id, id, id]).await;
    assert_eq!(first[&id], ActorName::Resolved("before".into()));
    assert_eq!(directory.calls(), [vec![id]]);
    directory
        .users
        .lock()
        .unwrap()
        .insert(id, IdpUser::new(id, "after"));
    let second = service.resolve(&ctx, [id]).await;
    assert_eq!(second[&id], ActorName::Resolved("after".into()));
    assert_eq!(directory.calls(), [vec![id], vec![id]]);
}

#[tokio::test]
async fn no_ids_never_read_the_directory() {
    let directory = Arc::new(Directory::default());
    let service = names(directory.clone());
    assert!(
        service
            .resolve(&SecurityContext::anonymous(), [])
            .await
            .is_empty()
    );
    assert!(directory.calls().is_empty());
}

#[tokio::test]
async fn four_hundred_fifty_ids_make_three_chunks() {
    let directory = Arc::new(Directory::default());
    for id in (1..=450).map(Uuid::from_u128) {
        directory
            .users
            .lock()
            .unwrap()
            .insert(id, IdpUser::new(id, "actor"));
    }
    let service = names(directory.clone());
    let resolved = service
        .resolve(
            &SecurityContext::anonymous(),
            (1..=450).map(Uuid::from_u128),
        )
        .await;
    assert_eq!(resolved.len(), 450);
    assert!(
        resolved
            .values()
            .all(|name| matches!(name, ActorName::Resolved(_)))
    );
    let mut sizes: Vec<_> = directory
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(Vec::len)
        .collect();
    sizes.sort_unstable();
    assert_eq!(sizes, [50, 200, 200]);
    assert!(directory.peak.load(Ordering::SeqCst) <= LOOKUP_CONCURRENCY);
}

#[tokio::test]
async fn many_chunks_run_at_most_four_at_once() {
    let directory = Arc::new(Directory::default());
    let service = names(directory.clone());
    let resolved = service
        .resolve(
            &SecurityContext::anonymous(),
            (1..=2000).map(Uuid::from_u128),
        )
        .await;
    assert_eq!(resolved.len(), 2000);
    assert!(resolved.values().all(|name| *name == ActorName::NotFound));
    assert_eq!(directory.calls().len(), 10);
    assert_eq!(directory.peak.load(Ordering::SeqCst), LOOKUP_CONCURRENCY);
}

#[tokio::test]
async fn a_system_id_reads_system_and_calls_nothing() {
    let directory = Arc::new(Directory::default());
    let system = Uuid::from_u128(0xf01);
    let service = ActorNames::with_directory(directory.clone(), &[system, Uuid::nil()]);
    let resolved = service
        .resolve(&SecurityContext::anonymous(), [system, Uuid::nil(), system])
        .await;
    assert_eq!(resolved[&system], ActorName::System);
    assert_eq!(resolved[&Uuid::nil()], ActorName::System);
    assert!(directory.calls().is_empty());

    let person = Uuid::from_u128(7);
    directory
        .users
        .lock()
        .unwrap()
        .insert(person, IdpUser::new(person, "person"));
    let mixed = service
        .resolve(&SecurityContext::anonymous(), [system, person])
        .await;
    assert_eq!(mixed[&system], ActorName::System);
    assert_eq!(mixed[&person], ActorName::Resolved("person".into()));
    assert_eq!(directory.calls(), [vec![person]]);
}

/// Scripted pages exercise cursor handling and the error mapping through the SDK query.
struct PagedDirectory {
    pages: Mutex<VecDeque<Result<Page<IdpUser>, CanonicalError>>>,
    queries: Mutex<Vec<ListUsersQuery>>,
}

#[async_trait]
impl ActorDirectory for PagedDirectory {
    async fn list_users(
        &self,
        _ctx: &SecurityContext,
        query: ListUsersQuery,
    ) -> Result<Page<IdpUser>, CanonicalError> {
        query_ids(&query);
        self.queries.lock().unwrap().push(query);
        self.pages
            .lock()
            .unwrap()
            .pop_front()
            .expect("no excess page calls")
    }
}

impl PagedDirectory {
    /// Every query so far, read under the lock and released before any assertion.
    fn queries(&self) -> Vec<ListUsersQuery> {
        self.queries.lock().unwrap().clone()
    }
}

/// A directory whose every expected request has an explicit answer.
fn paged_directory(pages: Vec<Result<Page<IdpUser>, CanonicalError>>) -> Arc<PagedDirectory> {
    Arc::new(PagedDirectory {
        pages: Mutex::new(pages.into()),
        queries: Mutex::new(Vec::new()),
    })
}

#[tokio::test]
async fn the_directory_errors_map_to_restricted_not_found_and_unavailable() {
    let id = Uuid::from_u128(1);
    for (error, expected) in [
        (
            UserResource::permission_denied()
                .with_reason("PROFILE_READ_DENIED")
                .create(),
            ActorName::Restricted,
        ),
        (
            CanonicalError::unauthenticated()
                .with_reason("AUTHENTICATION_REQUIRED")
                .create(),
            ActorName::Restricted,
        ),
        (
            UserResource::not_found("user not found")
                .with_resource("user")
                .create(),
            ActorName::NotFound,
        ),
        (
            CanonicalError::service_unavailable().create(),
            ActorName::Unavailable,
        ),
        (
            CanonicalError::internal("provider failed").create(),
            ActorName::Unavailable,
        ),
    ] {
        let service = names(paged_directory(vec![Err(error)]));
        let resolved = service.resolve(&SecurityContext::anonymous(), [id]).await;
        assert_eq!(resolved[&id], expected);
    }
}

/// A degraded chunk logs one warn line with its reason kind and id count: no id, no profile and
/// no error text. AM's own answers (refused, not found) are not an outage and log nothing.
#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn a_degraded_chunk_warns_with_its_reason_and_id_count_only() {
    let (one, two) = (Uuid::from_u128(0xabc1), Uuid::from_u128(0xabc2));
    let failed = paged_directory(vec![Err(
        CanonicalError::internal("provider detail text").create()
    )]);
    names(failed)
        .resolve(&SecurityContext::anonymous(), [one, two])
        .await;
    assert!(logs_contain("reason=\"directory\" ids=2"));
    let drifted = paged_directory(vec![Ok(page(Vec::new(), Some("never-progresses")))]);
    names(drifted)
        .resolve(&SecurityContext::anonymous(), [one])
        .await;
    assert!(logs_contain("reason=\"pagination\" ids=1"));
    names(Arc::new(HungDirectory::default()))
        .resolve(&SecurityContext::anonymous(), [one])
        .await;
    assert!(logs_contain("reason=\"budget\" ids=1"));
    assert!(!logs_contain(&one.to_string()));
    assert!(!logs_contain(&two.to_string()));
    assert!(!logs_contain("provider detail text"));

    let refused = paged_directory(vec![Err(UserResource::permission_denied()
        .with_reason("PROFILE_READ_DENIED")
        .create())]);
    names(refused)
        .resolve(&SecurityContext::anonymous(), [two])
        .await;
    let absent = paged_directory(vec![Ok(page(Vec::new(), None))]);
    names(absent)
        .resolve(&SecurityContext::anonymous(), [two])
        .await;
    logs_assert(|lines| {
        let warned = lines.iter().filter(|line| line.contains(" WARN ")).count();
        (warned == 3)
            .then_some(())
            .ok_or_else(|| format!("{warned} warn lines: {lines:?}"))
    });
}

#[test]
fn actor_names_debug_shows_the_system_ids_and_not_the_directory() {
    let system = Uuid::from_u128(0xf01);
    let debug = format!(
        "{:?}",
        ActorNames::with_directory(Arc::new(Directory::default()), &[system])
    );
    assert!(debug.starts_with("ActorNames"), "{debug}");
    assert!(debug.contains(&system.to_string()), "{debug}");
}

#[tokio::test]
async fn pagination_keeps_the_exact_filter_and_marks_absence_only_at_the_end() {
    let first = Uuid::from_u128(1);
    let second = Uuid::from_u128(2);
    let absent = Uuid::from_u128(3);
    let directory = paged_directory(vec![
        Ok(page(vec![IdpUser::new(first, "alice")], Some("page-2"))),
        Ok(page(vec![IdpUser::new(second, "bob")], None)),
    ]);
    let service = names(directory.clone());
    let resolved = service
        .resolve(&SecurityContext::anonymous(), [first, second, absent])
        .await;
    assert_eq!(resolved[&first], ActorName::Resolved("alice".into()));
    assert_eq!(resolved[&second], ActorName::Resolved("bob".into()));
    assert_eq!(resolved[&absent], ActorName::NotFound);
    let queries = directory.queries();
    assert_eq!(queries.len(), 2);
    assert_eq!(query_ids(&queries[0]), [first, second, absent]);
    assert_eq!(query_ids(&queries[1]), [first, second, absent]);
    assert!(queries[0].pagination.cursor().is_none());
    assert_eq!(queries[1].pagination.cursor(), Some("page-2"));
}

#[tokio::test]
async fn a_failed_later_page_keeps_known_names_but_claims_no_absence() {
    let first = Uuid::from_u128(1);
    let second = Uuid::from_u128(2);
    let directory = paged_directory(vec![
        Ok(page(vec![IdpUser::new(first, "alice")], Some("page-2"))),
        Err(CanonicalError::service_unavailable().create()),
    ]);
    let service = names(directory);
    let resolved = service
        .resolve(&SecurityContext::anonymous(), [first, second])
        .await;
    assert_eq!(resolved[&first], ActorName::Resolved("alice".into()));
    assert_eq!(resolved[&second], ActorName::Unavailable);
}

#[tokio::test]
async fn an_empty_continued_page_and_duplicate_ids_are_not_absence() {
    let id = Uuid::from_u128(1);
    for response in [
        page(Vec::new(), Some("never-progresses")),
        page(vec![IdpUser::new(id, "a"), IdpUser::new(id, "b")], None),
    ] {
        let directory = paged_directory(vec![Ok(response)]);
        let service = names(directory.clone());
        assert_eq!(
            service.resolve(&SecurityContext::anonymous(), [id]).await[&id],
            ActorName::Unavailable
        );
        assert_eq!(directory.queries().len(), 1);
    }
}

#[tokio::test]
async fn a_repeated_cursor_stops_without_unbounded_retries() {
    let first = Uuid::from_u128(1);
    let second = Uuid::from_u128(2);
    let third = Uuid::from_u128(3);
    let directory = paged_directory(vec![
        Ok(page(
            vec![IdpUser::new(first, "alice")],
            Some("same-cursor"),
        )),
        Ok(page(vec![IdpUser::new(second, "bob")], Some("same-cursor"))),
    ]);
    let service = names(directory.clone());
    let resolved = service
        .resolve(&SecurityContext::anonymous(), [first, second, third])
        .await;
    assert_eq!(resolved[&third], ActorName::Unavailable);
    assert_eq!(directory.queries().len(), 2);
}

#[tokio::test]
async fn a_mismatched_profile_id_is_not_disclosed() {
    let directory = Arc::new(Directory::default());
    let id = Uuid::from_u128(1);
    directory
        .users
        .lock()
        .unwrap()
        .insert(id, IdpUser::new(Uuid::from_u128(2), "someone else"));
    let service = names(directory);
    assert_eq!(
        service.resolve(&SecurityContext::anonymous(), [id]).await[&id],
        ActorName::Unavailable
    );
}

#[tokio::test]
async fn an_absent_account_management_client_is_unavailable() {
    let service = ActorNames::from_hub(Arc::new(ClientHub::new()), &[]);
    let id = Uuid::from_u128(1);
    assert_eq!(
        service.resolve(&SecurityContext::anonymous(), [id]).await[&id],
        ActorName::Unavailable
    );
}

/// An unanswering source records starts and cancellation without sleeping.
#[derive(Default)]
struct HungDirectory {
    started: AtomicUsize,
    cancelled: AtomicUsize,
    fast_id: Option<Uuid>,
}

/// Counts a dropped in-flight future.
struct Cancelled<'a>(&'a AtomicUsize);

impl Drop for Cancelled<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl ActorDirectory for HungDirectory {
    async fn list_users(
        &self,
        _ctx: &SecurityContext,
        query: ListUsersQuery,
    ) -> Result<Page<IdpUser>, CanonicalError> {
        let ids = query_ids(&query);
        self.started.fetch_add(1, Ordering::SeqCst);
        if self.fast_id.is_some_and(|id| ids.contains(&id)) {
            return Ok(page(
                ids.into_iter()
                    .map(|id| IdpUser::new(id, "available actor"))
                    .collect(),
                None,
            ));
        }
        let _cancelled = Cancelled(&self.cancelled);
        std::future::pending().await
    }
}

#[tokio::test(start_paused = true)]
async fn one_budget_over_two_seconds_bounds_parallelism_and_cancels_outstanding_reads() {
    assert_eq!(LOOKUP_BUDGET, std::time::Duration::from_secs(2));
    let directory = Arc::new(HungDirectory::default());
    let service = names(directory.clone());
    let start = tokio::time::Instant::now();
    let resolved = service
        .resolve(
            &SecurityContext::anonymous(),
            (1..=4000).map(Uuid::from_u128),
        )
        .await;
    assert_eq!(resolved.len(), 4000);
    assert!(
        resolved
            .values()
            .all(|name| *name == ActorName::Unavailable)
    );
    assert_eq!(directory.started.load(Ordering::SeqCst), LOOKUP_CONCURRENCY);
    assert_eq!(
        directory.cancelled.load(Ordering::SeqCst),
        LOOKUP_CONCURRENCY
    );
    // The timer wheel may round the shared deadline up by one millisecond.
    assert!(start.elapsed() <= LOOKUP_BUDGET + std::time::Duration::from_millis(1));
}

#[tokio::test(start_paused = true)]
async fn completed_names_survive_other_actors_timing_out() {
    let id = Uuid::from_u128(1);
    let directory = Arc::new(HungDirectory {
        fast_id: Some(id),
        ..Default::default()
    });
    let service = names(directory.clone());
    let resolved = service
        .resolve(
            &SecurityContext::anonymous(),
            (1..=4000).map(Uuid::from_u128),
        )
        .await;
    assert_eq!(resolved[&id], ActorName::Resolved("available actor".into()));
    assert_eq!(
        resolved
            .values()
            .filter(|name| **name == ActorName::Unavailable)
            .count(),
        4000 - LOOKUP_BATCH_SIZE
    );
    assert_eq!(
        directory.started.load(Ordering::SeqCst),
        LOOKUP_CONCURRENCY + 1
    );
    assert_eq!(
        directory.cancelled.load(Ordering::SeqCst),
        LOOKUP_CONCURRENCY
    );
}

/// A response row: an optional actor and its name.
struct Row {
    actor: Option<Uuid>,
    actor_name: Option<String>,
}

impl ActorFields for Row {
    fn actor_ids(&self, ids: &mut Vec<Uuid>) {
        ids.extend(self.actor);
    }
    fn fill_names(&mut self, names: &BTreeMap<Uuid, ActorName>) {
        self.actor_name = self.actor.and_then(|id| label(names, id));
    }
}

/// A response document: one actor, a nested list and an optional nested row.
struct Card {
    by: Uuid,
    by_name: Option<String>,
    rows: Vec<Row>,
    extra: Option<Row>,
}

impl ActorFields for Card {
    fn actor_ids(&self, ids: &mut Vec<Uuid>) {
        ids.push(self.by);
        self.rows.actor_ids(ids);
        self.extra.actor_ids(ids);
    }
    fn fill_names(&mut self, names: &BTreeMap<Uuid, ActorName>) {
        self.by_name = label(names, self.by);
        self.rows.fill_names(names);
        self.extra.fill_names(names);
    }
}

fn row(actor: Option<Uuid>) -> Row {
    Row {
        actor,
        actor_name: Some("stale".into()),
    }
}

fn card(one: Uuid, two: Uuid, system: Uuid) -> Card {
    Card {
        by: one,
        by_name: None,
        rows: vec![row(Some(two)), row(Some(one)), row(None)],
        extra: Some(row(Some(system))),
    }
}

#[tokio::test]
async fn fill_reads_every_id_of_one_document_in_one_lookup() {
    let directory = Arc::new(Directory::default());
    let (one, two, system) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(9));
    {
        let mut users = directory.users.lock().unwrap();
        users.insert(one, IdpUser::new(one, "one"));
        users.insert(two, IdpUser::new(two, "two"));
    }
    let service = ActorNames::with_directory(directory.clone(), &[system]);
    let mut document = vec![card(one, two, system), card(two, one, system)];
    service
        .fill(&SecurityContext::anonymous(), &mut document)
        .await;
    assert_eq!(directory.calls(), [vec![one, two]]);
    assert_eq!(document[0].by_name.as_deref(), Some("one"));
    assert_eq!(document[1].by_name.as_deref(), Some("two"));
    let rows: Vec<_> = document[0]
        .rows
        .iter()
        .map(|r| r.actor_name.as_deref())
        .collect();
    assert_eq!(rows, [Some("two"), Some("one"), None]);
    assert_eq!(
        document[0].extra.as_ref().unwrap().actor_name.as_deref(),
        Some(SYSTEM_LABEL)
    );
}

#[tokio::test]
async fn fill_leaves_every_name_null_when_the_directory_fails() {
    let (one, two) = (Uuid::from_u128(1), Uuid::from_u128(2));
    let service = names(paged_directory(vec![Err(
        CanonicalError::service_unavailable().create(),
    )]));
    let mut document = card(one, two, Uuid::from_u128(9));
    service
        .fill(&SecurityContext::anonymous(), &mut document)
        .await;
    assert_eq!(document.by_name, None);
    assert!(document.rows.iter().all(|r| r.actor_name.is_none()));
    assert_eq!(document.extra.unwrap().actor_name, None);
}

#[tokio::test]
async fn fill_without_ids_reads_nothing() {
    let directory = Arc::new(Directory::default());
    let service = names(directory.clone());
    let mut rows = vec![row(None), row(None)];
    service.fill(&SecurityContext::anonymous(), &mut rows).await;
    assert!(directory.calls().is_empty());
    assert!(rows.iter().all(|r| r.actor_name.is_none()));
}

#[test]
fn queried_ids_reads_the_id_set_of_a_lookup_and_nothing_else() {
    let (one, two) = (Uuid::from_u128(1), Uuid::from_u128(2));
    let query = ListUsersQuery::with_ids([two, one, two]).unwrap();
    assert_eq!(queried_ids(&query), [one, two]);
    assert!(queried_ids(&ListUsersQuery::default()).is_empty());
}

/// A response shape that names its actors through the macro.
struct Header {
    by: Uuid,
    by_name: Option<String>,
    rows: Vec<Row>,
}

crate::actor_fields!(Header { by => by_name } [rows]);

#[tokio::test]
async fn the_macro_implements_the_fields_of_an_id_and_its_nested_values() {
    let directory = Arc::new(Directory::default());
    let (one, two) = (Uuid::from_u128(1), Uuid::from_u128(2));
    {
        let mut users = directory.users.lock().unwrap();
        users.insert(one, IdpUser::new(one, "one"));
        users.insert(two, IdpUser::new(two, "two"));
    }
    let service = names(directory.clone());
    let mut header = Header {
        by: one,
        by_name: None,
        rows: vec![row(Some(two)), row(None)],
    };
    let mut ids = Vec::new();
    header.actor_ids(&mut ids);
    assert_eq!(ids, [one, two]);
    service
        .fill(&SecurityContext::anonymous(), &mut header)
        .await;
    assert_eq!(header.by_name.as_deref(), Some("one"));
    assert_eq!(header.rows[0].actor_name.as_deref(), Some("two"));
    assert_eq!(header.rows[1].actor_name, None);
    assert_eq!(directory.calls().len(), 1);
}

/// A handler's future must be `Send`: `resolve` and `fill` are awaited inside one, so a spawned
/// task proves their futures are.
#[tokio::test]
async fn resolve_and_fill_futures_are_send() {
    let directory = Arc::new(Directory::default());
    let service = names(directory.clone());
    let id = Uuid::from_u128(1);
    let task = tokio::spawn(async move {
        let ctx = SecurityContext::anonymous();
        let resolved = service.resolve(&ctx, [id]).await;
        let mut rows = vec![row(Some(id))];
        service.fill(&ctx, &mut rows).await;
        resolved
    });
    assert_eq!(task.await.unwrap()[&id], ActorName::NotFound);
}

/// AM itself as a double: it records the subject and tenant of the context and the tenant of each
/// user read, and answers the asked ids. Every other operation is refused: the names call none.
#[derive(Default)]
struct RecordingAm {
    reads: Mutex<Vec<(Uuid, Uuid, Uuid)>>,
}

impl RecordingAm {
    fn reads(&self) -> Vec<(Uuid, Uuid, Uuid)> {
        self.reads.lock().unwrap().clone()
    }
}

/// The refusal of every operation the actor names never call.
fn not_called<T>() -> Result<T, CanonicalError> {
    Err(CanonicalError::internal("the actor names call only list_users").create())
}

#[async_trait]
impl AccountManagementClient for RecordingAm {
    async fn create_tenant(
        &self,
        _: &SecurityContext,
        _: am::CreateTenantRequest,
    ) -> Result<am::Tenant, CanonicalError> {
        not_called()
    }
    async fn get_tenant(&self, _: &SecurityContext, _: Uuid) -> Result<am::Tenant, CanonicalError> {
        not_called()
    }
    async fn list_children(
        &self,
        _: &SecurityContext,
        _: Uuid,
        _: &toolkit_odata::ODataQuery,
    ) -> Result<Page<am::Tenant>, CanonicalError> {
        not_called()
    }
    async fn update_tenant(
        &self,
        _: &SecurityContext,
        _: Uuid,
        _: am::UpdateTenantRequest,
    ) -> Result<am::Tenant, CanonicalError> {
        not_called()
    }
    async fn suspend_tenant(
        &self,
        _: &SecurityContext,
        _: Uuid,
    ) -> Result<am::Tenant, CanonicalError> {
        not_called()
    }
    async fn unsuspend_tenant(
        &self,
        _: &SecurityContext,
        _: Uuid,
    ) -> Result<am::Tenant, CanonicalError> {
        not_called()
    }
    async fn delete_tenant(
        &self,
        _: &SecurityContext,
        _: Uuid,
    ) -> Result<am::Tenant, CanonicalError> {
        not_called()
    }
    async fn create_user(
        &self,
        _: &SecurityContext,
        _: Uuid,
        _: am::IdpNewUser,
    ) -> Result<IdpUser, CanonicalError> {
        not_called()
    }
    async fn get_user(
        &self,
        _: &SecurityContext,
        _: Uuid,
        _: Uuid,
    ) -> Result<IdpUser, CanonicalError> {
        not_called()
    }
    async fn list_users(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        query: ListUsersQuery,
    ) -> Result<Page<IdpUser>, CanonicalError> {
        self.reads
            .lock()
            .unwrap()
            .push((ctx.subject_id(), ctx.subject_tenant_id(), tenant_id));
        Ok(page(
            query_ids(&query)
                .into_iter()
                .map(|id| IdpUser::new(id, "from am"))
                .collect(),
            None,
        ))
    }
    async fn delete_user(
        &self,
        _: &SecurityContext,
        _: Uuid,
        _: Uuid,
    ) -> Result<(), CanonicalError> {
        not_called()
    }
    async fn update_user(
        &self,
        _: &SecurityContext,
        _: Uuid,
        _: Uuid,
        _: am::IdpUserPatch,
    ) -> Result<IdpUser, CanonicalError> {
        not_called()
    }
    async fn create_service_account(
        &self,
        _: &SecurityContext,
        _: Uuid,
        _: String,
        _: Vec<String>,
    ) -> Result<am::IdpServiceAccountCredentials, CanonicalError> {
        not_called()
    }
    async fn list_service_accounts(
        &self,
        _: &SecurityContext,
        _: Uuid,
    ) -> Result<Vec<am::IdpServiceAccountSummary>, CanonicalError> {
        not_called()
    }
    async fn rotate_service_account_secret(
        &self,
        _: &SecurityContext,
        _: Uuid,
        _: &str,
    ) -> Result<am::IdpServiceAccountCredentials, CanonicalError> {
        not_called()
    }
    async fn revoke_service_account(
        &self,
        _: &SecurityContext,
        _: Uuid,
        _: &str,
    ) -> Result<(), CanonicalError> {
        not_called()
    }
    async fn get_metadata(
        &self,
        _: &SecurityContext,
        _: Uuid,
        _: gts::GtsTypeId,
    ) -> Result<am::MetadataEntry, CanonicalError> {
        not_called()
    }
    async fn resolve_metadata(
        &self,
        _: &SecurityContext,
        _: Uuid,
        _: gts::GtsTypeId,
    ) -> Result<Option<am::MetadataEntry>, CanonicalError> {
        not_called()
    }
    async fn list_metadata(
        &self,
        _: &SecurityContext,
        _: Uuid,
        _: &toolkit_odata::ODataQuery,
    ) -> Result<Page<am::MetadataEntry>, CanonicalError> {
        not_called()
    }
    async fn upsert_metadata(
        &self,
        _: &SecurityContext,
        _: Uuid,
        _: am::UpsertMetadataRequest,
    ) -> Result<am::MetadataEntry, CanonicalError> {
        not_called()
    }
    async fn delete_metadata(
        &self,
        _: &SecurityContext,
        _: Uuid,
        _: gts::GtsTypeId,
    ) -> Result<(), CanonicalError> {
        not_called()
    }
}

/// The adapter over the hub's AM reads with the caller's own context, in the caller's own tenant:
/// no privileged context and no other tenant.
#[tokio::test]
async fn the_am_adapter_reads_with_the_callers_own_context_and_tenant() {
    let am = Arc::new(RecordingAm::default());
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn AccountManagementClient>(am.clone());
    let service = ActorNames::from_hub(hub, &[]);
    let (subject, tenant) = (Uuid::from_u128(0x5b), Uuid::from_u128(0x7e));
    let ctx = SecurityContext::builder()
        .subject_id(subject)
        .subject_tenant_id(tenant)
        .build()
        .unwrap();
    let id = Uuid::from_u128(1);
    let resolved = service.resolve(&ctx, [id]).await;
    assert_eq!(resolved[&id], ActorName::Resolved("from am".into()));
    assert_eq!(am.reads(), [(subject, tenant, tenant)]);
}
