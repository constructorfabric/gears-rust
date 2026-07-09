//! Unit tests for the platform-bootstrap saga.
//!
//! All tests run against in-memory fakes -- `FakeTenantRepo` for the
//! repository and `FakeIdpProvisioner` for the `IdP` plugin trait -- and
//! exercise the saga's classify / IdP-wait / finalize / compensate
//! branches without touching the DB or the network. Tests that need a
//! `TypesRegistryClient` use the inline [`StubTypesRegistry`] below
//! (returns one canned `GtsTypeSchema` from `get_type_schema` and
//! `unreachable!()` for every other method on the 13-method trait;
//! bootstrap only consults `get_type_schema` during preflight).
//!
//! `tokio::time::pause()` is used wherever the saga's wait/backoff
//! envelope would otherwise stall the test on real wall-clock sleeps.
//! All durations in [`bootstrap_cfg`] are pinned to 1 second so the
//! deadline arithmetic remains trivial to reason about.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    reason = "test helpers"
)]

use super::*;
use crate::domain::tenant::model::{TenantModel, TenantStatus};
use crate::domain::tenant::test_support::{
    FakeDeprovisionOutcome, FakeIdpProvisioner, FakeOutcome, FakeTenantRepo,
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::{
    GtsInstance, GtsTypeId, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery,
    TypesRegistryClient,
};
use uuid::Uuid;

const ROOT_ID_RAW: u128 = 0x100;
const TENANT_TYPE_UUID_RAW: u128 = 0xAA;
const ROOT_TENANT_TYPE: &str = "gts.cf.core.am.tenant_type.v1~cf.core.am.platform.v1~";

fn root_id() -> Uuid {
    Uuid::from_u128(ROOT_ID_RAW)
}

fn epoch_ts() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("stable epoch")
}

/// Pin every duration knob at 1 second so deadline arithmetic is
/// trivially explainable. Bootstrap defaults are 2-30s scale; the
/// saga retry-loop math is the same regardless of the absolute units.
fn bootstrap_cfg() -> BootstrapConfig {
    BootstrapConfig {
        root_id: root_id(),
        root_name: "platform-root".into(),
        root_tenant_type: gts::GtsTypeId::new(ROOT_TENANT_TYPE),
        root_tenant_metadata: None,
        idp_wait_timeout: std::time::Duration::from_secs(1),
        idp_retry_backoff_initial: std::time::Duration::from_secs(1),
        idp_retry_backoff_max: std::time::Duration::from_secs(1),
        strict: false,
    }
}

fn seed_root(repo: &FakeTenantRepo, status: TenantStatus) {
    let now = epoch_ts();
    repo.insert_tenant_raw(TenantModel {
        id: root_id(),
        parent_id: None,
        name: "platform-root".into(),
        status,
        self_managed: false,
        tenant_type_uuid: Uuid::from_u128(TENANT_TYPE_UUID_RAW),
        depth: 0,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    });
}

fn make_bootstrap(
    repo: Arc<FakeTenantRepo>,
    outcome: FakeOutcome,
) -> (Arc<FakeIdpProvisioner>, BootstrapService<FakeTenantRepo>) {
    let idp = Arc::new(FakeIdpProvisioner::new(outcome));
    let svc = BootstrapService::new(
        repo,
        idp.clone() as Arc<dyn IdpPluginClient>,
        bootstrap_cfg(),
    );
    (idp, svc)
}

/// Minimal `TypesRegistryClient` whose `get_type_schema` returns a
/// canned AM-prefixed schema with empty `allowed_parent_types`. Every
/// other method on the 13-method trait is `unreachable!()` because
/// bootstrap only consults `get_type_schema` during preflight (the
/// `NoRoot` saga path).
struct StubTypesRegistry;

impl StubTypesRegistry {
    fn arc() -> Arc<dyn TypesRegistryClient> {
        Arc::new(Self)
    }

    fn canned_schema() -> GtsTypeSchema {
        // Return a ROOT-level AM `tenant_type` schema (no parent chain
        // segment) so `GtsTypeSchema::try_new` does not require a
        // pre-resolved parent. Bootstrap preflight only inspects the
        // chain prefix on `type_id` (must start with
        // `gts.cf.core.am.tenant_type.v1~`) and the optional
        // `x-gts-traits.allowed_parent_types` array (left unset so
        // the eligibility check passes).
        GtsTypeSchema::try_new(
            GtsTypeId::new("gts.cf.core.am.tenant_type.v1~"),
            serde_json::json!({}),
            None,
            None,
        )
        .expect("canned root schema must construct")
    }

    /// Canned `gts.cf.core.am.tenant.v1~` projection schema. Pins the
    /// `name` field bounds (`minLength: 1, maxLength: 255`) so
    /// [`crate::domain::gts_validation::validate_tenant_name_via_gts`]
    /// — called from `insert_root_provisioning` — has a registered
    /// schema to validate `cfg.root_name` against. Without this the
    /// helper would short-circuit on `CanonicalError::NotFound` and the
    /// bounds would not gate the saga in tests.
    fn canned_tenant_schema() -> GtsTypeSchema {
        GtsTypeSchema::try_new(
            GtsTypeId::new("gts.cf.core.am.tenant.v1~"),
            serde_json::json!({
                "type": "object",
                "required": ["id", "name"],
                "properties": {
                    "id": { "type": "string", "format": "uuid" },
                    "name": { "type": "string", "minLength": 1, "maxLength": 255 },
                    "parent_id": { "type": ["string", "null"], "format": "uuid" },
                },
            }),
            None,
            None,
        )
        .expect("canned tenant schema must construct")
    }
}

#[async_trait]
impl TypesRegistryClient for StubTypesRegistry {
    async fn register(
        &self,
        _entities: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        unreachable!("not exercised by bootstrap")
    }
    async fn register_type_schemas(
        &self,
        _type_schemas: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        unreachable!("not exercised by bootstrap")
    }
    async fn get_type_schema(&self, type_id: &str) -> Result<GtsTypeSchema, CanonicalError> {
        // Dispatch by the two ids bootstrap consults:
        //   * `ROOT_TENANT_TYPE` — preflight tenant-type eligibility
        //     (`preflight_root_tenant_type`).
        //   * `gts.cf.core.am.tenant.v1~` — `root_name` structural
        //     validation in `insert_root_provisioning` mirroring the
        //     `create_tenant` site. Any other id is a wiring regression
        //     and trips a loud panic, same posture as the previous
        //     single-id assertion.
        match type_id {
            ROOT_TENANT_TYPE => Ok(Self::canned_schema()),
            "gts.cf.core.am.tenant.v1~" => Ok(Self::canned_tenant_schema()),
            other => panic!(
                "bootstrap queried unexpected type_id `{other}` (expected `{ROOT_TENANT_TYPE}` or `gts.cf.core.am.tenant.v1~`)"
            ),
        }
    }
    async fn get_type_schema_by_uuid(
        &self,
        _type_uuid: Uuid,
    ) -> Result<GtsTypeSchema, CanonicalError> {
        unreachable!("not exercised by bootstrap")
    }
    async fn get_type_schemas(
        &self,
        _type_ids: Vec<String>,
    ) -> HashMap<String, Result<GtsTypeSchema, CanonicalError>> {
        unreachable!("not exercised by bootstrap")
    }
    async fn get_type_schemas_by_uuid(
        &self,
        _type_uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsTypeSchema, CanonicalError>> {
        unreachable!("not exercised by bootstrap")
    }
    async fn list_type_schemas(
        &self,
        _query: TypeSchemaQuery,
    ) -> Result<Vec<GtsTypeSchema>, CanonicalError> {
        unreachable!("not exercised by bootstrap")
    }
    async fn register_instances(
        &self,
        _instances: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        unreachable!("not exercised by bootstrap")
    }
    async fn get_instance(&self, _id: &str) -> Result<GtsInstance, CanonicalError> {
        unreachable!("not exercised by bootstrap")
    }
    async fn get_instance_by_uuid(&self, _uuid: Uuid) -> Result<GtsInstance, CanonicalError> {
        unreachable!("not exercised by bootstrap")
    }
    async fn get_instances(
        &self,
        _ids: Vec<String>,
    ) -> HashMap<String, Result<GtsInstance, CanonicalError>> {
        unreachable!("not exercised by bootstrap")
    }
    async fn get_instances_by_uuid(
        &self,
        _uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsInstance, CanonicalError>> {
        unreachable!("not exercised by bootstrap")
    }
    async fn list_instances(
        &self,
        _query: InstanceQuery,
    ) -> Result<Vec<GtsInstance>, CanonicalError> {
        unreachable!("not exercised by bootstrap")
    }
}

// ---------------------------------------------------------------------
// classify(): four-arm pattern match
// ---------------------------------------------------------------------

#[tokio::test]
async fn classify_no_root_yields_no_root() {
    let repo = Arc::new(FakeTenantRepo::new());
    let (_idp, svc) = make_bootstrap(repo, FakeOutcome::Ok);
    let cls = svc
        .classify(&AccessScope::allow_all())
        .await
        .expect("classify must succeed on empty repo");
    assert!(matches!(cls, BootstrapClassification::NoRoot));
}

#[tokio::test]
async fn classify_active_root_yields_skip() {
    let repo = Arc::new(FakeTenantRepo::new());
    seed_root(&repo, TenantStatus::Active);
    let (_idp, svc) = make_bootstrap(repo, FakeOutcome::Ok);
    let cls = svc.classify(&AccessScope::allow_all()).await.unwrap();
    let model = match cls {
        BootstrapClassification::ActiveRootExists(m) => m,
        other => panic!("expected ActiveRootExists, got {other:?}"),
    };
    assert_eq!(model.id, root_id());
    assert!(matches!(model.status, TenantStatus::Active));
}

#[tokio::test]
async fn classify_provisioning_root_yields_resume() {
    let repo = Arc::new(FakeTenantRepo::new());
    seed_root(&repo, TenantStatus::Provisioning);
    let (_idp, svc) = make_bootstrap(repo, FakeOutcome::Ok);
    let cls = svc.classify(&AccessScope::allow_all()).await.unwrap();
    let model = match cls {
        BootstrapClassification::ProvisioningRootResume(m) => m,
        other => panic!("expected ProvisioningRootResume, got {other:?}"),
    };
    assert_eq!(model.id, root_id());
}

#[tokio::test]
async fn classify_suspended_root_yields_invariant_violation() {
    let repo = Arc::new(FakeTenantRepo::new());
    seed_root(&repo, TenantStatus::Suspended);
    let (_idp, svc) = make_bootstrap(repo, FakeOutcome::Ok);
    let cls = svc.classify(&AccessScope::allow_all()).await.unwrap();
    assert!(matches!(
        cls,
        BootstrapClassification::InvariantViolation {
            observed_status: TenantStatus::Suspended,
        }
    ));
}

#[tokio::test]
async fn classify_deleted_root_yields_invariant_violation() {
    let repo = Arc::new(FakeTenantRepo::new());
    seed_root(&repo, TenantStatus::Deleted);
    let (_idp, svc) = make_bootstrap(repo, FakeOutcome::Ok);
    let cls = svc.classify(&AccessScope::allow_all()).await.unwrap();
    assert!(matches!(
        cls,
        BootstrapClassification::InvariantViolation {
            observed_status: TenantStatus::Deleted,
        }
    ));
}

// ---------------------------------------------------------------------
// handle_provision_failure(): three SDK IdpProvisionFailure variants
// + the non-exhaustive wildcard arm. The Ambiguous arm is the
// behaviourally distinct one -- it MUST NOT compensate (the row stays
// in Provisioning so the reaper picks it up).
// ---------------------------------------------------------------------

#[tokio::test]
async fn handle_provision_failure_clean_compensates_and_returns_idp_unavailable() {
    let repo = Arc::new(FakeTenantRepo::new());
    seed_root(&repo, TenantStatus::Provisioning);
    let (_idp, svc) = make_bootstrap(repo.clone(), FakeOutcome::Ok);
    let scope = AccessScope::allow_all();

    let err = svc
        .handle_provision_failure(
            &scope,
            root_id(),
            IdpProvisionFailure::CleanFailure {
                detail: "fake clean".into(),
            },
        )
        .await;

    assert!(matches!(err, DomainError::IdpUnavailable { .. }));
    assert!(
        repo.find_by_id_unchecked(root_id()).is_none(),
        "CleanFailure must compensate (delete) the provisioning row"
    );
}

#[tokio::test]
#[tracing_test::traced_test]
async fn handle_provision_failure_clean_emits_warn_log_with_redacted_detail() {
    // `IdpProvisionFailure::CleanFailure` carries the provider's raw
    // `detail` which can include vendor SDK strings (hostnames, token-
    // bearing fragments, stack traces). The saga MUST surface a
    // triage breadcrumb via `tracing::warn!` on `am.idp` but the raw
    // detail MUST NOT reach the log channel — only its digest + length
    // (matching the Ambiguous and Unavailable arms).
    let repo = Arc::new(FakeTenantRepo::new());
    seed_root(&repo, TenantStatus::Provisioning);
    let (_idp, svc) = make_bootstrap(repo.clone(), FakeOutcome::Ok);
    let scope = AccessScope::allow_all();

    let _ = svc
        .handle_provision_failure(
            &scope,
            root_id(),
            IdpProvisionFailure::CleanFailure {
                detail: "fake clean detail string with secret-looking content".into(),
            },
        )
        .await;

    assert!(
        logs_contain("idp provision returned CleanFailure during bootstrap"),
        "CleanFailure arm MUST emit a warn-level log for triage"
    );
    assert!(
        logs_contain("detail_digest") && logs_contain("detail_len_chars"),
        "warn log MUST carry redacted digest + length, not raw provider detail"
    );
    assert!(
        !logs_contain("fake clean detail string with secret-looking content"),
        "raw provider detail MUST be redacted from structured logs"
    );
}

#[tokio::test]
async fn handle_provision_failure_ambiguous_keeps_row_returns_internal() {
    let repo = Arc::new(FakeTenantRepo::new());
    seed_root(&repo, TenantStatus::Provisioning);
    let (_idp, svc) = make_bootstrap(repo.clone(), FakeOutcome::Ok);
    let scope = AccessScope::allow_all();

    let err = svc
        .handle_provision_failure(
            &scope,
            root_id(),
            IdpProvisionFailure::Ambiguous {
                detail: "fake ambiguous".into(),
            },
        )
        .await;

    assert!(matches!(err, DomainError::Internal { .. }));
    assert!(
        repo.find_by_id_unchecked(root_id()).is_some(),
        "Ambiguous MUST NOT compensate -- the row stays in Provisioning so the reaper sweeps it on its next tick (FEATURE section 3 step 8.2)"
    );
}

#[tokio::test]
async fn handle_provision_failure_unsupported_compensates_returns_unsupported_op() {
    let repo = Arc::new(FakeTenantRepo::new());
    seed_root(&repo, TenantStatus::Provisioning);
    let (_idp, svc) = make_bootstrap(repo.clone(), FakeOutcome::Ok);
    let scope = AccessScope::allow_all();

    let err = svc
        .handle_provision_failure(
            &scope,
            root_id(),
            IdpProvisionFailure::UnsupportedOperation {
                detail: "fake unsupported".into(),
            },
        )
        .await;

    assert!(matches!(err, DomainError::UnsupportedOperation { .. }));
    assert!(
        repo.find_by_id_unchecked(root_id()).is_none(),
        "UnsupportedOperation must compensate the provisioning row"
    );
}

// ---------------------------------------------------------------------
// compensate(): swallow contract. A repo failure on the compensation
// path is logged and SWALLOWED so the bootstrap retry loop sees the
// original IdpUnavailable / UnsupportedOperation rather than a
// duplicate Internal stacked on top.
// ---------------------------------------------------------------------

#[tokio::test]
async fn compensate_swallows_fence_mismatch_when_a_peer_reaper_holds_the_claim() {
    // Saga compensation path uses fence `expected_claimed_by = None`
    // (the row was never reaper-claimed within this saga's window).
    // Seed a peer claim so the production fence in
    // `compensate_provisioning` rejects with `Conflict`. The saga
    // SHOULD log + swallow that error rather than propagate it.
    let repo = Arc::new(FakeTenantRepo::new());
    seed_root(&repo, TenantStatus::Provisioning);
    let peer_worker = Uuid::from_u128(0xDEAD_BEEF);
    repo.seed_claim(root_id(), peer_worker);
    let (_idp, svc) = make_bootstrap(repo.clone(), FakeOutcome::Ok);

    // We drive `compensate` indirectly through `handle_provision_failure`
    // (the `compensate` method itself is `async fn -> ()`, no return
    // value to assert against). The saga's outward error must still
    // be IdpUnavailable, with the row preserved because the
    // compensation fence rejected the delete.
    let err = svc
        .handle_provision_failure(
            &AccessScope::allow_all(),
            root_id(),
            IdpProvisionFailure::CleanFailure {
                detail: "fake clean".into(),
            },
        )
        .await;

    assert!(
        matches!(err, DomainError::IdpUnavailable { .. }),
        "outer error must remain IdpUnavailable; compensate() Conflict must be swallowed (got {err:?})"
    );
    assert!(
        repo.find_by_id_unchecked(root_id()).is_some(),
        "row stays put -- peer reaper still owns it, fence rejected the delete"
    );
}

// ---------------------------------------------------------------------
// run() happy paths -- three classifications resolve idempotently
// without touching the IdP retry loop.
// ---------------------------------------------------------------------

#[tokio::test]
async fn run_with_active_root_skips_idempotently() {
    // Idempotency contract (FEATURE §3): bootstrap on an already-
    // active root MUST NOT re-run the IdP step and MUST return the
    // existing row unchanged.
    let repo = Arc::new(FakeTenantRepo::new());
    seed_root(&repo, TenantStatus::Active);
    let (idp, svc) = make_bootstrap(repo, FakeOutcome::Ok);

    let model = svc.run().await.expect("idempotent skip must succeed");
    assert_eq!(model.id, root_id());
    assert!(matches!(model.status, TenantStatus::Active));
    assert_eq!(
        idp.provision_call_count(),
        0,
        "idempotency: skip path must NOT call provision_tenant"
    );
}

#[tokio::test]
async fn run_with_invariant_violation_root_returns_internal_without_calling_idp() {
    let repo = Arc::new(FakeTenantRepo::new());
    seed_root(&repo, TenantStatus::Suspended);
    let (idp, svc) = make_bootstrap(repo, FakeOutcome::Ok);

    let err = svc.run().await.expect_err("Suspended root must fail-fast");
    assert!(matches!(err, DomainError::Internal { .. }));
    assert_eq!(
        idp.provision_call_count(),
        0,
        "invariant violation must NOT touch the IdP plugin"
    );
}

// ---------------------------------------------------------------------
// run() retry loop -- deadline arithmetic + IdpUnavailable retry.
// ---------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn run_exhausts_deadline_and_returns_idp_unavailable() {
    // NoRoot path: classify -> preflight -> wait -> loop. Every saga
    // attempt finishes with CleanFailure -> compensate -> IdpUnavailable.
    // tokio::time::pause() makes the per-iteration backoff sleep
    // advance virtual time. With `idp_wait_timeout = 1` and
    // `idp_retry_backoff_initial = 1`, the deadline check at
    // the start of the second iteration trips immediately after the
    // first sleep completes.
    let repo = Arc::new(FakeTenantRepo::new());
    let (idp, svc) = make_bootstrap(repo, FakeOutcome::CleanFailure);
    let svc = svc.with_types_registry(StubTypesRegistry::arc());

    let err = svc
        .run()
        .await
        .expect_err("perpetual CleanFailure must exhaust the deadline");

    assert!(
        matches!(err, DomainError::IdpUnavailable { .. }),
        "expected IdpUnavailable, got {err:?}"
    );
    assert!(
        idp.provision_call_count() >= 1,
        "saga must have attempted provision_tenant at least once before timing out"
    );
}

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn run_with_provision_tenant_timeout_does_not_compensate() {
    // NoRoot path: saga inserts the Provisioning row, then awaits
    // `provision_tenant` inside `tokio::time::timeout_at(deadline, ...)`.
    // With `FakeOutcome::Hang` the provider never replies, so the
    // deadline trips and the saga falls into the `Err(_elapsed)`
    // branch in finalize() (service.rs `Err(_elapsed) =>` arm).
    //
    // The contract under test is the deliberate non-compensation:
    // `tokio::time::timeout_at` only stops local waiting; it does NOT
    // prove the IdP request never reached the vendor. Compensating
    // here would let the retry loop re-insert the Provisioning row
    // and re-call `provision_tenant`, potentially orphaning a vendor-
    // side tenant created by the original (post-deadline) request.
    // The saga MUST therefore leave the row in place for the
    // provisioning reaper to classify on its own tick.
    let repo = Arc::new(FakeTenantRepo::new());
    let repo_for_assert = Arc::clone(&repo);
    let (idp, svc) = make_bootstrap(repo, FakeOutcome::Hang);
    let svc = svc.with_types_registry(StubTypesRegistry::arc());
    let idp_for_assert = Arc::clone(&idp);
    let provision_entered = Arc::clone(&idp.provision_entered);

    let saga = tokio::spawn(async move { svc.run().await });

    // Wait until the saga deterministically enters `provision_tenant`
    // (the fake's first action is `provision_entered.notify_one()`,
    // BEFORE awaiting the never-resolving Hang future). This
    // replaces yield-spin with a positive synchronization primitive
    // — a future refactor that adds an intermediate `.await` before
    // `provision_tenant` cannot silently break this test by
    // stretching the yield envelope.
    provision_entered.notified().await;

    // Advance virtual time past the bootstrap deadline. With
    // `idp_wait_timeout = 1` the deadline trips at 1s after
    // run() entry; 5s is a comfortable cushion that also exceeds
    // the per-iteration backoff so any retry loop re-entry would
    // also re-trip the deadline rather than mask the timeout.
    tokio::time::advance(std::time::Duration::from_secs(5)).await;

    let err = saga
        .await
        .expect("saga task must complete")
        .expect_err("provision_tenant timeout MUST surface as IdpUnavailable");

    // Surface contract: IdpUnavailable with a "timed out" detail so
    // operators can correlate the bootstrap failure with the
    // dedicated `am.bootstrap` warn-log emitted on this path.
    match &err {
        DomainError::IdpUnavailable { detail } => assert!(
            detail.contains("timed out"),
            "IdpUnavailable.detail must mention 'timed out', got {detail:?}"
        ),
        other => panic!("expected IdpUnavailable, got {other:?}"),
    }

    // Compensation contract — the load-bearing invariant of the
    // timeout branch. Both checks pin the timeout-as-ambiguous
    // semantics: refactoring this arm to reuse the `CleanFailure`
    // path (which DOES compensate) would silently delete the local
    // row while a vendor-side tenant may exist, orphaning it.
    let row = repo_for_assert
        .find_by_id_unchecked(root_id())
        .expect("Provisioning row MUST remain for the reaper to pick up");
    assert!(
        matches!(row.status, TenantStatus::Provisioning),
        "row must remain in Provisioning status, got {row:?}"
    );
    assert_eq!(
        idp_for_assert.provision_call_count(),
        1,
        "provision_tenant MUST have been attempted exactly once before the deadline tripped"
    );
    assert_eq!(
        idp_for_assert.deprovision_calls.lock().expect("lock").len(),
        0,
        "deprovision_tenant MUST NOT be called on a timeout (vendor-side state is ambiguous)"
    );

    // Telemetry: the `am.bootstrap` warn-log is the single
    // operator-visible signal that the saga left a row behind for
    // the reaper. Pin it so a refactor that drops the log silently
    // removes incident-response context.
    assert!(
        logs_contain("exceeded the bootstrap deadline"),
        "expected 'exceeded the bootstrap deadline' warn-log from the timeout branch"
    );
}

#[tokio::test]
async fn run_returns_active_root_on_clean_noroot_path() {
    // Green-path E2E: classify NoRoot → preflight →
    // insert_root_provisioning → finalize → activate_tenant.
    // Pins the load-bearing invariant
    // "run() returns Ok(Active) on the clean fresh-bootstrap path".
    //
    // All other Ok-returning tests come in via skip-paths
    // (`run_with_active_root_skips_idempotently`,
    // `run_with_in_flight_provisioning_root_skips_when_peer_finalizes`)
    // which short-circuit BEFORE the saga's finalize-and-activate
    // chain. A regression in finalize / activate_tenant / closure
    // materialization would not surface there but would here.
    let repo = Arc::new(FakeTenantRepo::new());
    let repo_for_assert = Arc::clone(&repo);
    let (idp, svc) = make_bootstrap(repo, FakeOutcome::Ok);
    let svc = svc.with_types_registry(StubTypesRegistry::arc());

    let root = svc
        .run()
        .await
        .expect("clean NoRoot path MUST return Ok(active_root)");

    // Surface contract: shape of the returned model.
    assert_eq!(root.id, root_id());
    assert!(root.parent_id.is_none(), "root MUST have parent_id = None");
    assert_eq!(root.depth, 0, "root MUST sit at depth 0");
    assert!(
        matches!(root.status, TenantStatus::Active),
        "run() MUST return Active root, got {root:?}"
    );

    // Repo state contract: row in storage is also Active (saga
    // performed the Provisioning → Active flip via activate_tenant).
    let stored = repo_for_assert
        .find_by_id_unchecked(root_id())
        .expect("activated row MUST remain in repo");
    assert!(
        matches!(stored.status, TenantStatus::Active),
        "row in repo MUST be Active, got {stored:?}"
    );

    // Closure materialization contract: activation inserts the
    // (root, root) self-row.
    let closure = repo_for_assert.snapshot_closure();
    assert!(
        closure
            .iter()
            .any(|c| c.ancestor_id == root_id() && c.descendant_id == root_id()),
        "activation MUST insert the root self-closure-row, got {closure:?}"
    );

    // IdP contract: provision_tenant called exactly once on the
    // green path.
    assert_eq!(
        idp.provision_call_count(),
        1,
        "green-path saga MUST call provision_tenant exactly once"
    );
}

#[tokio::test]
async fn run_rejects_root_name_violating_tenant_v1_schema_via_gts() {
    // Pin the validate-before-insert fence in
    // `insert_root_provisioning`: a configured `root_name` that
    // violates the published `gts.cf.core.am.tenant.v1~` schema
    // (>255 chars per the canned stub schema, mirroring the live
    // bounds) MUST fail saga step 1 with `Validation` BEFORE any
    // `tenants` row is written. Mirror site for
    // `TenantService::create_tenant`'s GTS-name gate — without this
    // fence the only guard for `root_name` would be the DB CHECK
    // constraint, which leaks the bounds duplication the GTS-runtime
    // validation pattern was introduced to eliminate.
    let repo = Arc::new(FakeTenantRepo::new());
    let repo_for_assert = Arc::clone(&repo);
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    let mut cfg = bootstrap_cfg();
    cfg.root_name = "x".repeat(256);
    let svc = BootstrapService::new(repo, idp.clone() as Arc<dyn IdpPluginClient>, cfg)
        .with_types_registry(StubTypesRegistry::arc());

    let err = svc
        .run()
        .await
        .expect_err("oversized root_name MUST fail GTS validation in insert step");

    match &err {
        DomainError::Validation { detail } => {
            assert!(
                detail.contains("name") && detail.contains("gts.cf.core.am.tenant.v1~"),
                "Validation must name the offending field and schema, got: {detail}"
            );
        }
        other => panic!("expected DomainError::Validation, got {other:?}"),
    }

    // Repo state contract: validation runs BEFORE `insert_provisioning`,
    // so no `tenants` row materializes on the failure path.
    assert!(
        repo_for_assert.find_by_id_unchecked(root_id()).is_none(),
        "GTS validation MUST gate the insert; no row may land in repo"
    );

    // IdP contract: `provision_tenant` is a saga-step-2 call that
    // only runs after a successful step-1 insert. A validation
    // failure at step 1 must not advance to step 2.
    assert_eq!(
        idp.provision_call_count(),
        0,
        "name-validation failure at step 1 MUST NOT advance to provision_tenant"
    );
}

// ---------------------------------------------------------------------
// run() ProvisioningRootResume — stuck (>2x timeout) is fail-fast,
// in-flight (<=2x timeout) waits for the deadline.
// ---------------------------------------------------------------------

/// Seed a `Provisioning` root with `created_at = now - age_secs`
/// against the **real** wall clock. The age check inside `run()`
/// uses `OffsetDateTime::now_utc()`, so the row's `created_at` MUST
/// be relative to wall-clock-now (not the pinned `epoch_ts` used by
/// other fixtures) for the stuck-vs-in-flight branch to behave
/// deterministically.
fn seed_root_with_age(repo: &FakeTenantRepo, age_secs: i64) {
    let now = OffsetDateTime::now_utc();
    let created_at = now - time::Duration::seconds(age_secs);
    repo.insert_tenant_raw(TenantModel {
        id: root_id(),
        parent_id: None,
        name: "platform-root".into(),
        status: TenantStatus::Provisioning,
        self_managed: false,
        tenant_type_uuid: Uuid::from_u128(TENANT_TYPE_UUID_RAW),
        depth: 0,
        created_at,
        updated_at: now,
        deleted_at: None,
    });
}

#[tokio::test]
async fn bootstrap_compensates_stuck_provisioning_row_synchronously_when_idp_confirms_teardown() {
    // `idp_wait_timeout = 1` (cfg), so `stuck_threshold = 2s`.
    // Age = 10s > 2s → stuck branch fires. With the IdP confirming
    // deprovision (`Ok(())`) and a fresh `provision_tenant` returning
    // `Ok`, the saga compensates the stuck row in-band and restarts
    // through the standard NoRoot path to activate a new root.
    let repo = Arc::new(FakeTenantRepo::new());
    seed_root_with_age(&repo, 10);
    let repo_for_assert = Arc::clone(&repo);
    let (idp, svc) = make_bootstrap(repo, FakeOutcome::Ok);
    let svc = svc.with_types_registry(StubTypesRegistry::arc());

    let model = svc
        .run()
        .await
        .expect("in-band compensation MUST recover and activate a fresh root");

    assert_eq!(model.id, root_id());
    assert!(
        matches!(model.status, TenantStatus::Active),
        "expected Active root, got {model:?}"
    );
    assert_eq!(
        idp.deprovision_calls.lock().expect("lock").len(),
        1,
        "stuck-row compensation MUST invoke deprovision_tenant exactly once"
    );
    assert_eq!(
        idp.provision_call_count(),
        1,
        "after in-band reap, the saga MUST issue exactly one fresh provision_tenant call"
    );
    let row = repo_for_assert
        .find_by_id_unchecked(root_id())
        .expect("a fresh root row must exist after recovery");
    assert!(
        matches!(row.status, TenantStatus::Active),
        "recovered row must be Active, got {row:?}"
    );
}

#[tokio::test]
async fn bootstrap_falls_through_to_deferred_to_reaper_when_inband_compensation_fails() {
    // `idp_wait_timeout = 1` (cfg), so `stuck_threshold = 2s`.
    // Age = 10s > 2s → stuck branch fires. Deprovision returns
    // `Retryable`, so in-band compensation cannot confirm cleanup
    // and the saga surfaces the existing `deferred_to_reaper`
    // terminal without dropping the local Provisioning row.
    let repo = Arc::new(FakeTenantRepo::new());
    seed_root_with_age(&repo, 10);
    let repo_for_assert = Arc::clone(&repo);
    let (idp, svc) = make_bootstrap(repo, FakeOutcome::Ok);
    idp.set_deprovision_outcome(FakeDeprovisionOutcome::Retryable);
    let svc = svc.with_types_registry(StubTypesRegistry::arc());

    let err = svc
        .run()
        .await
        .expect_err("non-clean in-band compensation MUST surface as Internal (deferred to reaper)");

    match &err {
        DomainError::Internal { diagnostic, .. } => {
            assert!(
                diagnostic.contains("deferred to reaper"),
                "error must mention reaper deferral, got: {diagnostic}"
            );
        }
        other => panic!("expected Internal (deferred-to-reaper), got {other:?}"),
    }
    assert_eq!(
        idp.deprovision_calls.lock().expect("lock").len(),
        1,
        "stuck-row compensation MUST attempt deprovision_tenant exactly once before deferring"
    );
    assert_eq!(
        idp.provision_call_count(),
        0,
        "compensation failure MUST NOT advance to provision_tenant; the row is left for the reaper"
    );
    let row = repo_for_assert
        .find_by_id_unchecked(root_id())
        .expect("stuck row MUST remain in place when in-band compensation fails");
    assert!(
        matches!(row.status, TenantStatus::Provisioning),
        "stuck row must remain in Provisioning when compensation failed, got {row:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn run_with_in_flight_provisioning_root_waits_until_deadline() {
    // `idp_wait_timeout = 1` → stuck_threshold = 2s. Age = 0
    // (just inserted) → in-flight branch. Peer never finalizes, so
    // we hit the deadline and surface IdpUnavailable.
    let repo = Arc::new(FakeTenantRepo::new());
    seed_root_with_age(&repo, 0);
    let (idp, svc) = make_bootstrap(repo, FakeOutcome::Ok);
    let svc = svc.with_types_registry(StubTypesRegistry::arc());

    let err = svc
        .run()
        .await
        .expect_err("in-flight peer that never finalizes MUST exhaust the deadline");

    assert!(
        matches!(err, DomainError::IdpUnavailable { .. }),
        "expected IdpUnavailable, got {err:?}"
    );
    assert_eq!(
        idp.provision_call_count(),
        0,
        "in-flight branch waits for the peer; this replica MUST NOT contact the IdP"
    );
}

#[tokio::test(start_paused = true)]
async fn run_with_in_flight_provisioning_root_skips_when_peer_finalizes() {
    // Companion to `run_with_in_flight_provisioning_root_waits_until_deadline`:
    // same in-flight starting shape, but the peer's saga flips the row
    // to Active mid-wait. The classify on the next loop iteration must
    // observe `ActiveRootExists` and return the existing row without
    // ever calling the IdP from this replica.
    let repo = Arc::new(FakeTenantRepo::new());
    seed_root_with_age(&repo, 0); // Provisioning, age=0 → in-flight branch
    let repo_for_saga = Arc::clone(&repo);
    let (idp, svc) = make_bootstrap(repo_for_saga, FakeOutcome::Ok);
    let svc = svc.with_types_registry(StubTypesRegistry::arc());
    let idp_for_assert = Arc::clone(&idp);

    let saga = tokio::spawn(async move { svc.run().await });

    // Yield once so the saga reaches its first per-iteration sleep arm.
    tokio::task::yield_now().await;

    // Peer's saga finalised the root: flip the seeded row to Active.
    // `insert_tenant_raw` inserts/overwrites by id, so this acts as an
    // in-place state mutation visible to the next `classify`.
    let now = OffsetDateTime::now_utc();
    repo.insert_tenant_raw(TenantModel {
        id: root_id(),
        parent_id: None,
        name: "platform-root".into(),
        status: TenantStatus::Active,
        self_managed: false,
        tenant_type_uuid: Uuid::from_u128(TENANT_TYPE_UUID_RAW),
        depth: 0,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    });

    // Advance virtual time past the in-flight branch's per-iteration
    // sleep (1s @ `idp_retry_backoff_initial`) so the next
    // `classify` runs and observes the now-Active row.
    //
    // Note: 2s also breaches the 1s `idp_wait_timeout` deadline
    // captured at `run()` entry. This test passes only because the
    // `ActiveRootExists` arm short-circuits with `Ok` BEFORE the
    // deadline check on the `ProvisioningRootResume` arm fires (see
    // `service.rs::run` loop ordering: the match arms are evaluated
    // in declaration order, ActiveRootExists first). A future
    // refactor that reorders arms or moves the deadline check above
    // ActiveRootExists would silently flip this assertion from
    // `Ok(Active)` to `Err(IdpUnavailable)` — re-tune the sleep
    // (e.g. 800ms via `tokio::time::advance`) if that ordering
    // contract is ever loosened.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let model = saga
        .await
        .expect("saga task must complete")
        .expect("peer-finalised in-flight branch MUST surface as Ok(Active)");

    assert_eq!(model.id, root_id());
    assert!(
        matches!(model.status, TenantStatus::Active),
        "peer's finalisation must be observed as Active, got {model:?}"
    );
    assert_eq!(
        idp_for_assert.provision_call_count(),
        0,
        "peer-finalised branch must NOT contact the IdP from this replica"
    );
}

#[tokio::test(start_paused = true)]
async fn run_takes_over_when_peer_compensates_mid_resume_wait() {
    // ProvisioningRootResume → NoRoot transition: the row we saw in
    // Provisioning at initial classify is gone by the time we
    // re-classify in the loop (peer's saga compensated, or the
    // reaper picked it up). The `pending_takeover_precheck` flag
    // forces the takeover bridge — preflight — before we proceed
    // to insert. After the bridge runs, the saga drives the green
    // path (Insert → Finalize → activate) to completion.
    //
    // Pins three load-bearing invariants of the Resume → takeover
    // arc that no other test exercises end-to-end:
    //   1. The transition exists: NoRoot in the loop after a Resume
    //      observation routes to TakeoverPreflight, not directly
    //      to Insert.
    //   2. provision_tenant is called exactly once — the wait path
    //      itself does NOT contact the IdP from this replica.
    //   3. The end state is Ok(Active); the takeover bridge does
    //      not regress to a deadline-trip terminal arm.
    //
    // Uses `bootstrap_cfg_long_deadline` (30s budget) so the
    // takeover bridge has room to complete after waking from the
    // 1s Sleep(PeerInProgress) — the standard 1s-deadline cfg
    // would trip the deadline check on wake-up.
    let repo = Arc::new(FakeTenantRepo::new());
    seed_root_with_age(&repo, 0); // Provisioning, age=0 → Resume in-flight
    let repo_for_saga = Arc::clone(&repo);
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    let svc = BootstrapService::new(
        repo_for_saga,
        idp.clone() as Arc<dyn IdpPluginClient>,
        bootstrap_cfg_long_deadline(),
    )
    .with_types_registry(StubTypesRegistry::arc());
    let idp_for_assert = Arc::clone(&idp);

    let saga = tokio::spawn(async move { svc.run().await });

    // Yield once so the saga reaches its first per-iteration sleep
    // arm: initial classify saw Resume → set takeover flag → loop
    // classify saw Resume again → emit retry metric →
    // Sleep(PeerInProgress).
    tokio::task::yield_now().await;

    // Peer compensated mid-wait: delete the Provisioning row
    // through the production-shaped trait method. Passing
    // `expected_claimed_by = None` matches the saga-side
    // compensation contract (no reaper claim held). The next loop
    // classify will observe NoRoot, take the takeover branch, and
    // proceed.
    repo.compensate_provisioning(&AccessScope::allow_all(), root_id(), None)
        .await
        .expect("peer compensation MUST succeed in test");

    // Advance virtual time past the 1s per-iteration backoff so
    // the saga wakes from Sleep(PeerInProgress) and re-classifies.
    // After takeover the chain runs synchronously
    // (TakeoverPreflight → Insert → Finalize → Terminal),
    // so 1500ms is enough to wake the sleep AND stay well under
    // the 30s `bootstrap_cfg_long_deadline()` budget. `advance` is
    // preferred over `sleep` here because it makes the intent
    // ("push virtual time") explicit instead of relying on
    // sleep's auto-advance side effect.
    tokio::time::advance(std::time::Duration::from_millis(1500)).await;

    let model = saga
        .await
        .expect("saga task must complete")
        .expect("takeover-from-Resume MUST surface as Ok(Active)");

    assert_eq!(model.id, root_id());
    assert!(model.parent_id.is_none(), "root MUST have parent_id = None");
    assert!(
        matches!(model.status, TenantStatus::Active),
        "takeover saga MUST return Active root, got {model:?}"
    );
    assert_eq!(
        idp_for_assert.provision_call_count(),
        1,
        "takeover saga MUST call provision_tenant exactly once \
         (the wait path does NOT contact the IdP from this replica)"
    );
}

/// Like [`bootstrap_cfg`] but with a long deadline + 1s backoff cap so
/// the AlreadyExists-streak path can run its three iterations before
/// the deadline trips. With the 1s cap, each retry sleeps 1s; three
/// retries fit comfortably inside the 30s timeout.
fn bootstrap_cfg_long_deadline() -> BootstrapConfig {
    BootstrapConfig {
        root_id: root_id(),
        root_name: "platform-root".into(),
        root_tenant_type: gts::GtsTypeId::new(ROOT_TENANT_TYPE),
        root_tenant_metadata: None,
        idp_wait_timeout: std::time::Duration::from_secs(30),
        idp_retry_backoff_initial: std::time::Duration::from_secs(1),
        idp_retry_backoff_max: std::time::Duration::from_secs(1),
        strict: false,
    }
}

#[tokio::test(start_paused = true)]
async fn run_aborts_after_max_already_exists_streak_when_root_id_drifts() {
    // Simulate a configured `root_id` that drifted away from the actual
    // platform root: classify-by-id returns NoRoot for the configured
    // id, but `insert_root_provisioning` collides with the existing
    // root via the `parent_id = None` single-root invariant in the
    // fake repo (which mirrors `ux_tenants_single_root` in production).
    // The pair would oscillate forever; the streak cap converts that
    // into a clean Internal after MAX_ALREADY_EXISTS_STREAK = 3 hits.
    let repo = Arc::new(FakeTenantRepo::new());
    let now = OffsetDateTime::now_utc();
    let drifted_id = Uuid::from_u128(0xDEAD);
    repo.insert_tenant_raw(TenantModel {
        id: drifted_id,
        parent_id: None,
        name: "drifted-root".into(),
        status: TenantStatus::Active,
        self_managed: false,
        tenant_type_uuid: Uuid::from_u128(TENANT_TYPE_UUID_RAW),
        depth: 0,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    });

    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    let svc = BootstrapService::new(
        Arc::clone(&repo),
        idp.clone() as Arc<dyn IdpPluginClient>,
        bootstrap_cfg_long_deadline(),
    );
    let svc = svc.with_types_registry(StubTypesRegistry::arc());

    let err = svc
        .run()
        .await
        .expect_err("drifted root_id must abort with Internal once the streak exhausts");

    match &err {
        DomainError::Internal { diagnostic, .. } => {
            assert!(
                diagnostic.contains("different id") || diagnostic.contains("config drift"),
                "Internal must explain the drift, got: {diagnostic}"
            );
        }
        other => panic!("expected Internal (root_id drift), got {other:?}"),
    }
    assert_eq!(
        idp.provision_call_count(),
        0,
        "drifted-id streak must NOT contact the IdP -- every loss occurs at the local insert"
    );
}

#[tokio::test(start_paused = true)]
async fn step3_failure_under_idp_required_keeps_provisioning_row_on_unsupported_deprovision() {
    // Pin the orphan-prevention contract from cypilot/CodeRabbit
    // round 4: when `idp.required = true` and step-3 (`activate_tenant`)
    // fails AFTER `provision_tenant` succeeded, `compensate_step3_failure`
    // calls `deprovision_tenant`. If the IdP plugin returns
    // `UnsupportedOperation`, the local Provisioning row MUST NOT be
    // deleted — vendor-side state may exist that AM cannot reach,
    // so the row has to stay for the reaper to take ownership.
    //
    // The symmetric `TenantService` invariant is pinned by
    // `hard_delete_batch_defers_unsupported_when_idp_required_true` and
    // `reaper_marks_unsupported_terminal_when_idp_required_true`.
    let repo = Arc::new(FakeTenantRepo::new());
    repo.expect_next_activation_failure("synthetic finalisation tx abort");

    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    idp.set_deprovision_outcome(FakeDeprovisionOutcome::Unsupported);

    let svc = BootstrapService::new(
        Arc::clone(&repo),
        idp.clone() as Arc<dyn IdpPluginClient>,
        bootstrap_cfg(),
    );
    let svc = svc
        .with_types_registry(StubTypesRegistry::arc())
        .with_idp_required(true);

    let err = svc
        .run()
        .await
        .expect_err("step-3 finalisation failure must propagate");
    assert!(
        matches!(err, DomainError::Internal { .. }),
        "expected Internal (synthetic activate_tenant abort), got {err:?}"
    );

    // Step-3 ran exactly once before failing.
    assert_eq!(
        idp.provision_call_count(),
        1,
        "saga must reach provision_tenant before activate_tenant fails"
    );
    // Compensator MUST have called deprovision_tenant once.
    assert_eq!(
        idp.deprovision_calls.lock().expect("lock").len(),
        1,
        "compensate_step3_failure must attempt vendor-side deprovision"
    );
    // Critical invariant: the Provisioning row is still in the repo.
    // `idp.required=true` + `UnsupportedOperation` deprovision means
    // the vendor side may still own state — deleting locally would
    // orphan it.
    let surviving = repo.find_by_id_unchecked(root_id()).expect(
        "Provisioning row must remain for the reaper after unsupported_required step-3 failure",
    );
    assert!(
        matches!(surviving.status, TenantStatus::Provisioning),
        "row must still be in Provisioning, got status={:?}",
        surviving.status,
    );
}
