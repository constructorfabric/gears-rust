// Created: 2026-08-26 by Virtuozzo International GmbH
//! The declaration repository contract.
//!
//! Read-only for now: entry 2.3's read surface is the first slice, and the
//! lifecycle mutations arrive with the flows that own them. Declaring only what
//! exists keeps the trait honest about what an implementor must supply today.

use async_trait::async_trait;
use toolkit_db::secure::DBRunner;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::category::DomainVisibility;
use crate::domain::error::DomainError;

/// A declaration as the domain sees it.
///
/// Deliberately a projection, not the row. The read surface renders `key`,
/// `value_type_id` and the resolved trait set; the columns that exist only to
/// support writes or masking stay in `infra`, so a reader cannot come to depend
/// on them by accident.
// The flags are separate facts an administrator reads back one by one; a
// state enum would only re-encode them.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    /// Surrogate identity, stable across a re-key.
    pub id: Uuid,

    /// The full setting key.
    pub key: String,

    /// The setting's own name slug, unique within its category.
    pub leaf_slug: String,

    /// GTS id of the value type the setting's values validate against — a
    /// separate fact of the declaration, not a half of the key (ADR-002).
    pub value_type_id: String,

    /// Owning category.
    pub category_id: Uuid,

    /// `global`, `cascading`, or `local`.
    pub scope_class: String,

    /// `standard` or `advanced`.
    pub mode: String,

    /// `active` or `retired`.
    pub status: String,

    /// The administrative domain, when the declaration is bound to one.
    pub domain_affinity: Option<String>,

    /// The licence feature gating the declaration, when one applies.
    ///
    /// Carried but **not yet enforced**: the gate belongs to the License
    /// Resolver, which has a design and no implementation. Surfacing the field
    /// lets a caller see what would gate it once the resolver exists.
    pub licence_feature: Option<String>,

    /// The contributing module, for module-contributed declarations.
    pub owner_module: Option<String>,

    /// Optional long-form description.
    pub description: Option<String>,

    /// The Schema Default — the floor every resolution terminates in.
    pub default_value: serde_json::Value,

    /// Whether the value type carries the `secret` trait.
    pub has_secret_trait: bool,

    /// `public`, `pii` or `secret`.
    pub data_classification: String,

    /// Whether changing the value needs a fresh re-authentication.
    pub requires_step_up: bool,

    /// Whether the effective value may be served on the anonymous surface.
    pub anonymous_exposable: bool,

    /// `admin_authored` or `module_contributed`.
    pub source: String,

    /// When the declaration's definition last changed — the definition arm of
    /// the recency indicator, never an aggregate over the setting's values.
    pub last_change_at: time::OffsetDateTime,

    /// Row version, refreshed by every write including metadata edits.
    pub updated_at: time::OffsetDateTime,
}

/// A declaration about to be inserted, every column decided.
// The flags are separate facts an administrator reads back one by one; a
// state enum would only re-encode them.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarationDraft {
    /// The full setting key.
    pub key: String,
    /// The setting's own name slug.
    pub leaf_slug: String,
    /// The value type its values validate against.
    pub value_type_id: String,
    /// The owning category.
    pub category_id: Uuid,
    /// The Schema Default.
    pub default_value: serde_json::Value,
    /// `global`, `cascading` or `local`.
    pub scope_class: String,
    /// `standard` or `advanced`.
    pub mode: String,
    /// Whether changing the value needs a fresh re-authentication.
    pub requires_step_up: bool,
    /// Whether the effective value may be served on the anonymous surface.
    pub anonymous_exposable: bool,
    /// The administrative domain, if any.
    pub domain_affinity: Option<String>,
    /// Whether the value type carries the `secret` trait.
    pub has_secret_trait: bool,
    /// `public`, `pii` or `secret`.
    pub data_classification: String,
    /// `admin_authored` or `module_contributed`.
    pub source: String,
    /// The contributing module, for a contributed declaration.
    pub owner_module: Option<String>,
    /// The licence feature gating the setting, if any.
    pub licence_feature: Option<String>,
    /// Human-readable description.
    pub description: Option<String>,
    /// The principal or module that created the row.
    pub created_by: String,
}

/// The metadata a reconcile may change in place at the same major.
///
/// Nothing here alters a live setting's resolution: the Schema Default, the
/// value type and the scope class are absent on purpose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarationMetadata {
    /// `standard` or `advanced`.
    pub mode: String,
    /// Human-readable description.
    pub description: Option<String>,
    /// The administrative domain, if any.
    pub domain_affinity: Option<String>,
    /// The licence feature gating the setting, if any.
    pub licence_feature: Option<String>,
    /// `public`, `pii` or `secret`.
    pub data_classification: String,
    /// Whether changing the value needs a fresh re-authentication.
    pub requires_step_up: bool,
    /// Whether the effective value may be served on the anonymous surface.
    pub anonymous_exposable: bool,
}

impl DeclarationMetadata {
    /// Whether applying this metadata to `current` changes what a reader is
    /// served — and so moves `last_change_at`, the definition arm of the
    /// effective recency.
    ///
    /// The classification decides masking, step-up gates the write, anonymous
    /// exposure and the licence feature decide who is served at all, the
    /// domain affinity decides where the setting is visible. A description or
    /// a mode is how the setting is presented, not what it is: it moves the
    /// tag (`updated_at`) and nothing else.
    #[must_use]
    pub fn redefines(&self, current: &Declaration) -> bool {
        self.data_classification != current.data_classification
            || self.requires_step_up != current.requires_step_up
            || self.anonymous_exposable != current.anonymous_exposable
            || self.licence_feature != current.licence_feature
            || self.domain_affinity != current.domain_affinity
    }
}

/// Operations on declarations.
#[async_trait]
pub trait DeclarationRepository: Send + Sync {
    /// The declaration at exactly this key, whatever its status.
    async fn find_by_key<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: &str,
    ) -> Result<Option<Declaration>, DomainError>;

    /// Every declaration whose key starts with `key_prefix` — the base and the
    /// version-stripped path followed by `.v` — whatever its status. Callers
    /// re-check the stripped path exactly, since a `LIKE` prefix is not a
    /// token boundary.
    async fn find_by_key_prefix<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key_prefix: &str,
    ) -> Result<Vec<Declaration>, DomainError>;

    /// Insert a new, active declaration.
    async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        draft: DeclarationDraft,
    ) -> Result<Declaration, DomainError>;

    /// Update the metadata that may change in place, stamping `updated_at`;
    /// `last_change_at` too when `redefines` says the change alters what a
    /// reader is served (see [`DeclarationMetadata::redefines`]) — the caller
    /// decides from the row it holds, the repository takes the decision.
    ///
    /// `expected` is the `updated_at` the caller compared the tag against, and
    /// the write applies to the row at that version alone; `None` writes
    /// unconditionally, for a caller that holds no tag and works under its own
    /// transaction — a reconcile, a revive, an upgrade.
    ///
    /// # Errors
    /// [`DomainError::PreconditionFailed`] when a version was given and no row
    /// is at it any more.
    async fn update_metadata<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        metadata: DeclarationMetadata,
        expected: Option<time::OffsetDateTime>,
        redefines: bool,
    ) -> Result<(), DomainError>;

    /// Move a declaration between `active` and `retired`, stamping
    /// `last_change_at` and `updated_at`; `expected` as for
    /// [`Self::update_metadata`].
    ///
    /// # Errors
    /// [`DomainError::PreconditionFailed`] when a version was given and no row
    /// is at it any more.
    async fn set_status<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        status: &str,
        expected: Option<time::OffsetDateTime>,
    ) -> Result<(), DomainError>;

    /// The declaration by id, share-locked for the rest of the caller's
    /// transaction.
    ///
    /// What a value write reads before it stores: a retire or a major upgrade
    /// under way holds this row's update lock, so the read waits for it and
    /// sees `retired`; one that starts later waits for the write's commit, and
    /// the values it retires or copies include the write.
    async fn find_locked<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Declaration>, DomainError>;

    /// Take the declaration row for update for the rest of the caller's
    /// transaction, changing nothing.
    ///
    /// What a change to the setting's administrative state does when it writes
    /// no column of the row itself — a restriction set or cleared — so that a
    /// value write in flight, which holds the row for share until its commit,
    /// is serialized against it: the write either commits first, or re-derives
    /// its access after this transaction commits and is refused.
    ///
    /// # Errors
    /// [`DomainError::NotFound`] when there is no such row.
    async fn lock_for_update<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<(), DomainError>;

    /// Adopt a re-declaration's value type and Schema Default — the definition
    /// arm of the effective recency — stamping `last_change_at` and
    /// `updated_at`.
    ///
    /// Only a revive calls this: an active declaration's definition never
    /// changes in place, it rides a new major.
    async fn set_definition<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        value_type_id: &str,
        default_value: &serde_json::Value,
    ) -> Result<(), DomainError>;

    /// Fetch one declaration by id, within the caller's scope and visibility.
    ///
    /// The visibility predicate is applied here rather than by the caller: a
    /// gated declaration must be indistinguishable from an absent one, and a
    /// repository that returned the row and left the filtering to a service
    /// would make that a decision each call site could forget.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn find<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        visibility: &DomainVisibility,
        id: Uuid,
    ) -> Result<Option<Declaration>, DomainError>;

    /// Every declaration filed under a category, whatever its status.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn find_by_category<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        category_id: Uuid,
    ) -> Result<Vec<Declaration>, DomainError>;

    /// List declarations for the caller, leaving out those `hidden` for the
    /// tenant whose root-to-self chain is `hidden_for` — in the query, so the
    /// page is cut and counted after the exclusion. An empty chain excludes
    /// nothing.
    ///
    /// # Errors
    /// [`DomainError::Validation`] when the query names an unmapped field, uses
    /// an unsupported operator, or carries an undecodable cursor.
    async fn list<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        visibility: &DomainVisibility,
        hidden_for: &[Uuid],
        query: &ODataQuery,
    ) -> Result<Page<Declaration>, DomainError>;
}
