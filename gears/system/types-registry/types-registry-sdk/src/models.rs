//! Requests, operations and projected entity snapshots.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::num::NonZeroU8;
use std::str::FromStr;

use gts::{GtsId, GtsIdPattern, GtsIdSegment, GtsInstanceId, GtsTypeId};
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use uuid::Uuid;

/// One authored or materialized JSON document.
pub type JsonDocument = serde_json::Value;

// keys and selection.

/// How a caller names one entity: its GTS identifier or its Registry Reference.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EntityKey {
    GtsId(GtsId),
    GtsUuid(Uuid),
}

impl EntityKey {
    /// Kind from the identifier’s trailing `~`; Registry References carry no kind.
    #[must_use]
    pub fn kind(&self) -> Option<EntityKind> {
        match self {
            Self::GtsId(id) => Some(EntityKind::of(id)),
            Self::GtsUuid(_) => None,
        }
    }
}

impl From<GtsId> for EntityKey {
    fn from(id: GtsId) -> Self {
        Self::GtsId(id)
    }
}

impl From<Uuid> for EntityKey {
    fn from(uuid: Uuid) -> Self {
        Self::GtsUuid(uuid)
    }
}

impl fmt::Display for EntityKey {
    /// The canonical spelling: the identifier, or the hyphenated lowercase UUID.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GtsId(id) => id.fmt(f),
            Self::GtsUuid(uuid) => uuid.hyphenated().fmt(f),
        }
    }
}

/// Opaque freshness token; compare for equality.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Validator(Vec<u8>);

impl Validator {
    /// Wraps validator bytes received from the registry.
    #[must_use]
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for Validator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Validator")
            .field(&String::from_utf8_lossy(&self.0))
            .finish()
    }
}

/// One key of a batch read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchGetItem {
    pub key: EntityKey,
    /// Makes the read conditional for this key alone; `None` reads it unconditionally.
    pub if_none_match: Option<Validator>,
}

impl From<EntityKey> for BatchGetItem {
    fn from(key: EntityKey) -> Self {
        Self {
            key,
            if_none_match: None,
        }
    }
}

/// Batch-read key ceiling; larger reads are split.
pub const MAX_BATCH_GET_KEYS: usize = 100;

/// A batch read: every key answered under one projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchGetEntitiesRequest {
    pub items: Vec<BatchGetItem>,
    pub projection: Projection,
    /// Bypass any client cache for an authoritative read. Not sent on the wire.
    pub fresh: bool,
}

/// A selectable or mandatory entity field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntityField {
    GtsId,
    GtsUuid,
    Kind,
    LifecycleStatus,
    Origin,
    Content,
    ResolvedSchema,
    EffectiveTraits,
    EffectiveTraitsSchema,
    Provenance,
}

impl EntityField {
    pub const ALL: [Self; 10] = [
        Self::GtsId,
        Self::GtsUuid,
        Self::Kind,
        Self::LifecycleStatus,
        Self::Origin,
        Self::Content,
        Self::ResolvedSchema,
        Self::EffectiveTraits,
        Self::EffectiveTraitsSchema,
        Self::Provenance,
    ];

    /// Present on every snapshot, whatever the selection.
    pub const MANDATORY: [Self; 4] = [
        Self::GtsId,
        Self::GtsUuid,
        Self::Kind,
        Self::LifecycleStatus,
    ];

    /// What [`Projection::Default`] selects on single, batch and discovery reads: no documents.
    pub const DEFAULT: [Self; 5] = [
        Self::GtsId,
        Self::GtsUuid,
        Self::Kind,
        Self::LifecycleStatus,
        Self::Origin,
    ];

    /// The wire name, as `$select` spells it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::GtsId => "gts_id",
            Self::GtsUuid => "gts_uuid",
            Self::Kind => "kind",
            Self::LifecycleStatus => "lifecycle_status",
            Self::Origin => "origin",
            Self::Content => "content",
            Self::ResolvedSchema => "resolved_schema",
            Self::EffectiveTraits => "effective_traits",
            Self::EffectiveTraitsSchema => "effective_traits_schema",
            Self::Provenance => "provenance",
        }
    }
}

/// Typed selection always containing [`EntityField::MANDATORY`]; empty/unknown sets are impossible.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FieldSelection(BTreeSet<EntityField>);

impl FieldSelection {
    #[must_use]
    pub fn light() -> Self {
        Self(EntityField::MANDATORY.into_iter().collect())
    }

    /// The mandatory fields plus `fields`.
    #[must_use]
    pub fn with(fields: &[EntityField]) -> Self {
        let mut selection = Self::light();
        selection.0.extend(fields.iter().copied());
        selection
    }

    #[must_use]
    pub fn full() -> Self {
        Self(EntityField::ALL.into_iter().collect())
    }

    #[must_use]
    pub fn contains(&self, field: EntityField) -> bool {
        self.0.contains(&field)
    }

    /// The selected fields in a stable order.
    pub fn fields(&self) -> impl Iterator<Item = EntityField> + '_ {
        self.0.iter().copied()
    }
}

/// Which fields a read asks for.
///
/// `Default` and an explicit selection compare differently. Use [`Self::normalized`]
/// to compare effective field sets; typed list helpers give `Default` their documents.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum Projection {
    /// The read's default: the document-free [`EntityField::DEFAULT`] fields on
    /// `batch_get_entities` and `list_entities`; documents on the kind-narrowed list
    /// helpers.
    #[default]
    Default,
    /// Exactly these fields, the mandatory ones included.
    Select(FieldSelection),
}

impl Projection {
    /// The field set the registry answers: [`EntityField::DEFAULT`] for `Default`.
    #[must_use]
    pub fn normalized(&self) -> FieldSelection {
        match self {
            Self::Default => FieldSelection::with(&EntityField::DEFAULT),
            Self::Select(selection) => selection.clone(),
        }
    }
}

/// Projection and client-cache policy for single and batch typed read helpers.
///
/// The default uses [`Projection::Default`] and `fresh: false`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadOptions {
    /// The fields to retrieve.
    pub projection: Projection,
    /// Bypass any client cache for this read. Not sent on the wire.
    /// Has no effect when the client has no cache.
    pub fresh: bool,
}

// results.

/// Every key's answer, keyed by the key as asked.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BatchGetEntitiesResponse(pub HashMap<EntityKey, EntityLookup>);

/// Per-key read result; match future variants with a fallback arm.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum EntityLookup {
    /// `etag` is scoped to the normalized projection, so it changes with it.
    Found {
        entity: Box<Entity>,
        etag: Validator,
    },
    /// Matching validator: no snapshot transferred; every non-`NotFound` answer carries an etag.
    Unchanged { etag: Validator },
    /// Absent.
    NotFound,
}

/// Projected entity with mandatory identity, kind and lifecycle.
/// `None` is unselected or inapplicable; `Some(Null)` is a selected JSON null.
#[derive(Debug, Clone, PartialEq)]
pub struct Entity {
    pub gts_id: GtsId,
    pub gts_uuid: Uuid,
    pub kind: EntityKind,
    pub lifecycle_status: LifecycleStatus,
    pub origin: Option<Origin>,
    /// The authored document, whichever kind it is.
    pub content: Option<JsonDocument>,
    /// Type Schemas only.
    pub resolved_schema: Option<JsonDocument>,
    /// Type Schemas only.
    pub effective_traits: Option<JsonDocument>,
    /// Type Schemas only.
    pub effective_traits_schema: Option<JsonDocument>,
    /// Managed Type Schemas only; see [`Provenance`].
    pub provenance: Option<Provenance>,
}

impl Entity {
    /// The identifier's segments, base first.
    #[must_use]
    pub fn segments(&self) -> &[GtsIdSegment] {
        self.gts_id.segments()
    }
}

/// Projected Type Schema. Unselected fields are `None`; selected JSON null is `Some(Null)`.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeSchema {
    pub type_id: GtsTypeId,
    pub type_uuid: Uuid,
    pub lifecycle_status: LifecycleStatus,
    pub origin: Option<Origin>,
    /// The authored schema.
    pub content: Option<JsonDocument>,
    pub resolved_schema: Option<JsonDocument>,
    pub effective_traits: Option<JsonDocument>,
    pub effective_traits_schema: Option<JsonDocument>,
    /// How the current revision was admitted; `None` when unselected.
    pub provenance: Option<Provenance>,
}

/// Projected Instance; `type_id` identifies its conforming Type Schema.
#[derive(Debug, Clone, PartialEq)]
pub struct Instance {
    pub id: GtsInstanceId,
    pub uuid: Uuid,
    pub type_id: GtsTypeId,
    pub lifecycle_status: LifecycleStatus,
    pub origin: Option<Origin>,
    /// The authored document.
    pub content: Option<JsonDocument>,
}

impl TryFrom<Entity> for TypeSchema {
    /// A snapshot of the other kind, handed back unchanged.
    type Error = Entity;

    fn try_from(snapshot: Entity) -> Result<Self, Self::Error> {
        if snapshot.kind != EntityKind::TypeSchema || !snapshot.gts_id.is_type() {
            return Err(snapshot);
        }
        let Ok(type_id) = GtsTypeId::try_new(snapshot.gts_id.as_ref()) else {
            return Err(snapshot);
        };
        Ok(Self {
            type_id,
            type_uuid: snapshot.gts_uuid,
            lifecycle_status: snapshot.lifecycle_status,
            origin: snapshot.origin,
            content: snapshot.content,
            resolved_schema: snapshot.resolved_schema,
            effective_traits: snapshot.effective_traits,
            effective_traits_schema: snapshot.effective_traits_schema,
            provenance: snapshot.provenance,
        })
    }
}

impl TryFrom<Entity> for Instance {
    /// A snapshot of the other kind, handed back unchanged.
    type Error = Entity;

    fn try_from(snapshot: Entity) -> Result<Self, Self::Error> {
        // Reject Type Schema-only fields rather than silently dropping them.
        if snapshot.kind != EntityKind::Instance
            || snapshot.gts_id.is_type()
            || snapshot.resolved_schema.is_some()
            || snapshot.effective_traits.is_some()
            || snapshot.effective_traits_schema.is_some()
            || snapshot.provenance.is_some()
        {
            return Err(snapshot);
        }
        let ids = GtsInstanceId::try_new(snapshot.gts_id.as_ref()).ok().zip(
            snapshot
                .gts_id
                .get_type_id()
                .and_then(|type_id| GtsTypeId::try_new(&type_id).ok()),
        );
        let Some((id, type_id)) = ids else {
            return Err(snapshot);
        };
        Ok(Self {
            id,
            uuid: snapshot.gts_uuid,
            type_id,
            lifecycle_status: snapshot.lifecycle_status,
            origin: snapshot.origin,
            content: snapshot.content,
        })
    }
}

/// Admission metadata of a managed Type Schema, selected as one group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Provenance {
    /// Whether the cross-minor check was skipped when this revision was admitted.
    /// `false` does not imply that a check ran. See [`RegisterItem::skip_compatibility_check`].
    pub compatibility_check_skipped: bool,
}

/// Type Schema or Instance; a GTS identifier's trailing `~` decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityKind {
    TypeSchema,
    Instance,
}

impl EntityKind {
    /// The kind `id` names.
    #[must_use]
    pub fn of(id: &GtsId) -> Self {
        if id.is_type() {
            Self::TypeSchema
        } else {
            Self::Instance
        }
    }
}

/// Entity origin; match future variants with a fallback arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Origin {
    /// Managed entity version and timestamps.
    Managed {
        resource_version: u64,
        created_at: OffsetDateTime,
        updated_at: OffsetDateTime,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LifecycleStatus {
    Active,
    Deleted,
}

// discovery.

/// Discovery restrictions; absent fields are unrestricted, except lifecycle defaults to Active.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EntityFilter {
    pub pattern: Option<GtsIdPattern>,
    /// Maximum identifier segment count; REST depth rejects zero.
    pub max_chain_depth: Option<NonZeroU8>,
    pub kind: Option<EntityKind>,
    pub lifecycle: LifecycleFilter,
}

/// Which lifecycle states a page lists; tombstones only on request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum LifecycleFilter {
    #[default]
    Active,
    Deleted,
    All,
}

/// Opaque cursor bound to its filter and projection; changed queries cannot resume it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Cursor(String);

impl Cursor {
    /// Wraps a position received from the registry.
    #[must_use]
    pub fn from_token(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Page size and position. `limit: None` takes the registry's default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageRequest {
    pub limit: Option<u32>,
    pub cursor: Option<Cursor>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ListEntitiesRequest {
    pub filter: EntityFilter,
    pub projection: Projection,
    pub page: PageRequest,
}

/// One bounded page; `next` is absent on the last one.
#[derive(Debug, Clone, PartialEq)]
pub struct ListEntitiesResponse {
    pub items: Vec<Entity>,
    pub next: Option<Cursor>,
}

// write path.

/// Mutation replay key for one identical request; changed requests need a new key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// Maximum key length in bytes.
    pub const MAX_LEN: usize = 255;

    /// A fresh random key.
    #[must_use]
    pub fn generate() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    /// Accept 1..=`Self::MAX_LEN` printable ASCII bytes (0x20..=0x7E), without surrounding spaces.
    ///
    /// # Errors
    /// `InvalidArgument` naming `idempotency_key`.
    pub fn new(key: impl Into<String>) -> Result<Self, CanonicalError> {
        let key = key.into();
        let refusal = if key.is_empty() {
            Some("an idempotency key must not be empty".to_owned())
        } else if key.len() > Self::MAX_LEN {
            Some(format!(
                "an idempotency key must be at most {} bytes; this one is {}",
                Self::MAX_LEN,
                key.len()
            ))
        } else if !key.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
            Some("an idempotency key must be printable ASCII".to_owned())
        } else if key.starts_with(' ') || key.ends_with(' ') {
            Some("an idempotency key must not start or end with a space".to_owned())
        } else {
            None
        };
        match refusal {
            None => Ok(Self(key)),
            Some(detail) => Err(crate::gts::TypeResource::invalid_argument()
                .with_field_violation(
                    crate::field::IDEMPOTENCY_KEY_FIELD,
                    detail,
                    crate::field::INVALID_IDEMPOTENCY_KEY,
                )
                .create()),
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Publisher `SemVer` precedence: prereleases count; build metadata affects display only.
/// Length is bounded by [`Self::MAX_LEN`]. Use the publishing crate’s `CARGO_PKG_VERSION`.
#[derive(Debug, Clone)]
pub struct PublisherVersion(semver::Version);

impl PublisherVersion {
    /// Maximum version-text bytes, checked before parsing to bound untrusted input.
    pub const MAX_LEN: usize = 128;
}

/// Why a [`PublisherVersion`] was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PublisherVersionError {
    /// The input exceeds [`PublisherVersion::MAX_LEN`] bytes.
    #[error("publisher version is {len} bytes long, more than the {max} allowed")]
    TooLong { len: usize, max: usize },
    /// The input is not a valid `SemVer` 2.0.0 version.
    #[error("publisher version is not valid SemVer: {message}")]
    Invalid { message: String },
}

impl FromStr for PublisherVersion {
    type Err = PublisherVersionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() > Self::MAX_LEN {
            return Err(PublisherVersionError::TooLong {
                len: s.len(),
                max: Self::MAX_LEN,
            });
        }
        semver::Version::parse(s)
            .map(Self)
            .map_err(|e| PublisherVersionError::Invalid {
                message: e.to_string(),
            })
    }
}

impl TryFrom<&str> for PublisherVersion {
    type Error = PublisherVersionError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl fmt::Display for PublisherVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl PartialEq for PublisherVersion {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for PublisherVersion {}

impl PartialOrd for PublisherVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PublisherVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp_precedence(&other.0)
    }
}

impl Hash for PublisherVersion {
    // Ignore build metadata, matching cmp_precedence; destructuring detects added fields.
    fn hash<H: Hasher>(&self, state: &mut H) {
        let semver::Version {
            major,
            minor,
            patch,
            pre,
            build: _,
        } = &self.0;
        major.hash(state);
        minor.hash(state);
        patch.hash(state);
        pre.hash(state);
    }
}

/// Publishing gear and its own `CARGO_PKG_VERSION`; shared helpers only forward it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PublisherContext {
    pub name: String,
    pub version: PublisherVersion,
}

/// Required publisher identity and version for a mutation request.
#[derive(Debug, Clone, PartialEq)]
pub struct RegisterEntitiesRequest {
    pub items: Vec<RegisterItem>,
    pub dry_run: bool,
    pub publisher: PublisherContext,
}

/// One candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct RegisterItem {
    /// Instance identity may be absent from its document; Type Schema `$id` must be
    /// `gts://<gts_id>`.
    pub gts_id: GtsId,
    pub content: JsonDocument,
    /// `Some(v)`: must still be at `v`. `None`: must not exist.
    pub expected_resource_version: Option<u64>,
    /// Request skipping the cross-minor check for a new minor with a predecessor (major > 0).
    /// Requires deployment permission; all other validation still applies.
    /// Read the outcome in [`Provenance::compatibility_check_skipped`].
    pub skip_compatibility_check: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeleteEntitiesRequest {
    pub items: Vec<DeleteItem>,
    pub dry_run: bool,
    pub publisher: PublisherContext,
}

/// Names the target as a read does; the precondition is required.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteItem {
    pub key: EntityKey,
    pub expected_resource_version: u64,
}

/// Operation state read back from the registry after acceptance.
#[derive(Debug, Clone)]
pub enum Operation {
    Registration(RegistrationOperation),
    Deletion(DeletionOperation),
}

impl Operation {
    #[must_use]
    pub fn operation_id(&self) -> Uuid {
        match self {
            Self::Registration(op) => op.operation_id,
            Self::Deletion(op) => op.operation_id,
        }
    }

    #[must_use]
    pub fn status(&self) -> OperationStatus {
        match self {
            Self::Registration(op) => op.status,
            Self::Deletion(op) => op.status,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RegistrationOperation {
    pub operation_id: Uuid,
    pub status: OperationStatus,
    pub items: Vec<RegistrationItemResult>,
}

/// One registration candidate's state in an operation.
#[derive(Debug, Clone)]
pub struct RegistrationItemResult {
    pub gts_id: GtsId,
    pub outcome: RegistrationOutcome,
}

#[derive(Debug, Clone)]
pub struct DeletionOperation {
    pub operation_id: Uuid,
    pub status: OperationStatus,
    pub items: Vec<DeletionItemResult>,
}

/// One deletion target's state in an operation.
#[derive(Debug, Clone)]
pub struct DeletionItemResult {
    /// The target's key, in canonical form.
    pub entity_key: EntityKey,
    pub outcome: DeletionOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperationStatus {
    Pending,
    Running,
    Completed,
}

/// Where one registration candidate stands, with exactly the data that state has.
#[derive(Debug, Clone)]
pub enum RegistrationOutcome {
    /// Not yet evaluated.
    Pending,
    /// Being evaluated.
    Running,
    /// Committed at `resource_version`.
    Succeeded { resource_version: u64 },
    /// A dry run would have committed it; a dry run allocates no version.
    WouldSucceed,
    /// An update whose content equals the current revision's: nothing was written, and
    /// `resource_version` is the existing one. Dry runs report it too.
    Unchanged { resource_version: u64 },
    /// Refused. Decode reason and context with
    /// [`AdmissionFailure::from_canonical`](crate::AdmissionFailure::from_canonical).
    Failed { error: CanonicalError },
}

/// Where one deletion target stands, with exactly the data that state has. A deletion is
/// never unchanged.
#[derive(Debug, Clone)]
pub enum DeletionOutcome {
    /// Not yet evaluated.
    Pending,
    /// Being evaluated.
    Running,
    /// Deleted; the tombstone is at `resource_version`.
    Succeeded { resource_version: u64 },
    /// A dry run would have deleted it; a dry run allocates no version.
    WouldSucceed,
    /// Refused. Decode reason and context with
    /// [`AdmissionFailure::from_canonical`](crate::AdmissionFailure::from_canonical).
    Failed { error: CanonicalError },
}

impl RegistrationOutcome {
    /// The state without its data.
    #[must_use]
    pub fn status(&self) -> CandidateStatus {
        match self {
            Self::Pending => CandidateStatus::Pending,
            Self::Running => CandidateStatus::Running,
            Self::Succeeded { .. } | Self::WouldSucceed => CandidateStatus::Succeeded,
            Self::Unchanged { .. } => CandidateStatus::Unchanged,
            Self::Failed { .. } => CandidateStatus::Failed,
        }
    }

    /// The candidate will not change again.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.status().is_terminal()
    }

    /// The version the entity stands at: committed, or found unchanged.
    #[must_use]
    pub fn resource_version(&self) -> Option<u64> {
        match self {
            Self::Succeeded { resource_version } | Self::Unchanged { resource_version } => {
                Some(*resource_version)
            }
            _ => None,
        }
    }

    /// The refusal, when the candidate failed.
    #[must_use]
    pub fn error(&self) -> Option<&CanonicalError> {
        match self {
            Self::Failed { error } => Some(error),
            _ => None,
        }
    }
}

impl DeletionOutcome {
    /// The state without its data.
    #[must_use]
    pub fn status(&self) -> CandidateStatus {
        match self {
            Self::Pending => CandidateStatus::Pending,
            Self::Running => CandidateStatus::Running,
            Self::Succeeded { .. } | Self::WouldSucceed => CandidateStatus::Succeeded,
            Self::Failed { .. } => CandidateStatus::Failed,
        }
    }

    /// The target will not change again.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.status().is_terminal()
    }

    /// The tombstone's version, once deleted.
    #[must_use]
    pub fn resource_version(&self) -> Option<u64> {
        match self {
            Self::Succeeded { resource_version } => Some(*resource_version),
            _ => None,
        }
    }

    /// The refusal, when the target failed.
    #[must_use]
    pub fn error(&self) -> Option<&CanonicalError> {
        match self {
            Self::Failed { error } => Some(error),
            _ => None,
        }
    }
}

/// An item outcome without its data, for matching and logging. A dry run's `WouldSucceed`
/// is `Succeeded` here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CandidateStatus {
    Pending,
    Running,
    Succeeded,
    Unchanged,
    Failed,
}

impl CandidateStatus {
    /// `Succeeded`, `Unchanged` or `Failed`: the candidate will not change again.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Unchanged | Self::Failed)
    }
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod models_tests;
