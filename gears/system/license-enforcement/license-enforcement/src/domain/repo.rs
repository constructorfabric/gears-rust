//! The port of the gear store. `infra::storage` implements it.

use std::collections::HashMap;

use async_trait::async_trait;
use gts::GtsTypeId;
use license_enforcement_sdk::models::{Grant, Pack};
use license_enforcement_sdk::{
    BindingFilter, EvidenceId, GrantId, Intake, MaterialWrite, PackId, StoredEvidence,
};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
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

/// Queries of the gear store.
#[async_trait]
pub trait GrantRepo: Send + Sync + Copy + 'static {
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

    /// Evidence by id. Without `with_bytes`, its `bytes` are empty and not read.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn evidence<C: DBRunner>(
        &self,
        runner: &C,
        id: EvidenceId,
        with_bytes: bool,
    ) -> Result<StoredEvidence, DomainError>;

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

    /// The payload hash and the stored response for the token, inside the window.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn idempotent_result<C: DBRunner>(
        &self,
        runner: &C,
        tenant: Uuid,
        token: &str,
        now: OffsetDateTime,
    ) -> Result<Option<(String, serde_json::Value)>, DomainError>;

    /// Store the result for the token, replacing an expired one.
    ///
    /// # Errors
    ///
    /// Store faults.
    #[allow(clippy::too_many_arguments)]
    async fn record_result<C: DBRunner>(
        &self,
        runner: &C,
        tenant: Uuid,
        token: &str,
        payload_hash: String,
        response: serde_json::Value,
        expires_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<(), DomainError>;

    /// Evidence with this replay key exists.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn replay_key_used<C: DBRunner>(
        &self,
        runner: &C,
        plugin_id: &str,
        replay_key: &str,
    ) -> Result<bool, DomainError>;

    /// The evidence of the plugin with this external id, and whether other
    /// evidence already replaced it.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn evidence_by_external_id<C: DBRunner>(
        &self,
        runner: &C,
        plugin_id: &str,
        external_id: &str,
    ) -> Result<Option<(EvidenceId, bool)>, DomainError>;

    /// Insert evidence. A unique violation on the replay key, the external id,
    /// or the replaced evidence is `AdmissionConflict`: a concurrent admission
    /// took it, and the retry reports why.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn insert_evidence<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        row: NewEvidenceRow,
        now: OffsetDateTime,
    ) -> Result<(), DomainError>;

    /// Insert a pack.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn insert_pack<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: PackId,
        holder: Uuid,
        now: OffsetDateTime,
    ) -> Result<(), DomainError>;

    /// Insert a root grant.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn insert_root<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        g: &Grant,
        now: OffsetDateTime,
    ) -> Result<(), DomainError>;

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

    /// Write the binding and version of `g` if the stored version is still
    /// `g.version - 1`; otherwise `VersionMismatch`.
    ///
    /// # Errors
    ///
    /// Store faults.
    async fn update_binding<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        g: &Grant,
        now: OffsetDateTime,
    ) -> Result<(), DomainError>;

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

    /// A pack in `scope`, with its binding counts.
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
