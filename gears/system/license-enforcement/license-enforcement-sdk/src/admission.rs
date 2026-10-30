//! The admission plugin contract (DESIGN §3.3, ADR-0001, ADR-0002).
//!
//! Root grants come only from the one admission plugin that the binary links.
//! The plugin crate registers itself with [`register_admission_plugin!`](crate::register_admission_plugin); the
//! gear takes it with [`linked_admission_plugin`].

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::ids::{EvidenceId, GrantId};
use crate::link::{LinkError, exactly_one};
use crate::models::GrantTerms;
use crate::reason::DenialReason;

/// Identity of the linked plugin, reported in telemetry.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PluginInfo {
    /// Plugin id.
    pub id: &'static str,
    /// The plugin verifies its evidence.
    pub verifies_evidence: bool,
    /// Version of the trust material that the plugin uses now.
    pub trust_material_version: Option<String>,
}

impl PluginInfo {
    /// Identity of a plugin.
    #[must_use]
    pub fn new(
        id: &'static str,
        verifies_evidence: bool,
        trust_material_version: Option<String>,
    ) -> Self {
        Self {
            id,
            verifies_evidence,
            trust_material_version,
        }
    }
}

/// The intakes that the plugin serves. The gear rejects a create or a verb
/// that needs an intake that the plugin does not serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Intakes {
    /// Grant parameters from an authorized caller.
    pub issue: bool,
    /// Evidence bytes that the plugin parses.
    pub import: bool,
}

impl Intakes {
    /// The served intakes.
    #[must_use]
    pub fn new(issue: bool, import: bool) -> Self {
        Self { issue, import }
    }
}

/// The intake through which evidence came.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Intake {
    /// Grant parameters from an authorized caller.
    Issue,
    /// Evidence bytes that the plugin parses.
    Import,
}

/// Who sent an `issue` request. The gear fills it from the caller's
/// `SecurityContext`; credentials do not reach the plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerIdentity {
    /// Subject id.
    pub subject_id: Uuid,
    /// Subject type.
    pub subject_type: Option<String>,
    /// Tenant of the subject.
    pub tenant_id: Uuid,
}

/// Input to [`GrantAdmissionPluginV1::admit`]. `Debug` does not print the
/// evidence bytes.
#[derive(Clone)]
#[non_exhaustive]
pub enum AdmissionInput {
    /// The `issue` intake.
    Issue {
        /// Tenant that will hold the roots.
        holder_tenant_id: Uuid,
        /// One entry per root.
        grants: Vec<GrantTerms>,
        /// The roots form one pack.
        as_pack: bool,
        /// The caller.
        caller: CallerIdentity,
        /// Idempotency token of the request. With the caller's tenant it is the
        /// replay key of the evidence.
        idempotency_token: String,
    },
    /// The `import` intake.
    Import {
        /// Evidence bytes.
        bytes: Vec<u8>,
    },
}

impl fmt::Debug for AdmissionInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Issue {
                holder_tenant_id,
                grants,
                as_pack,
                caller,
                idempotency_token,
            } => f
                .debug_struct("Issue")
                .field("holder_tenant_id", holder_tenant_id)
                .field("grants", grants)
                .field("as_pack", as_pack)
                .field("caller", caller)
                .field("idempotency_token", idempotency_token)
                .finish(),
            Self::Import { bytes } => f
                .debug_struct("Import")
                .field("bytes", &Redacted(bytes.len()))
                .finish(),
        }
    }
}

/// Shows the length of evidence or material bytes instead of the bytes.
struct Redacted(usize);

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{} bytes>", self.0)
    }
}

/// Root fields that the evidence fixes (ADR-0002).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RootFields {
    /// Tenant that holds the root.
    pub holder_tenant_id: Uuid,
    /// The vendor-set terms.
    pub terms: GrantTerms,
}

impl RootFields {
    /// Root fields of a holder.
    #[must_use]
    pub fn new(holder_tenant_id: Uuid, terms: GrantTerms) -> Self {
        Self {
            holder_tenant_id,
            terms,
        }
    }
}

/// A root grant that the evidence names.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AdmittedRoot {
    /// Root id. The plugin can carry it in the evidence, so that later evidence
    /// names the same grant.
    pub id: GrantId,
    /// Root fields.
    pub fields: RootFields,
}

impl AdmittedRoot {
    /// A root with its fields.
    #[must_use]
    pub fn new(id: GrantId, fields: RootFields) -> Self {
        Self { id, fields }
    }
}

/// Evidence to store. `Debug` does not print the bytes.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct NewEvidence {
    /// The exact bytes that the plugin admitted.
    pub bytes: Vec<u8>,
    /// The gear rejects a second evidence with the same key.
    pub replay_key: Option<String>,
    /// Id that the evidence carries, unique per plugin. Later evidence names
    /// this evidence by it in `replaces`.
    pub external_id: Option<String>,
    /// `external_id` of the evidence that this evidence replaces. The gear
    /// finds that evidence by it and rejects this one when there is none
    /// ([`REPLACED_EVIDENCE_MISSING`](crate::error::precondition::REPLACED_EVIDENCE_MISSING)),
    /// or when other evidence already replaced it
    /// ([`EVIDENCE_ALREADY_REPLACED`](crate::error::precondition::EVIDENCE_ALREADY_REPLACED)).
    pub replaces: Option<String>,
}

impl NewEvidence {
    /// Evidence without a replay key or an external id that replaces nothing.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            replay_key: None,
            external_id: None,
            replaces: None,
        }
    }
}

impl fmt::Debug for NewEvidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewEvidence")
            .field("bytes", &Redacted(self.bytes.len()))
            .field("replay_key", &self.replay_key)
            .field("external_id", &self.external_id)
            .field("replaces", &self.replaces)
            .finish()
    }
}

/// The value that a [`MaterialWrite`] expects the key to hold. `Debug` does not
/// print the bytes.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Expect {
    /// Any value or none.
    Any,
    /// The key is absent.
    Absent,
    /// The key holds exactly these bytes.
    Equals(Vec<u8>),
}

/// A write to the plugin material.
///
/// The gear applies the write only if the key holds what `expect` names. On a
/// mismatch, the writes of `admit` fail the admission as a conflict, and a
/// write of `verify` is dropped. A plugin that reads a value and writes a
/// larger one with `expect` set to what it read keeps that value monotonic.
/// `Debug` does not print the bytes.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MaterialWrite {
    /// Key.
    pub key: String,
    /// Value; `None` deletes the key.
    pub value: Option<Vec<u8>>,
    /// What the key must hold for the write to apply.
    pub expect: Expect,
}

impl MaterialWrite {
    /// Set `key` to `value`, whatever it holds.
    #[must_use]
    pub fn put(key: impl Into<String>, value: Vec<u8>) -> Self {
        Self {
            key: key.into(),
            value: Some(value),
            expect: Expect::Any,
        }
    }

    /// Delete `key`, whatever it holds.
    #[must_use]
    pub fn delete(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: None,
            expect: Expect::Any,
        }
    }

    /// Apply the write only if the key holds what `expect` names.
    #[must_use]
    pub fn expecting(mut self, expect: Expect) -> Self {
        self.expect = expect;
        self
    }
}

impl fmt::Debug for Expect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Any => f.write_str("Any"),
            Self::Absent => f.write_str("Absent"),
            Self::Equals(bytes) => f
                .debug_tuple("Equals")
                .field(&Redacted(bytes.len()))
                .finish(),
        }
    }
}

impl fmt::Debug for MaterialWrite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MaterialWrite")
            .field("key", &self.key)
            .field("value", &self.value.as_ref().map(|v| Redacted(v.len())))
            .field("expect", &self.expect)
            .finish()
    }
}

/// What the plugin admitted. Start from `Admission::default()`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Admission {
    /// Evidence that names roots. `None` for evidence that only updates the
    /// plugin material, for example a revocation list.
    pub evidence: Option<NewEvidence>,
    /// The roots that the evidence names.
    pub roots: Vec<AdmittedRoot>,
    /// The roots form one pack.
    pub as_pack: bool,
    /// Writes to the plugin material, in the same transaction.
    pub material: Vec<MaterialWrite>,
}

/// Evidence as stored, given to [`GrantAdmissionPluginV1::verify`]. `Debug`
/// does not print the bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredEvidence {
    /// Evidence id.
    pub id: EvidenceId,
    /// Id of the plugin that admitted it.
    pub plugin_id: String,
    /// The intake it came through.
    pub intake: Intake,
    /// The exact bytes that the plugin admitted.
    pub bytes: Vec<u8>,
}

impl fmt::Debug for StoredEvidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredEvidence")
            .field("id", &self.id)
            .field("plugin_id", &self.plugin_id)
            .field("intake", &self.intake)
            .field("bytes", &Redacted(self.bytes.len()))
            .finish()
    }
}

/// Revocation state of a root from the plugin's material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RevocationState {
    /// Suspended.
    Suspended,
    /// Terminated.
    Terminated,
}

/// Result of [`GrantAdmissionPluginV1::verify`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum VerifyOutcome {
    /// Verified. A plugin that verifies its evidence returns the root fields
    /// from it; a plugin with a trusted store returns `None`, and the stored
    /// fields apply.
    Verified(Option<Box<RootFields>>),
    /// The root is revoked by the plugin's material.
    Revoked(RevocationState),
    /// Denied.
    Denied(PluginDenial),
}

/// Why [`GrantAdmissionPluginV1::verify`] denied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PluginDenial {
    /// The signature is missing, malformed, or fails.
    SignatureInvalid,
    /// The signing key is not trusted.
    KeyUntrusted,
    /// The evidence is past its freshness.
    Stale,
    /// The clock is behind a mark that the plugin keeps.
    ClockRollback,
    /// The evidence does not name the root exactly once.
    Duplicated,
}

impl From<PluginDenial> for DenialReason {
    fn from(denial: PluginDenial) -> Self {
        match denial {
            PluginDenial::SignatureInvalid => Self::SignatureInvalid,
            PluginDenial::KeyUntrusted => Self::KeyUntrusted,
            PluginDenial::Stale => Self::Stale,
            PluginDenial::ClockRollback => Self::ClockRollback,
            PluginDenial::Duplicated => Self::Duplicated,
        }
    }
}

/// What [`GrantAdmissionPluginV1::verify`] returns.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Verification {
    /// The outcome for the root.
    pub outcome: VerifyOutcome,
    /// Writes to the plugin material, for example a clock mark. The gear
    /// applies them after `verify` returns.
    pub material: Vec<MaterialWrite>,
}

impl Verification {
    /// An outcome without material writes.
    #[must_use]
    pub fn new(outcome: VerifyOutcome) -> Self {
        Self {
            outcome,
            material: Vec::new(),
        }
    }
}

/// Why the plugin rejected evidence.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RejectionReason {
    /// The evidence cannot be parsed.
    #[error("malformed evidence: {0}")]
    Malformed(String),
    /// The signature is missing, malformed, or fails.
    #[error("signature invalid")]
    SignatureInvalid,
    /// The signing key is not trusted.
    #[error("key untrusted")]
    KeyUntrusted,
    /// The evidence is past its freshness.
    #[error("stale")]
    Stale,
    /// The clock is behind a mark that the plugin keeps.
    #[error("clock rollback")]
    ClockRollback,
    /// The evidence is already admitted.
    #[error("replay")]
    Replay,
    /// The evidence is older than the evidence that it replaces.
    #[error("version regression")]
    VersionRegression,
}

impl RejectionReason {
    /// The `field_violations[].reason` of the `InvalidArgument` that carries
    /// this rejection.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        use crate::error::reason;
        match self {
            Self::Malformed(_) => reason::EVIDENCE_MALFORMED,
            Self::SignatureInvalid => reason::EVIDENCE_SIGNATURE_INVALID,
            Self::KeyUntrusted => reason::EVIDENCE_KEY_UNTRUSTED,
            Self::Stale => reason::EVIDENCE_STALE,
            Self::ClockRollback => reason::EVIDENCE_CLOCK_ROLLBACK,
            Self::Replay => reason::EVIDENCE_REPLAY,
            Self::VersionRegression => reason::EVIDENCE_VERSION_REGRESSION,
        }
    }
}

/// A fault that prevents an answer. The gear fails closed on it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("admission plugin fault: {0}")]
pub struct PluginFault(pub String);

/// Error of [`GrantAdmissionPluginV1::admit`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AdmissionError {
    /// The plugin rejected the evidence.
    #[error(transparent)]
    Rejected(#[from] RejectionReason),
    /// The plugin could not answer.
    #[error(transparent)]
    Fault(#[from] PluginFault),
}

/// The gear store cannot answer a read of plugin material.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("plugin material store fault: {0}")]
pub struct MaterialError(pub String);

/// Read access to the plugin's own key-value material in the gear store. Only
/// the linked plugin reads it; the gear does not interpret it. The plugin
/// writes material only through [`Admission::material`] and
/// [`Verification::material`].
#[async_trait]
pub trait PluginMaterial: Send + Sync {
    /// Read a key.
    ///
    /// # Errors
    ///
    /// [`MaterialError`] when the store cannot answer.
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, MaterialError>;
}

/// The admission plugin contract, version 1.
#[async_trait]
pub trait GrantAdmissionPluginV1: Send + Sync {
    /// Identity for telemetry.
    fn info(&self) -> PluginInfo;

    /// The intakes that the plugin serves.
    fn intakes(&self) -> Intakes;

    /// Admit evidence at `instant`: return the evidence, the roots that it
    /// names, and the material to keep. The plugin checks freshness and the
    /// clock mark against `instant`, not against the system clock.
    ///
    /// # Errors
    ///
    /// [`AdmissionError::Rejected`] for evidence that the plugin does not admit;
    /// [`AdmissionError::Fault`] when it cannot answer.
    async fn admit(
        &self,
        input: AdmissionInput,
        instant: OffsetDateTime,
        material: &dyn PluginMaterial,
    ) -> Result<Admission, AdmissionError>;

    /// Verify the root `root_id` at the evaluation `instant`, the instant that
    /// the evaluator gets. Called for the selected candidate only. The plugin
    /// checks freshness and the clock mark against `instant`, not against the
    /// system clock. If `evidence` does not name `root_id` exactly once, the
    /// outcome is [`VerifyOutcome::Denied`] with [`PluginDenial::Duplicated`].
    ///
    /// # Errors
    ///
    /// [`PluginFault`] when the plugin cannot answer. Never a decision.
    async fn verify(
        &self,
        root_id: GrantId,
        evidence: &StoredEvidence,
        instant: OffsetDateTime,
        material: &dyn PluginMaterial,
    ) -> Result<Verification, PluginFault>;

    /// Health of the plugin.
    ///
    /// # Errors
    ///
    /// [`PluginFault`] when the plugin is not healthy.
    async fn health(&self) -> Result<(), PluginFault>;
}

/// Link-time registration of an admission plugin. Create it with
/// [`register_admission_plugin!`](crate::register_admission_plugin).
pub struct AdmissionPluginRegistration {
    /// Builds the plugin.
    pub factory: fn() -> Arc<dyn GrantAdmissionPluginV1>,
}

inventory::collect!(AdmissionPluginRegistration);

/// Register the admission plugin of this crate at link time.
///
/// ```ignore
/// license_enforcement_sdk::register_admission_plugin!(|| std::sync::Arc::new(MyPlugin));
/// ```
#[macro_export]
macro_rules! register_admission_plugin {
    ($factory:expr) => {
        $crate::__private::inventory::submit! {
            $crate::AdmissionPluginRegistration { factory: $factory }
        }
    };
}

/// The one admission plugin that the binary links.
///
/// # Errors
///
/// [`LinkError`] when the binary links no plugin or more than one.
pub fn linked_admission_plugin() -> Result<Arc<dyn GrantAdmissionPluginV1>, LinkError> {
    exactly_one(inventory::iter::<AdmissionPluginRegistration>).map(|r| (r.factory)())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "admission_tests.rs"]
mod admission_tests;
