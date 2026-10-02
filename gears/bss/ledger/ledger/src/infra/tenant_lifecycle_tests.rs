//! Unit tests for [`ResolverTenantLifecycleReader`] and [`plan_lifecycle`] —
//! the registry anchor that decides which ledger-derived tenants a background
//! sweep may still touch, and which may have their runs reclaimed. A wrong
//! answer here is expensive in both directions: too generous and the sweep
//! keeps writing rows for tenants that no longer exist (the 29 GB
//! `ledger_reconciliation_run` defect); too strict and a live tenant silently
//! stops being reconciled — or has its runs purged.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::unimplemented,
    reason = "test doubles: unused trait methods are unreachable, assertions unwrap"
)]

use std::collections::HashSet;
use std::sync::Mutex;

use tenant_resolver_sdk::{
    GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions, GetDescendantsResponse,
    IsAncestorOptions, TenantInfo, TenantResolverError,
};

use super::*;

/// A registry double: knows a fixed `id -> status` map and records the size and
/// status filter of every batch it was asked about (so chunking and the
/// all-statuses query are observable).
struct FakeResolver {
    known: Vec<(Uuid, TenantStatus)>,
    batches: Mutex<Vec<usize>>,
    filters: Mutex<Vec<Vec<TenantStatus>>>,
    fail: bool,
}

impl FakeResolver {
    fn new(known: Vec<(Uuid, TenantStatus)>) -> Self {
        Self {
            known,
            batches: Mutex::new(Vec::new()),
            filters: Mutex::new(Vec::new()),
            fail: false,
        }
    }

    fn failing() -> Self {
        Self {
            known: Vec::new(),
            batches: Mutex::new(Vec::new()),
            filters: Mutex::new(Vec::new()),
            fail: true,
        }
    }

    fn batches(&self) -> Vec<usize> {
        self.batches.lock().expect("batches lock").clone()
    }
}

#[async_trait]
impl TenantResolverClient for FakeResolver {
    async fn get_tenant(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        unimplemented!("not exercised by the lifecycle reader")
    }

    async fn get_root_tenant(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<TenantInfo, TenantResolverError> {
        unimplemented!("not exercised by the lifecycle reader")
    }

    async fn get_tenants(
        &self,
        _ctx: &SecurityContext,
        ids: &[TenantId],
        options: &GetTenantsOptions,
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        if self.fail {
            return Err(TenantResolverError::NoPluginAvailable);
        }
        self.batches.lock().expect("batches lock").push(ids.len());
        self.filters
            .lock()
            .expect("filters lock")
            .push(options.status.clone());
        let asked: HashSet<Uuid> = ids.iter().map(|id| id.0).collect();
        Ok(self
            .known
            .iter()
            .filter(|(id, status)| {
                asked.contains(id) && (options.status.is_empty() || options.status.contains(status))
            })
            .map(|(id, status)| TenantInfo {
                id: TenantId(*id),
                name: format!("tenant-{id}"),
                status: *status,
                tenant_type: None,
                parent_id: None,
                self_managed: false,
            })
            .collect())
    }

    async fn get_ancestors(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
        _options: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        unimplemented!("not exercised by the lifecycle reader")
    }

    async fn get_descendants(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
        _options: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        unimplemented!("not exercised by the lifecycle reader")
    }

    async fn is_ancestor(
        &self,
        _ctx: &SecurityContext,
        _ancestor_id: TenantId,
        _descendant_id: TenantId,
        _options: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        unimplemented!("not exercised by the lifecycle reader")
    }
}

fn tenant(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn reader(fake: &Arc<FakeResolver>) -> ResolverTenantLifecycleReader {
    ResolverTenantLifecycleReader::new(Arc::clone(fake) as Arc<dyn TenantResolverClient>)
}

#[tokio::test]
async fn classifies_live_and_deleted_and_omits_unknown() {
    let active = tenant(1);
    let suspended = tenant(2);
    let deleted = tenant(3);
    // Never registered — the hard-deleted / never-existed case (28% of the
    // stage1 rows), which the registry answers by simply omitting the id.
    let absent = tenant(4);

    let fake = Arc::new(FakeResolver::new(vec![
        (active, TenantStatus::Active),
        (suspended, TenantStatus::Suspended),
        (deleted, TenantStatus::Deleted),
    ]));

    let known = reader(&fake)
        .lifecycles(&[active, suspended, deleted, absent])
        .await
        .expect("registry read");

    assert_eq!(
        known,
        HashMap::from([
            (active, RegistryLifecycle::Live),
            (suspended, RegistryLifecycle::Live),
            (deleted, RegistryLifecycle::Deleted),
        ]),
        "a suspended tenant still owns live money; a deleted one is reported as such; \
         an unknown one is simply absent"
    );
}

#[tokio::test]
async fn queries_every_status_so_deletion_is_positive_evidence() {
    // Filtering on Active/Suspended would make a Deleted tenant
    // indistinguishable from one the plugin just does not know — and the
    // purge must only ever act on the former.
    let fake = Arc::new(FakeResolver::new(vec![(tenant(1), TenantStatus::Deleted)]));

    reader(&fake)
        .lifecycles(&[tenant(1)])
        .await
        .expect("registry read");

    assert_eq!(
        fake.filters.lock().expect("filters lock").clone(),
        vec![Vec::<TenantStatus>::new()],
        "the registry is asked for every status"
    );
}

#[tokio::test]
async fn splits_large_candidate_sets_into_bounded_batches() {
    // One and a half chunks: the registry must be asked twice, and the union
    // of both answers returned. Unchunked, the id list would grow one bind
    // parameter per tenant straight into Postgres' 65,535-parameter ceiling.
    let candidates: Vec<Uuid> = (0..LOOKUP_CHUNK + LOOKUP_CHUNK / 2)
        .map(|n| tenant(n as u128 + 1))
        .collect();
    let known = candidates
        .iter()
        .map(|id| (*id, TenantStatus::Active))
        .collect();

    let fake = Arc::new(FakeResolver::new(known));

    let live = reader(&fake)
        .lifecycles(&candidates)
        .await
        .expect("registry read");

    assert_eq!(live.len(), candidates.len());
    assert_eq!(fake.batches(), vec![LOOKUP_CHUNK, LOOKUP_CHUNK / 2]);
}

#[tokio::test]
async fn empty_candidate_set_never_touches_the_registry() {
    let fake = Arc::new(FakeResolver::new(Vec::new()));

    assert!(
        reader(&fake)
            .lifecycles(&[])
            .await
            .expect("no-op")
            .is_empty()
    );
    assert!(fake.batches().is_empty(), "no round trip for an empty set");
}

#[tokio::test]
async fn registry_failure_surfaces_as_an_error_not_an_empty_set() {
    // Critical: an empty `Ok` map means "the registry knows none of them". A
    // transient registry fault must NOT be able to mint that answer.
    let fake = Arc::new(FakeResolver::failing());

    assert!(reader(&fake).lifecycles(&[tenant(1)]).await.is_err());
}

#[test]
fn suspended_is_live_deleted_is_deleted() {
    assert_eq!(
        RegistryLifecycle::from(TenantStatus::Active),
        RegistryLifecycle::Live
    );
    assert_eq!(
        RegistryLifecycle::from(TenantStatus::Suspended),
        RegistryLifecycle::Live
    );
    assert_eq!(
        RegistryLifecycle::from(TenantStatus::Deleted),
        RegistryLifecycle::Deleted
    );
}

// --- plan_lifecycle -------------------------------------------------------

#[test]
fn plan_reconciles_live_and_separates_deleted_from_unregistered() {
    let (live, deleted, absent) = (tenant(1), tenant(2), tenant(3));
    let answer = Ok(HashMap::from([
        (live, RegistryLifecycle::Live),
        (deleted, RegistryLifecycle::Deleted),
    ]));

    let plan = plan_lifecycle(&[live, deleted, absent], answer, UnregisteredTenants::Skip)
        .expect("trusted answer");

    assert_eq!(
        plan,
        LifecyclePlan {
            reconcile: vec![live],
            deleted: vec![deleted],
            unregistered: vec![absent],
        }
    );
}

#[test]
fn plan_reconciles_unregistered_when_skipping_is_off() {
    // A non-authoritative plugin (static-tr-plugin) omits every runtime-created
    // tenant; with the knob off those keep being reconciled. Deleted ones are
    // still skipped — that answer is positive.
    let (live, deleted, absent) = (tenant(1), tenant(2), tenant(3));
    let answer = Ok(HashMap::from([
        (live, RegistryLifecycle::Live),
        (deleted, RegistryLifecycle::Deleted),
    ]));

    let plan = plan_lifecycle(
        &[live, deleted, absent],
        answer,
        UnregisteredTenants::Reconcile,
    )
    .expect("trusted answer");

    assert_eq!(plan.reconcile, vec![live, absent]);
    assert_eq!(plan.deleted, vec![deleted]);
    assert!(plan.unregistered.is_empty());
}

#[test]
fn plan_treats_a_read_failure_as_unavailable() {
    let err = plan_lifecycle(
        &[tenant(1)],
        Err::<HashMap<Uuid, RegistryLifecycle>, _>(anyhow::anyhow!("no plugin")),
        UnregisteredTenants::Skip,
    )
    .expect_err("a failed read is never a plan");

    assert_eq!(err.reason(), "read_failed");
    assert!(err.to_string().contains("no plugin"));
}

#[test]
fn plan_treats_recognising_nobody_as_unavailable() {
    // What single-tenant-tr-plugin answers for an anonymous context, and what
    // a misbound plugin answers generally. Read literally it would skip the
    // whole fleet.
    let err = plan_lifecycle(
        &[tenant(1), tenant(2)],
        Ok(HashMap::<Uuid, RegistryLifecycle>::new()),
        UnregisteredTenants::Skip,
    )
    .expect_err("recognising nobody is not a plausible fleet");

    assert_eq!(err.reason(), "none_recognised");
    assert!(err.to_string().contains("none of 2 candidates"));
}

#[test]
fn plan_over_no_candidates_is_empty_not_unavailable() {
    let plan = plan_lifecycle(
        &[],
        Ok(HashMap::<Uuid, RegistryLifecycle>::new()),
        UnregisteredTenants::Skip,
    )
    .expect("nothing to classify");

    assert_eq!(plan, LifecyclePlan::default());
}

#[test]
fn reconcile_all_is_the_fail_safe_plan() {
    let candidates = [tenant(1), tenant(2)];

    let plan = LifecyclePlan::reconcile_all(&candidates);

    assert_eq!(plan.reconcile, candidates.to_vec());
    assert!(plan.deleted.is_empty(), "the fail-safe plan purges nothing");
    assert!(plan.unregistered.is_empty());
}
