//! The actor name resolver over a fake directory: the page's one lookup pass,
//! its per-tenant queries, the chunking, the budget, the per-caller cache and the
//! pass's metric.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use account_management_sdk::{
    self as am, AccountManagementClient, IdpUser, IdpUserFilterField, IdpUserPagination,
    ListUsersQuery,
};
use async_trait::async_trait;
use toolkit::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::filter::{FilterNode, ODataValue};
use toolkit_odata::{Page, PageInfo};
use toolkit_security::SecurityContext;
use tracing_test::traced_test;
use uuid::Uuid;

use super::{
    ActorDirectory, ActorNameResolver, ActorRef, LOOKUP_BUDGET, LOOKUP_CONCURRENCY,
    NAME_CACHE_CAPACITY, NAME_TTL,
};
use crate::domain::ports::ActorNameMetrics;
use crate::domain::resolution::MASK_TOKEN;

/// The resource AM attributes a refusal to.
#[toolkit_canonical_errors::resource_error(toolkit_gts::gts_id!("cf.core.am.user.v1~"))]
struct UserResource;

/// The most ids one lookup carries.
const CHUNK: usize = IdpUserPagination::MAX_TOP as usize;

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

/// The caller's tenant in these tests.
fn home() -> Uuid {
    id(0xc0)
}

/// An authenticated caller: `subject` in `tenant`.
fn caller(tenant: Uuid, subject: u128) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(id(subject))
        .subject_tenant_id(tenant)
        .subject_type("gts.cf.core.security.subject_user.v1~")
        .token_scopes(vec!["*".to_owned()])
        .build()
        .expect("an authenticated context builds")
}

fn ctx() -> SecurityContext {
    caller(home(), 0x5b)
}

/// A page record as the tests see it: the resolver's input and the name it was given.
struct Rec {
    actor_tenant_id: Option<Uuid>,
    actor: String,
    actor_masked: bool,
    actor_name: Option<String>,
}

/// The resolver over [`Rec`]s: the names of the page, set on each record.
trait FillExt {
    async fn fill(&self, ctx: &SecurityContext, records: &mut [Rec]);
}

impl FillExt for ActorNameResolver {
    async fn fill(&self, ctx: &SecurityContext, records: &mut [Rec]) {
        let refs: Vec<ActorRef> = records
            .iter()
            .map(|r| ActorRef {
                actor_tenant_id: r.actor_tenant_id,
                actor: r.actor.clone(),
                masked: r.actor_masked,
            })
            .collect();
        let names = self.names_for_page(ctx, &refs).await;
        assert_eq!(names.len(), records.len());
        for (record, name) in records.iter_mut().zip(names) {
            record.actor_name = name.as_str().map(str::to_owned);
        }
    }
}

/// An unmasked record by `actor`, who acted from `tenant` (`None` when none was recorded).
fn record(actor: &str, tenant: Option<Uuid>) -> Rec {
    Rec {
        actor_tenant_id: tenant,
        actor: actor.to_owned(),
        actor_masked: false,
        actor_name: None,
    }
}

/// A record by the user `n`, acting from the caller's tenant.
fn by(n: u128) -> Rec {
    record(&id(n).to_string(), Some(home()))
}

/// A record whose actor the caller may not see.
fn masked(mut r: Rec) -> Rec {
    r.actor = MASK_TOKEN.to_owned();
    r.actor_masked = true;
    r
}

fn names_of(records: &[Rec]) -> Vec<Option<&str>> {
    records.iter().map(|r| r.actor_name.as_deref()).collect()
}

/// The ids of an id-set query, or a failure for any other shape.
fn query_ids(query: &ListUsersQuery) -> Vec<Uuid> {
    let Some(FilterNode::InList {
        field: IdpUserFilterField::Id,
        values,
    }) = &query.filter
    else {
        panic!("expected a typed id-set lookup");
    };
    values
        .iter()
        .map(|v| match v {
            ODataValue::Uuid(id) => *id,
            _ => panic!("expected a UUID"),
        })
        .collect()
}

fn page(items: Vec<IdpUser>) -> Page<IdpUser> {
    Page::new(
        items,
        PageInfo {
            next_cursor: None,
            prev_cursor: None,
            limit: 200,
        },
    )
}

/// What a pass reported, by result.
#[derive(Default)]
struct Metrics {
    passes: Mutex<Vec<(&'static str, Duration)>>,
}

impl ActorNameMetrics for Metrics {
    fn pass(&self, result: &'static str, duration: Duration) {
        self.passes.lock().unwrap().push((result, duration));
    }
}

impl Metrics {
    fn results(&self) -> Vec<&'static str> {
        self.passes.lock().unwrap().iter().map(|p| p.0).collect()
    }
}

/// A user directory by tenant: it records each query as `(tenant, ids)` and answers per script.
#[derive(Default)]
struct Directory {
    calls: Mutex<Vec<(Uuid, Vec<Uuid>)>>,
    /// The users each tenant lists, by display name.
    users: Mutex<BTreeMap<(Uuid, Uuid), IdpUser>>,
    /// Tenants AM refuses to this caller.
    refused: Mutex<BTreeSet<Uuid>>,
    /// Tenants whose lookups fail, always.
    failing: Mutex<BTreeSet<Uuid>>,
    /// Tenants whose first lookup fails, and later ones answer.
    fail_first: Mutex<BTreeSet<Uuid>>,
    in_flight: AtomicUsize,
    peak: AtomicUsize,
}

impl Directory {
    fn lists(&self, tenant: Uuid, user: u128, name: &str) {
        self.users.lock().unwrap().insert(
            (tenant, id(user)),
            IdpUser::new(id(user), "login").with_display_name(name),
        );
    }

    fn calls(&self) -> Vec<(Uuid, Vec<Uuid>)> {
        self.calls.lock().unwrap().clone()
    }

    fn sorted_calls(&self) -> Vec<(Uuid, Vec<Uuid>)> {
        let mut calls = self.calls();
        for (_, ids) in &mut calls {
            ids.sort_unstable();
        }
        calls
    }
}

#[async_trait]
impl ActorDirectory for Directory {
    async fn list_users(
        &self,
        _ctx: &SecurityContext,
        tenant_id: Uuid,
        query: ListUsersQuery,
    ) -> Result<Page<IdpUser>, CanonicalError> {
        let ids = query_ids(&query);
        assert!(ids.len() <= CHUNK);
        self.calls.lock().unwrap().push((tenant_id, ids.clone()));
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        // Let the other queued lookups start before this one answers.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        if self.refused.lock().unwrap().contains(&tenant_id) {
            return Err(UserResource::permission_denied()
                .with_reason("DENIED")
                .create());
        }
        if self.failing.lock().unwrap().contains(&tenant_id)
            || self.fail_first.lock().unwrap().remove(&tenant_id)
        {
            return Err(CanonicalError::internal("provider detail text").create());
        }
        let users = self.users.lock().unwrap();
        Ok(page(
            ids.iter()
                .filter_map(|i| users.get(&(tenant_id, *i)).cloned())
                .collect(),
        ))
    }
}

fn resolver(directory: &Arc<Directory>) -> (ActorNameResolver, Arc<Metrics>) {
    let metrics = Arc::new(Metrics::default());
    let service = ActorNameResolver::with_directory(directory.clone(), metrics.clone());
    (service, metrics)
}

// ── What is looked up ────────────────────────────────────────────────────────

#[tokio::test]
async fn a_page_of_users_makes_one_lookup_over_the_deduplicated_ids() {
    let d = Arc::new(Directory::default());
    d.lists(home(), 1, "Ann");
    d.lists(home(), 2, "Bob");
    let (service, metrics) = resolver(&d);
    let mut records = vec![by(1), by(2), by(1), by(1)];
    service.fill(&ctx(), &mut records).await;
    assert_eq!(
        names_of(&records),
        [Some("Ann"), Some("Bob"), Some("Ann"), Some("Ann")]
    );
    assert_eq!(d.sorted_calls(), [(home(), vec![id(1), id(2)])]);
    assert_eq!(metrics.results(), ["ok"]);
}

#[tokio::test]
async fn a_masked_actor_reads_no_name_and_is_never_looked_up() {
    let d = Arc::new(Directory::default());
    let (service, metrics) = resolver(&d);
    let mut records = vec![
        masked(by(1)),
        masked(record(&Uuid::nil().to_string(), None)),
    ];
    // The flag decides, whatever the actor text is.
    records[0].actor = id(1).to_string();
    service.fill(&ctx(), &mut records).await;
    assert_eq!(names_of(&records), [None, None]);
    assert!(d.calls().is_empty());
    assert_eq!(metrics.results(), ["skipped"]);
}

#[tokio::test]
async fn a_module_actor_reads_no_name_and_is_never_looked_up() {
    let d = Arc::new(Directory::default());
    let (service, metrics) = resolver(&d);
    let mut records = vec![
        record("gears.billing", Some(home())),
        record("not-a-uuid", None),
    ];
    service.fill(&ctx(), &mut records).await;
    assert_eq!(names_of(&records), [None, None]);
    assert!(d.calls().is_empty());
    assert_eq!(metrics.results(), ["skipped"]);
}

#[tokio::test]
async fn the_nil_actor_reads_system_without_a_lookup() {
    let d = Arc::new(Directory::default());
    d.lists(home(), 1, "Ann");
    let (service, metrics) = resolver(&d);
    let mut records = vec![record(&Uuid::nil().to_string(), Some(home())), by(1)];
    service.fill(&ctx(), &mut records).await;
    assert_eq!(names_of(&records), [Some("System"), Some("Ann")]);
    assert_eq!(d.calls(), [(home(), vec![id(1)])]);
    assert_eq!(metrics.results(), ["ok"]);
}

#[tokio::test]
async fn a_page_with_only_the_nil_actor_makes_no_lookup_and_is_skipped() {
    let d = Arc::new(Directory::default());
    let (service, metrics) = resolver(&d);
    let mut records = vec![record(&Uuid::nil().to_string(), None)];
    service.fill(&ctx(), &mut records).await;
    assert_eq!(names_of(&records), [Some("System")]);
    assert!(d.calls().is_empty());
    assert_eq!(metrics.results(), ["skipped"]);
}

#[tokio::test]
async fn an_empty_page_is_skipped() {
    let d = Arc::new(Directory::default());
    let (service, metrics) = resolver(&d);
    service.fill(&ctx(), &mut []).await;
    assert!(d.calls().is_empty());
    assert_eq!(metrics.results(), ["skipped"]);
}

// ── Chunks and concurrency ───────────────────────────────────────────────────

#[tokio::test]
async fn more_than_two_hundred_actors_are_read_in_chunks_of_two_hundred() {
    let d = Arc::new(Directory::default());
    for n in 1..=450 {
        d.lists(home(), n, "actor");
    }
    let (service, _) = resolver(&d);
    let mut records: Vec<_> = (1..=450).map(by).collect();
    service.fill(&ctx(), &mut records).await;
    assert!(
        records
            .iter()
            .all(|r| r.actor_name.as_deref() == Some("actor"))
    );
    let mut sizes: Vec<_> = d.calls().iter().map(|(_, ids)| ids.len()).collect();
    sizes.sort_unstable();
    assert_eq!(sizes, [50, 200, 200]);
}

#[tokio::test]
async fn at_most_four_chunks_are_in_flight_at_once() {
    let d = Arc::new(Directory::default());
    let (service, _) = resolver(&d);
    let mut records: Vec<_> = (1..=2000).map(by).collect();
    service.fill(&ctx(), &mut records).await;
    assert_eq!(d.calls().len(), 10);
    assert_eq!(d.peak.load(Ordering::SeqCst), LOOKUP_CONCURRENCY);
}

// ── Tenants ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn each_actor_is_asked_for_in_the_tenant_it_acted_from() {
    let d = Arc::new(Directory::default());
    let child = id(0xc1);
    d.lists(child, 1, "Ann");
    d.lists(home(), 2, "Bob");
    let (service, metrics) = resolver(&d);
    let mut records = vec![
        record(&id(1).to_string(), Some(child)),
        record(&id(2).to_string(), Some(home())),
    ];
    service.fill(&ctx(), &mut records).await;
    assert_eq!(names_of(&records), [Some("Ann"), Some("Bob")]);
    let mut calls = d.calls();
    calls.sort();
    let mut expected = vec![(child, vec![id(1)]), (home(), vec![id(2)])];
    expected.sort();
    assert_eq!(calls, expected);
    assert_eq!(metrics.results(), ["ok"]);
}

#[tokio::test]
async fn an_actor_its_tenant_does_not_list_reads_null_and_is_asked_nowhere_else() {
    // An ancestor administrator is listed in its own tenant, which the record names.
    let d = Arc::new(Directory::default());
    let child = id(0xc1);
    d.lists(child, 1, "Ann");
    d.lists(home(), 2, "Bob");
    let (service, metrics) = resolver(&d);
    let mut records = vec![
        record(&id(1).to_string(), Some(child)),
        record(&id(2).to_string(), Some(child)),
    ];
    service.fill(&ctx(), &mut records).await;
    assert_eq!(names_of(&records), [Some("Ann"), None]);
    assert_eq!(d.sorted_calls(), [(child, vec![id(1), id(2)])]);
    assert_eq!(metrics.results(), ["ok"]);
}

#[tokio::test]
async fn a_record_without_an_actor_tenant_reads_null_and_is_never_looked_up() {
    let d = Arc::new(Directory::default());
    d.lists(home(), 1, "Ann");
    d.lists(home(), 2, "Bob");
    let (service, metrics) = resolver(&d);
    let mut records = vec![by(1), record(&id(2).to_string(), None)];
    service.fill(&ctx(), &mut records).await;
    assert_eq!(names_of(&records), [Some("Ann"), None]);
    assert_eq!(d.calls(), [(home(), vec![id(1)])]);
    assert_eq!(metrics.results(), ["ok"]);

    let mut alone = vec![record(&id(2).to_string(), None)];
    service.fill(&ctx(), &mut alone).await;
    assert_eq!(names_of(&alone), [None]);
    assert_eq!(d.calls().len(), 1, "no lookup for it");
    assert_eq!(metrics.results(), ["ok", "skipped"]);
}

// ── Refusals, absences and failures ──────────────────────────────────────────

#[tokio::test]
#[traced_test]
async fn a_refused_or_absent_actor_reads_null_and_is_an_answer() {
    let d = Arc::new(Directory::default());
    d.refused.lock().unwrap().insert(home());
    let (service, metrics) = resolver(&d);
    let mut records = vec![by(1), by(2)];
    service.fill(&ctx(), &mut records).await;
    assert_eq!(names_of(&records), [None, None]);
    // A whole chunk refused: AM's own answer, no warn line.
    assert_eq!(metrics.results(), ["ok"]);
    assert!(!logs_contain("WARN"));

    d.refused.lock().unwrap().clear();
    let mut again = vec![by(1)];
    service.fill(&ctx(), &mut again).await;
    assert_eq!(names_of(&again), [None], "not found is an answer too");
    assert_eq!(metrics.results(), ["ok", "ok"]);
    assert!(!logs_contain("WARN"));
}

#[tokio::test]
#[traced_test]
async fn a_failed_chunk_reads_null_warns_once_and_degrades_the_pass() {
    let d = Arc::new(Directory::default());
    d.failing.lock().unwrap().insert(home());
    let (service, metrics) = resolver(&d);
    let mut records = vec![by(1), by(2)];
    service.fill(&ctx(), &mut records).await;
    assert_eq!(names_of(&records), [None, None]);
    assert_eq!(metrics.results(), ["degraded"]);
    assert!(logs_contain("reason=\"directory\" ids=2"));
    assert!(!logs_contain(&id(1).to_string()));
    assert!(!logs_contain(&id(2).to_string()));
    assert!(!logs_contain("provider detail text"));
}

#[tokio::test]
async fn a_failed_chunk_is_not_retried() {
    let d = Arc::new(Directory::default());
    d.fail_first.lock().unwrap().insert(home());
    d.lists(home(), 1, "Ann");
    let (service, metrics) = resolver(&d);
    let mut records = vec![by(1)];
    service.fill(&ctx(), &mut records).await;
    assert_eq!(names_of(&records), [None]);
    assert_eq!(d.calls().len(), 1);
    assert_eq!(metrics.results(), ["degraded"]);
}

// ── The budget ───────────────────────────────────────────────────────────────

/// A directory that never answers.
#[derive(Default)]
struct HungDirectory {
    started: AtomicUsize,
    fast: Option<Uuid>,
}

#[async_trait]
impl ActorDirectory for HungDirectory {
    async fn list_users(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: Uuid,
        query: ListUsersQuery,
    ) -> Result<Page<IdpUser>, CanonicalError> {
        let ids = query_ids(&query);
        self.started.fetch_add(1, Ordering::SeqCst);
        if let Some(fast) = self.fast.filter(|f| ids.contains(f)) {
            // Only the fast actor is listed; the rest of its chunk is absent.
            return Ok(page(
                ids.into_iter()
                    .filter(|i| *i == fast)
                    .map(|i| IdpUser::new(i, "quick").with_display_name("Quick"))
                    .collect(),
            ));
        }
        std::future::pending().await
    }
}

#[tokio::test(start_paused = true)]
#[traced_test]
async fn a_hung_directory_leaves_the_names_null_within_one_second() {
    assert_eq!(LOOKUP_BUDGET, Duration::from_secs(1));
    let d = Arc::new(HungDirectory::default());
    let metrics = Arc::new(Metrics::default());
    let service = ActorNameResolver::with_directory(d.clone(), metrics.clone());
    let mut records = vec![by(1), record(&id(2).to_string(), Some(id(0xc1)))];
    let start = tokio::time::Instant::now();
    service.fill(&ctx(), &mut records).await;
    assert_eq!(names_of(&records), [None, None]);
    // The timer wheel may round the deadline up by a millisecond.
    assert!(start.elapsed() <= LOOKUP_BUDGET + Duration::from_millis(1));
    // Both tenants' chunks were started, and neither answered in time.
    assert_eq!(d.started.load(Ordering::SeqCst), 2);
    assert_eq!(metrics.results(), ["degraded"]);
    assert!(logs_contain("reason=\"budget\""));
}

#[tokio::test(start_paused = true)]
async fn a_client_slower_than_the_budget_keeps_the_names_it_did_answer() {
    let quick = id(1);
    let d = Arc::new(HungDirectory {
        fast: Some(quick),
        ..Default::default()
    });
    let metrics = Arc::new(Metrics::default());
    let service = ActorNameResolver::with_directory(d, metrics.clone());
    // 251 ids sort into two chunks: the fast id leads chunk 1, which answers it and lists
    // none of the 199 others; chunk 2 holds the 51 remaining slow ids and never answers.
    let mut records: Vec<_> = std::iter::once(by(1)).chain((1000..1250).map(by)).collect();
    service.fill(&ctx(), &mut records).await;
    assert_eq!(records[0].actor_name.as_deref(), Some("Quick"));
    assert!(records[1..].iter().all(|r| r.actor_name.is_none()));
    assert_eq!(metrics.results(), ["degraded"]);
}

// ── The cache ────────────────────────────────────────────────────────────────

fn cached(d: &Arc<Directory>) -> (ActorNameResolver, Arc<Metrics>) {
    let (service, metrics) = resolver(d);
    (service.caching_for(NAME_TTL), metrics)
}

#[tokio::test(start_paused = true)]
async fn a_name_is_reused_for_its_caller_until_it_expires() {
    let d = Arc::new(Directory::default());
    d.lists(home(), 1, "before");
    let (service, metrics) = cached(&d);
    let mut first = vec![by(1)];
    service.fill(&ctx(), &mut first).await;
    assert_eq!(names_of(&first), [Some("before")]);

    d.lists(home(), 1, "after");
    let mut second = vec![by(1)];
    service.fill(&ctx(), &mut second).await;
    assert_eq!(names_of(&second), [Some("before")]);
    assert_eq!(d.calls().len(), 1, "the second read made no lookup");
    // An all-cache-hit pass is `ok`.
    assert_eq!(metrics.results(), ["ok", "ok"]);

    tokio::time::advance(NAME_TTL + Duration::from_secs(1)).await;
    let mut third = vec![by(1)];
    service.fill(&ctx(), &mut third).await;
    assert_eq!(names_of(&third), [Some("after")]);
    assert_eq!(d.calls().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn another_subject_reads_the_directory_itself() {
    let d = Arc::new(Directory::default());
    d.lists(home(), 1, "Ann");
    let (service, _) = cached(&d);
    let mut one = vec![by(1)];
    service.fill(&caller(home(), 0xa1), &mut one).await;
    let mut other_subject = vec![by(1)];
    service
        .fill(&caller(home(), 0xa2), &mut other_subject)
        .await;
    let mut other_tenant = vec![by(1)];
    service
        .fill(&caller(id(0xc9), 0xa1), &mut other_tenant)
        .await;
    assert_eq!(d.calls().len(), 3);
}

#[tokio::test(start_paused = true)]
async fn a_refused_absent_or_failed_actor_is_not_cached() {
    let d = Arc::new(Directory::default());
    let (service, _) = cached(&d);
    let mut absent = vec![by(1)];
    service.fill(&ctx(), &mut absent).await;
    assert_eq!(names_of(&absent), [None]);
    // The user becomes visible: the next read shows the name.
    d.lists(home(), 1, "Ann");
    let mut again = vec![by(1)];
    service.fill(&ctx(), &mut again).await;
    assert_eq!(names_of(&again), [Some("Ann")]);
    assert_eq!(d.calls().len(), 2);

    d.refused.lock().unwrap().insert(home());
    d.users.lock().unwrap().clear();
    tokio::time::advance(NAME_TTL + Duration::from_secs(1)).await;
    let mut refused = vec![by(1)];
    service.fill(&ctx(), &mut refused).await;
    d.refused.lock().unwrap().clear();
    d.failing.lock().unwrap().insert(home());
    let mut failed = vec![by(1)];
    service.fill(&ctx(), &mut failed).await;
    assert_eq!(d.calls().len(), 4, "neither answer was kept");
}

#[tokio::test(start_paused = true)]
async fn an_anonymous_context_makes_no_call_caches_nothing_and_is_skipped() {
    let d = Arc::new(Directory::default());
    d.lists(home(), 1, "Ann");
    let (service, metrics) = cached(&d);
    let mut records = vec![by(1), record(&Uuid::nil().to_string(), None)];
    service
        .fill(&SecurityContext::anonymous(), &mut records)
        .await;
    assert_eq!(names_of(&records), [None, Some("System")]);
    assert!(d.calls().is_empty());
    assert_eq!(metrics.results(), ["skipped"]);
    // Nothing was kept for the next caller.
    let mut next = vec![by(1)];
    service.fill(&ctx(), &mut next).await;
    assert_eq!(d.calls().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn the_cache_holds_at_most_ten_thousand_names() {
    assert_eq!(NAME_CACHE_CAPACITY, 10_000);
    let d = Arc::new(Directory::default());
    for n in 1..=10_000 {
        d.lists(home(), n, "n");
    }
    d.lists(home(), 20_000, "extra");
    let (service, _) = cached(&d);
    let mut all: Vec<_> = (1..=10_000).map(by).collect();
    service.fill(&ctx(), &mut all).await;
    let after_fill = d.calls().len();
    let mut again: Vec<_> = (1..=10_000).map(by).collect();
    service.fill(&ctx(), &mut again).await;
    assert_eq!(d.calls().len(), after_fill, "a full cache still answers");

    // One more name overflows it, nothing is expired, so all are dropped.
    let mut extra = vec![by(20_000)];
    service.fill(&ctx(), &mut extra).await;
    let before = d.calls().len();
    let mut old = vec![by(1)];
    service.fill(&ctx(), &mut old).await;
    assert_eq!(d.calls().len(), before + 1, "the old names were dropped");
    let mut kept = vec![by(20_000)];
    service.fill(&ctx(), &mut kept).await;
    assert_eq!(d.calls().len(), before + 1, "the new name was kept");
}

#[tokio::test(start_paused = true)]
async fn a_full_cache_drops_its_expired_names_before_all() {
    let d = Arc::new(Directory::default());
    for n in 1..=12_000 {
        d.lists(home(), n, "n");
    }
    let (service, _) = cached(&d);
    // 6,000 names at t0, 4,000 at t0 + 200 s; at t0 + 350 s the first are expired.
    let mut old: Vec<_> = (1..=6_000).map(by).collect();
    service.fill(&ctx(), &mut old).await;
    tokio::time::advance(Duration::from_secs(200)).await;
    let mut mid: Vec<_> = (6_001..=10_000).map(by).collect();
    service.fill(&ctx(), &mut mid).await;
    tokio::time::advance(Duration::from_secs(150)).await;
    let mut fresh: Vec<_> = (10_001..=12_000).map(by).collect();
    service.fill(&ctx(), &mut fresh).await;
    let before = d.calls().len();
    // The 4,000 unexpired names survived the overflow.
    let mut survivors: Vec<_> = (6_001..=10_000).map(by).collect();
    service.fill(&ctx(), &mut survivors).await;
    assert_eq!(d.calls().len(), before, "the unexpired names were kept");
}

// ── The label ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_label_is_the_display_name_then_first_and_last_then_the_username() {
    let d = Arc::new(Directory::default());
    let user = |n: u128, login: &str| IdpUser::new(id(n), login);
    {
        let mut users = d.users.lock().unwrap();
        users.insert(
            (home(), id(1)),
            user(1, " alice ")
                .with_display_name("  Alice Jones  ")
                .with_first_name("A")
                .with_last_name("S"),
        );
        users.insert(
            (home(), id(2)),
            user(2, "alice")
                .with_display_name(" \t ")
                .with_first_name(" Alice ")
                .with_last_name(" Smith "),
        );
        users.insert((home(), id(3)), user(3, "alice").with_last_name(" Smith "));
        users.insert((home(), id(4)), user(4, "  alice ").with_display_name(" "));
        users.insert((home(), id(5)), user(5, " ").with_display_name(" "));
    }
    let (service, _) = resolver(&d);
    let mut records: Vec<_> = (1..=5).map(by).collect();
    service.fill(&ctx(), &mut records).await;
    assert_eq!(
        names_of(&records),
        [
            Some("Alice Jones"),
            Some("Alice Smith"),
            Some("Smith"),
            Some("alice"),
            None
        ]
    );
}

// ── Metrics ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn each_fill_reports_exactly_one_pass() {
    let d = Arc::new(Directory::default());
    d.lists(home(), 1, "Ann");
    let (service, metrics) = resolver(&d);
    for _ in 0..3 {
        service.fill(&ctx(), &mut [by(1)]).await;
    }
    service.fill(&ctx(), &mut [record("module", None)]).await;
    assert_eq!(metrics.results(), ["ok", "ok", "ok", "skipped"]);
}

// ── Pagination guards and error kinds ────────────────────────────────────────

/// One scripted query as `(tenant, ids, cursor)`.
type Call = (Uuid, Vec<Uuid>, Option<String>);

/// A directory that answers each query from a script: the next scripted answer, then an empty page.
#[derive(Default)]
struct Scripted {
    answers: Mutex<std::collections::VecDeque<Result<Page<IdpUser>, CanonicalError>>>,
    /// Each query asked.
    calls: Mutex<Vec<Call>>,
}

impl Scripted {
    fn with(answers: Vec<Result<Page<IdpUser>, CanonicalError>>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.into()),
            calls: Mutex::default(),
        })
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl ActorDirectory for Scripted {
    async fn list_users(
        &self,
        _ctx: &SecurityContext,
        tenant_id: Uuid,
        query: ListUsersQuery,
    ) -> Result<Page<IdpUser>, CanonicalError> {
        self.calls.lock().unwrap().push((
            tenant_id,
            query_ids(&query),
            query.pagination.cursor().map(str::to_owned),
        ));
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(page(vec![])))
    }
}

fn user(n: u128, name: &str) -> IdpUser {
    IdpUser::new(id(n), "login").with_display_name(name)
}

/// A page of `items` whose next page is at `cursor`.
fn page_at(items: Vec<IdpUser>, cursor: &str) -> Page<IdpUser> {
    let mut p = page(items);
    p.page_info.next_cursor = Some(cursor.to_owned());
    p
}

fn scripted_resolver(d: &Arc<Scripted>) -> (ActorNameResolver, Arc<Metrics>) {
    let metrics = Arc::new(Metrics::default());
    (
        ActorNameResolver::with_directory(d.clone(), metrics.clone()),
        metrics,
    )
}

#[tokio::test]
#[traced_test]
async fn two_pages_with_a_cursor_resolve_the_chunk_together() {
    let d = Scripted::with(vec![
        Ok(page_at(vec![user(1, "Ann")], "c1")),
        Ok(page(vec![user(2, "Bob")])),
    ]);
    let (service, metrics) = scripted_resolver(&d);
    let mut records = vec![by(1), by(2)];
    service.fill(&ctx(), &mut records).await;
    assert_eq!(names_of(&records), [Some("Ann"), Some("Bob")]);
    assert_eq!(metrics.results(), ["ok"]);
    let calls = d.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].2, None);
    assert_eq!(calls[1].2.as_deref(), Some("c1"));
    assert!(!logs_contain("WARN"));
}

#[tokio::test]
#[traced_test]
async fn a_page_with_an_unrequested_id_is_discarded_and_degrades() {
    let d = Scripted::with(vec![Ok(page(vec![user(1, "Ann"), user(99, "Foreign")]))]);
    let (service, metrics) = scripted_resolver(&d);
    let mut records = vec![by(1), by(2)];
    service.fill(&ctx(), &mut records).await;
    assert_eq!(names_of(&records), [None, None]);
    assert_eq!(metrics.results(), ["degraded"]);
    assert!(logs_contain("reason=\"pagination\" ids=2"));
    assert!(!logs_contain("Foreign"));
}

#[tokio::test]
#[traced_test]
async fn a_page_with_a_duplicated_id_degrades() {
    let d = Scripted::with(vec![Ok(page(vec![user(1, "Ann"), user(1, "Ann again")]))]);
    let (service, metrics) = scripted_resolver(&d);
    let mut records = vec![by(1), by(2)];
    service.fill(&ctx(), &mut records).await;
    assert_eq!(names_of(&records), [None, None]);
    assert_eq!(metrics.results(), ["degraded"]);
    assert!(logs_contain("reason=\"pagination\" ids=2"));
}

#[tokio::test]
#[traced_test]
async fn an_empty_page_with_a_cursor_makes_no_progress_and_degrades() {
    let d = Scripted::with(vec![
        Ok(page_at(vec![], "c1")),
        Ok(page(vec![user(1, "Ann")])),
    ]);
    let (service, metrics) = scripted_resolver(&d);
    let mut records = vec![by(1)];
    service.fill(&ctx(), &mut records).await;
    assert_eq!(names_of(&records), [None]);
    assert_eq!(metrics.results(), ["degraded"]);
    assert_eq!(d.calls().len(), 1, "the chunk stops at the empty page");
    assert!(logs_contain("reason=\"pagination\" ids=1"));
}

#[tokio::test]
#[traced_test]
async fn a_cursor_returned_twice_degrades_without_looping() {
    let d = Scripted::with(vec![
        Ok(page_at(vec![user(1, "Ann")], "c1")),
        Ok(page_at(vec![user(2, "Bob")], "c1")),
        Ok(page_at(vec![user(3, "Cy")], "c1")),
        Ok(page(vec![user(4, "Di")])),
    ]);
    let (service, metrics) = scripted_resolver(&d);
    let mut records = vec![by(1), by(2), by(3), by(4)];
    service.fill(&ctx(), &mut records).await;
    assert_eq!(d.calls().len(), 2, "the repeated cursor ends the chunk");
    assert_eq!(names_of(&records), [Some("Ann"), Some("Bob"), None, None]);
    assert_eq!(metrics.results(), ["degraded"]);
    assert!(logs_contain("reason=\"pagination\" ids=2"));
}

#[tokio::test]
#[traced_test]
async fn a_refusal_of_any_kind_reads_null_is_an_answer_and_is_not_cached() {
    let kinds: [fn() -> CanonicalError; 3] = [
        || {
            UserResource::permission_denied()
                .with_reason("DENIED")
                .create()
        },
        || {
            CanonicalError::unauthenticated()
                .with_reason("EXPIRED")
                .create()
        },
        || {
            UserResource::not_found("no such user")
                .with_resource("user")
                .create()
        },
    ];
    for kind in kinds {
        let d = Scripted::with(vec![Err(kind()), Err(kind())]);
        let (service, metrics) = scripted_resolver(&d);
        let service = service.caching_for(NAME_TTL);
        let mut first = vec![by(1)];
        service.fill(&ctx(), &mut first).await;
        assert_eq!(names_of(&first), [None]);
        assert_eq!(metrics.results(), ["ok"]);
        // Not cached: the next read asks again.
        let mut second = vec![by(1)];
        service.fill(&ctx(), &mut second).await;
        assert_eq!(d.calls().len(), 2);
        assert!(!logs_contain("WARN"));
    }
}

#[tokio::test(start_paused = true)]
async fn a_cache_hit_and_a_miss_ask_directory_for_the_miss_only() {
    let d = Arc::new(Directory::default());
    d.lists(home(), 1, "Ann");
    d.lists(home(), 2, "Bob");
    let (service, metrics) = cached(&d);
    let mut warm = vec![by(1)];
    service.fill(&ctx(), &mut warm).await;
    let mut records = vec![by(1), by(2)];
    service.fill(&ctx(), &mut records).await;
    assert_eq!(names_of(&records), [Some("Ann"), Some("Bob")]);
    assert_eq!(
        d.calls(),
        [(home(), vec![id(1)]), (home(), vec![id(2)])],
        "the second query carries only the miss"
    );
    assert_eq!(metrics.results(), ["ok", "ok"]);
}

#[test]
fn a_poisoned_cache_lock_still_lets_a_later_read_and_write_proceed() {
    use super::NameCache;
    let cache = Arc::new(NameCache::default());
    let caller = (home(), id(0x5b));
    let held = cache.clone();
    let poisoned = std::thread::spawn(move || {
        let _guard = held.entries.lock().unwrap();
        panic!("poison the lock");
    })
    .join();
    assert!(poisoned.is_err());
    assert!(cache.entries.is_poisoned());
    let now = tokio::time::Instant::now();
    let ids = BTreeSet::from([id(1)]);
    assert!(cache.fresh(caller, &ids, now).is_empty());
    cache.keep(
        caller,
        &BTreeMap::from([(id(1), "Ann".to_owned())]),
        now,
        NAME_TTL,
    );
    assert_eq!(
        cache
            .fresh(caller, &ids, now)
            .get(&id(1))
            .map(String::as_str),
        Some("Ann")
    );
}

// ── The Account Management adapter ───────────────────────────────────────────

/// A page to the fake AM: it answers every asked id.
fn answered(query: &ListUsersQuery) -> Page<IdpUser> {
    page(
        query_ids(query)
            .into_iter()
            .map(|i| IdpUser::new(i, "login").with_display_name("from am"))
            .collect(),
    )
}

/// The refusal of every operation the resolver never calls.
fn not_called<T>() -> Result<T, CanonicalError> {
    Err(CanonicalError::internal("the resolver calls only list_users").create())
}

/// AM itself as a double: it records the subject and tenant of the context and the tenant of each
/// user read, and answers the asked ids. Every other operation is refused: the resolver calls none.
#[derive(Default)]
struct RecordingAm {
    reads: Mutex<Vec<(Uuid, Uuid, Uuid)>>,
}

impl RecordingAm {
    fn reads(&self) -> Vec<(Uuid, Uuid, Uuid)> {
        self.reads.lock().unwrap().clone()
    }
}

/// The [`AccountManagementClient`] of [`RecordingAm`]: `list_users` records and answers; each
/// operation listed as `name(arguments after the context) -> ok type` is refused.
macro_rules! recording_am {
    ($($name:ident($($arg:ty),*) -> $ok:ty;)*) => {
        #[async_trait]
        impl AccountManagementClient for RecordingAm {
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
                Ok(answered(&query))
            }
            $(
                async fn $name(
                    &self,
                    _: &SecurityContext,
                    $(_: $arg,)*
                ) -> Result<$ok, CanonicalError> {
                    not_called()
                }
            )*
        }
    };
}

recording_am! {
    create_tenant(am::CreateTenantRequest) -> am::Tenant;
    get_tenant(Uuid) -> am::Tenant;
    list_children(Uuid, &toolkit_odata::ODataQuery) -> Page<am::Tenant>;
    update_tenant(Uuid, am::UpdateTenantRequest) -> am::Tenant;
    suspend_tenant(Uuid) -> am::Tenant;
    unsuspend_tenant(Uuid) -> am::Tenant;
    delete_tenant(Uuid) -> am::Tenant;
    create_user(Uuid, am::IdpNewUser) -> IdpUser;
    get_user(Uuid, Uuid) -> IdpUser;
    delete_user(Uuid, Uuid) -> ();
    update_user(Uuid, Uuid, am::IdpUserPatch) -> IdpUser;
    create_service_account(Uuid, String, Vec<String>) -> am::IdpServiceAccountCredentials;
    list_service_accounts(Uuid) -> Vec<am::IdpServiceAccountSummary>;
    rotate_service_account_secret(Uuid, &str) -> am::IdpServiceAccountCredentials;
    revoke_service_account(Uuid, &str) -> ();
    get_metadata(Uuid, gts::GtsTypeId) -> am::MetadataEntry;
    resolve_metadata(Uuid, gts::GtsTypeId) -> Option<am::MetadataEntry>;
    list_metadata(Uuid, &toolkit_odata::ODataQuery) -> Page<am::MetadataEntry>;
    upsert_metadata(Uuid, am::UpsertMetadataRequest) -> am::MetadataEntry;
    delete_metadata(Uuid, gts::GtsTypeId) -> ();
}

#[tokio::test]
async fn the_am_adapter_reads_with_the_callers_own_context_and_the_actors_tenant() {
    let am = Arc::new(RecordingAm::default());
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn AccountManagementClient>(am.clone());
    let metrics = Arc::new(Metrics::default());
    let service = ActorNameResolver::from_hub(hub, metrics.clone());
    let (subject, tenant, child) = (id(0x5b), id(0x7e), id(0x7f));
    let context = caller(tenant, 0x5b);

    let mut records = vec![record(&id(1).to_string(), Some(child))];
    service.fill(&context, &mut records).await;
    assert_eq!(names_of(&records), [Some("from am")]);
    assert_eq!(am.reads(), [(subject, tenant, child)]);

    // A record without an actor tenant is read nowhere.
    let mut unrecorded = vec![record(&id(2).to_string(), None)];
    service.fill(&context, &mut unrecorded).await;
    assert_eq!(names_of(&unrecorded), [None]);
    assert_eq!(am.reads().len(), 1);
    assert_eq!(metrics.results(), ["ok", "skipped"]);
}

#[tokio::test]
async fn the_hub_binding_caches_names_for_the_caller() {
    let am = Arc::new(RecordingAm::default());
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn AccountManagementClient>(am.clone());
    let service = ActorNameResolver::from_hub(hub, Arc::new(Metrics::default()));
    for _ in 0..2 {
        service.fill(&caller(home(), 0x5b), &mut [by(1)]).await;
    }
    assert_eq!(am.reads().len(), 1);
}

#[tokio::test]
async fn a_hub_without_account_management_reads_every_user_name_null() {
    let metrics = Arc::new(Metrics::default());
    let service = ActorNameResolver::from_hub(Arc::new(ClientHub::new()), metrics.clone());
    let mut records = vec![
        by(1),
        record(&Uuid::nil().to_string(), None),
        record("module", None),
    ];
    service.fill(&ctx(), &mut records).await;
    assert_eq!(names_of(&records), [None, Some("System"), None]);
    assert_eq!(metrics.results(), ["skipped"]);
}

#[tokio::test]
async fn the_am_is_looked_up_at_each_call_so_registration_order_cannot_turn_names_off() {
    let hub = Arc::new(ClientHub::new());
    let service = ActorNameResolver::from_hub(hub.clone(), Arc::new(Metrics::default()));
    let mut before = vec![by(1)];
    service.fill(&ctx(), &mut before).await;
    assert_eq!(names_of(&before), [None]);
    hub.register::<dyn AccountManagementClient>(Arc::new(RecordingAm::default()));
    let mut after = vec![by(1)];
    service.fill(&ctx(), &mut after).await;
    assert_eq!(names_of(&after), [Some("from am")]);
}
