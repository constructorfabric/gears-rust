//! The port of the gear store. `infra::storage` implements it.

use std::collections::HashMap;

use async_trait::async_trait;
use gts::GtsTypeId;
use license_enforcement_sdk::models::{Grant, Pack};
use license_enforcement_sdk::{
    BindingFilter, DelegationId, EvidenceId, GrantId, Intake, MaterialWrite, PackId, StoredEvidence,
};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, TxConfig};
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::error::DomainError;

/// A grant that matches a binding filter of the check.
#[domain_model]
pub struct Candidate {
    /// The grant.
    pub grant: Grant,
    /// Id of its root.
    pub root: GrantId,
}

/// Evidence as the store keeps it.
#[domain_model]
pub struct NewEvidenceRow {
    pub id: EvidenceId,
    pub holder: Uuid,
    pub plugin_id: &'static str,
    pub intake: Intake,
    pub bytes: Vec<u8>,
    pub replay_key: Option<String>,
    pub external_id: Option<String>,
    pub replaces: Option<EvidenceId>,
}

/// Who sent a mutating request: the key of its idempotency record with the
/// token. Another subject of the same tenant never replays it.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct Caller {
    pub tenant: Uuid,
    pub subject: Uuid,
}

/// Queries of the gear store.
#[async_trait]
pub trait GrantRepo: Send + Sync + Copy + 'static {
    /// A read-only transaction that reads one state of the store, in the
    /// isolation that the engine of `db` needs for it.
    fn snapshot_read(&self, db: &toolkit_db::DBProvider<DomainError>) -> TxConfig;

    /// Live candidates on the resource that match a filter: first the active
    /// grants whose window has started, then the rest; in each part by the first
    /// matching filter, then by id. At most the configured number: later
    /// candidates are not read.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn candidates<C: DBRunner>(
        &self,
        runner: &C,
        tenant: Uuid,
        resource_type: &GtsTypeId,
        resource_id: Option<&str>,
        filters: &[BindingFilter],
        instant: OffsetDateTime,
    ) -> Result<Vec<Candidate>, DomainError>;

    /// The roots of the candidates, by id.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn roots<C: DBRunner>(
        &self,
        runner: &C,
        candidates: &[Candidate],
    ) -> Result<HashMap<GrantId, Grant>, DomainError>;

    /// The tenant holds a live grant on the resource.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn any_live_on_resource<C: DBRunner>(
        &self,
        runner: &C,
        tenant: Uuid,
        resource_type: &GtsTypeId,
        resource_id: Option<&str>,
        instant: OffsetDateTime,
    ) -> Result<bool, DomainError>;

    /// The grant on the resource that matches a filter and ended last.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn last_ended<C: DBRunner>(
        &self,
        runner: &C,
        tenant: Uuid,
        resource_type: &GtsTypeId,
        resource_id: Option<&str>,
        filters: &[BindingFilter],
        instant: OffsetDateTime,
    ) -> Result<Option<Grant>, DomainError>;

    /// Evidence by id, in one query; an id without a row is absent. Without
    /// `with_bytes`, their `bytes` are empty and not read. Not scoped.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn evidence<C: DBRunner>(
        &self,
        runner: &C,
        ids: &[EvidenceId],
        with_bytes: bool,
    ) -> Result<HashMap<EvidenceId, StoredEvidence>, DomainError>;

    /// A key of the plugin material.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn material<C: DBRunner>(
        &self,
        runner: &C,
        plugin_id: &str,
        key: &str,
    ) -> Result<Option<Vec<u8>>, DomainError>;

    /// Apply writes to the plugin material. `Ok(false)` when a write found the key
    /// holding something other than what it expects; nothing after it is applied,
    /// and the caller rolls back the transaction to drop the writes before it.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn apply_material<C: DBRunner>(
        &self,
        runner: &C,
        plugin_id: &str,
        writes: &[MaterialWrite],
    ) -> Result<bool, DomainError>;

    /// The payload hash and the stored response for the token of `caller`,
    /// inside the window.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn idempotent_result<C: DBRunner>(
        &self,
        runner: &C,
        caller: Caller,
        token: &str,
        now: OffsetDateTime,
    ) -> Result<Option<(String, serde_json::Value)>, DomainError>;

    /// Store the result for the token, replacing an expired one.
    ///
    /// # Errors
    ///
    /// Store faults.
    #[expect(
        clippy::too_many_arguments,
        reason = "one idempotency record: key, payload hash, response, expiry"
    )]
    async fn record_result<C: DBRunner>(
        &self,
        runner: &C,
        caller: Caller,
        token: &str,
        payload_hash: String,
        response: serde_json::Value,
        expires_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<(), DomainError>;

    /// The evidence of `holder` with this external id.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn evidence_by_external_id<C: DBRunner>(
        &self,
        runner: &C,
        holder: Uuid,
        plugin_id: &str,
        external_id: &str,
    ) -> Result<Option<EvidenceId>, DomainError>;

    /// Insert evidence. The unique indexes of the plugin decide a reuse:
    ///
    /// # Errors
    ///
    /// - the replay key: `InvalidArgument` with `TOKEN_REUSED`;
    /// - the external id: `EvidenceRejected(Replay)`;
    /// - the replaced evidence: `EvidenceChainBroken(EVIDENCE_ALREADY_REPLACED)`;
    /// - another unique violation: `AdmissionConflict`;
    /// - store faults.
    async fn insert_evidence<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        row: NewEvidenceRow,
        now: OffsetDateTime,
    ) -> Result<(), DomainError>;

    /// Insert a pack with the counts of the members inserted with it.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn insert_pack<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        pack: &Pack,
        now: OffsetDateTime,
    ) -> Result<(), DomainError>;

    /// Insert root grants, in as few statements as the bind limit allows.
    ///
    /// # Errors
    ///
    /// `RootAlreadyAdmitted` with the first id that is already stored; store
    /// faults.
    async fn insert_roots<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        roots: &[Grant],
        now: OffsetDateTime,
    ) -> Result<(), DomainError>;

    /// The grants `ids` in `scope`, in id order, in one query; an id outside
    /// `scope` is absent. With `lock`, the rows stay locked until the end of
    /// the transaction.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn grants<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        ids: &[GrantId],
        lock: bool,
    ) -> Result<Vec<Grant>, DomainError>;

    /// A grant in `scope`; with `lock`, the row stays locked until the end of the
    /// transaction.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn grant<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: GrantId,
        lock: bool,
    ) -> Result<Option<Grant>, DomainError>;

    /// The named grantor query: the sub-grant `id` that `grantor` delegated,
    /// in any tenant; with `lock`, the row stays locked until the end of the
    /// transaction.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn grantor_grant<C: DBRunner>(
        &self,
        runner: &C,
        grantor: Uuid,
        id: GrantId,
        lock: bool,
    ) -> Result<Option<Grant>, DomainError>;

    /// Insert each `(sub-grant, root)`, in as few statements as the bind limit
    /// allows. A second live child of a parent violates I3 and is
    /// `InvalidTransition`.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn insert_sub_grants<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        subs: &[(Grant, GrantId)],
        now: OffsetDateTime,
    ) -> Result<(), DomainError>;

    /// Write the binding, validity, state, termination reason, parent,
    /// evidence, and version of each of `grants` if its stored version is
    /// still `version - 1`; otherwise `VersionMismatch`. A pack member moves
    /// between the counts of its pack, and each pack is written once.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn update_grants<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        grants: &[Grant],
        now: OffsetDateTime,
    ) -> Result<(), DomainError>;

    /// [`Self::update_grants`] of one grant.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn update_grant<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        g: &Grant,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        self.update_grants(runner, scope, std::slice::from_ref(g), now)
            .await
    }

    /// Lock the roots of the trees that hold `ids`, in id order. Every
    /// operation that changes a tree takes this lock first, so operations on one
    /// tree run one after another and each sees the rows the previous one
    /// wrote. Not scoped: the caller decides access on `ids`.
    ///
    /// # Errors
    ///
    /// `TreeMoved` when a concurrent replacement gave a tree a root below one
    /// already locked; store faults.
    async fn lock_roots<C: DBRunner>(&self, runner: &C, ids: &[GrantId])
    -> Result<(), DomainError>;

    /// For each of `ids` that is live, the grant and its live descendants,
    /// parents before children, from one read of their trees; an id that is
    /// not live is absent. Not scoped: descendants are held by other tenants,
    /// and the caller decides access on `ids`. With `lock`, the rows stay locked
    /// until the end of the transaction.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn live_subtrees<C: DBRunner>(
        &self,
        runner: &C,
        ids: &[GrantId],
        lock: bool,
    ) -> Result<HashMap<GrantId, Vec<Grant>>, DomainError>;

    /// [`Self::live_subtrees`] of one grant; empty when it is not live.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn live_subtree<C: DBRunner>(
        &self,
        runner: &C,
        id: GrantId,
        lock: bool,
    ) -> Result<Vec<Grant>, DomainError> {
        Ok(self
            .live_subtrees(runner, &[id], lock)
            .await?
            .remove(&id)
            .unwrap_or_default())
    }

    /// The sub-grants of a delegation whose grantor or holder is `party`, by
    /// id, terminated included, and the version of the delegation.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn delegation_grants<C: DBRunner>(
        &self,
        runner: &C,
        party: Uuid,
        id: DelegationId,
        lock: bool,
    ) -> Result<(Vec<Grant>, u64), DomainError>;

    /// Set the version of a delegation in every row of it. The caller holds
    /// the rows.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn set_delegation_version<C: DBRunner>(
        &self,
        runner: &C,
        id: DelegationId,
        version: u64,
    ) -> Result<(), DomainError>;

    /// Set the version of a pack if the stored one is still `version - 1`;
    /// otherwise `VersionMismatch`.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn update_pack_version<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: PackId,
        version: u64,
    ) -> Result<(), DomainError>;

    /// The members of a pack held by `holder`, by id, terminated included.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn pack_grants<C: DBRunner>(
        &self,
        runner: &C,
        holder: Uuid,
        id: PackId,
        lock: bool,
    ) -> Result<Vec<Grant>, DomainError>;

    /// For each of `ids`, the grant and its live ancestors up to the root, in
    /// this order, from one read of the live rows of their trees. Shorter when
    /// a link is terminated; empty for an id without a live row. Not scoped: ancestors are
    /// held by other tenants, and the caller decides access on `ids`.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn chains_of<C: DBRunner>(
        &self,
        runner: &C,
        ids: &[GrantId],
    ) -> Result<HashMap<GrantId, Vec<Grant>>, DomainError>;

    /// [`Self::chains_of`] of one grant.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn chain<C: DBRunner>(&self, runner: &C, id: GrantId) -> Result<Vec<Grant>, DomainError> {
        Ok(self
            .chains_of(runner, &[id])
            .await?
            .remove(&id)
            .unwrap_or_default())
    }

    /// The chain of each `(grant, its root)` pair over the live rows
    /// alone, keyed by the grant, from one read of the trees of those roots: a
    /// terminated link ends the chain short of the root. Not scoped.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn chains<C: DBRunner>(
        &self,
        runner: &C,
        starts: &[(GrantId, GrantId)],
    ) -> Result<HashMap<GrantId, Vec<Grant>>, DomainError>;

    /// The number of live rows under each of `parents`, in one query. Not
    /// scoped.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn live_children<C: DBRunner>(
        &self,
        runner: &C,
        parents: &[GrantId],
    ) -> Result<HashMap<GrantId, u64>, DomainError>;

    /// The number of live rows with each of `ids`, in one query. Not scoped.
    /// A store without its primary key can hold copies of a row (ADR-0002).
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn live_rows<C: DBRunner>(
        &self,
        runner: &C,
        ids: &[GrantId],
    ) -> Result<HashMap<GrantId, u64>, DomainError>;

    /// Set the root of each grant of `roots`, given as (grant, root), in
    /// batched statements. Not scoped: replacement moves chains whose links
    /// other tenants hold.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn set_roots<C: DBRunner>(
        &self,
        runner: &C,
        roots: &[(GrantId, GrantId)],
    ) -> Result<(), DomainError>;

    /// A page of packs in `scope`, with their binding counts. `members` scopes
    /// the grants that a filter matches.
    ///
    /// # Errors
    ///
    /// Store faults; a filter without `members` is an internal fault.
    async fn list_packs<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        members: Option<&AccessScope>,
        query: &ODataQuery,
    ) -> Result<Page<Pack>, DomainError>;

    /// Delete at most `batch` trees whose grants are all terminated and last
    /// changed before `cutoff`, then the packs left without members, and at
    /// most `batch` terminated sub-grants without children that last changed
    /// before `cutoff`, so that a branch goes leaf first. Returns the number of deleted grants.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn sweep_terminated<C: DBRunner>(
        &self,
        runner: &C,
        cutoff: OffsetDateTime,
        batch: u64,
    ) -> Result<u64, DomainError>;

    /// Delete at most `batch` idempotency records that expired before `now`.
    /// Returns the number of deleted records.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn purge_idempotency<C: DBRunner>(
        &self,
        runner: &C,
        now: OffsetDateTime,
        batch: u64,
    ) -> Result<u64, DomainError>;

    /// A page of grants in `scope`.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn list_grants<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<Grant>, DomainError>;

    /// A pack in `scope`, with its stored binding counts.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn pack<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: PackId,
    ) -> Result<Option<Pack>, DomainError>;
}
