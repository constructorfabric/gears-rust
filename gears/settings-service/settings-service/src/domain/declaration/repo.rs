//! The declaration repository contract.
//!
//! Read-only for now: entry 2.3's read surface is the first slice, and the
//! lifecycle mutations arrive with the flows that own them. Declaring only what
//! exists keeps the trait honest about what an implementor must supply today.

use std::collections::HashMap;

use async_trait::async_trait;
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;
use toolkit_odata::{CursorV1, ODataQuery, Page};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::category::DomainVisibility;
use crate::domain::declaration::kinds::{DataClassification, DeclarationStatus};
use crate::domain::error::DomainError;
use crate::domain::odata::Listing;
use crate::domain::search::needle::{Corpus, Needle};

/// A declaration as the domain sees it.
///
/// Deliberately a projection, not the row. The read surface renders `key`,
/// `value_type_id` and the resolved trait set; the columns that exist only to
/// support writes or masking stay in `infra`, so a reader cannot come to depend
/// on them by accident.
// The flags are separate facts an administrator reads back one by one; a
// state enum would only re-encode them.
#[domain_model]
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

    /// The lifecycle status: active, or retired and out of resolution.
    pub status: DeclarationStatus,

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

    /// Optional human-readable label; never derived from `leaf_slug`.
    pub display_name: Option<String>,

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

impl Declaration {
    /// The data classification. A stored class that is none of the three
    /// reads as `secret` — fail closed, so it masks rather than shows.
    #[must_use]
    pub fn classification(&self) -> DataClassification {
        DataClassification::from_stored(&self.data_classification)
    }
}

/// A declaration about to be inserted, every column decided.
// The flags are separate facts an administrator reads back one by one; a
// state enum would only re-encode them.
#[domain_model]
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
    /// Human-readable label, trimmed.
    pub display_name: Option<String>,
    /// Human-readable description.
    pub description: Option<String>,
    /// The principal or module that created the row.
    pub created_by: String,
}

/// The metadata a reconcile may change in place at the same major.
///
/// Nothing here alters a live setting's resolution: the Schema Default, the
/// value type and the scope class are absent on purpose.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarationMetadata {
    /// `standard` or `advanced`.
    pub mode: String,
    /// Human-readable label, trimmed. Presentation only: it never moves
    /// `last_change_at`.
    pub display_name: Option<String>,
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

/// How many settings a category holds for one reader, and how they split.
///
/// The three are counted over one set — the declarations the reader's browse
/// of the category would list, whatever their status — so `settings` is the
/// rail's number and the other two are subsets of it: the settings tagged
/// `advanced`, which a console groups apart and may fold away, and the ones
/// `retired`, which browse still lists under their own outcome.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CategoryTally {
    /// Every declaration the reader can see in the category.
    pub settings: u64,
    /// Of those, the ones tagged `advanced`.
    pub advanced: u64,
    /// Of those, the ones `retired`.
    pub retired: u64,
}

/// What the browse narrowing by text asks the candidate read: the needle, the
/// classifications it may match a declaration's Schema Default against, and
/// the spelling of the substring predicate.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct BrowseNarrowing<'a> {
    /// The validated, trimmed text.
    pub needle: &'a Needle,
    /// The corpus the caller's entitlement admits, decided before the read.
    pub corpus: Corpus,
    /// Whether the database in use is `PostgreSQL` (`ILIKE`), as against the
    /// portable `LIKE` spelling. A flag rather than a binding of the
    /// repository: `DeclarationRepo` is built at about fifty sites and cannot
    /// read the backend from its connection. A wrong value is SQL in the wrong
    /// dialect.
    pub postgres: bool,
}

/// One candidate of a narrowed browse batch: a declaration, whether its own
/// text matched, and its place in the listing, from which a cursor continues
/// after it or before it, so a page can end on any candidate of the batch.
#[domain_model]
#[derive(Debug, Clone)]
pub struct BrowseCandidate {
    /// The declaration.
    pub declaration: Declaration,
    /// Whether the declaration text (`key`, `display_name`, `description`,
    /// category name, Schema Default) contains the needle.
    pub text_matched: bool,
    /// The forward cursor positioned after this row, not yet encoded; the
    /// same position with `d = "bwd"` is the cursor before it.
    pub position: CursorV1,
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
        status: DeclarationStatus,
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

    /// Hard-delete a declaration at the version the caller compared its tag
    /// against. The only path that hard-deletes a declaration; its
    /// `setting_values`, `tenant_permissions` and `pending_secrets` go by
    /// `ON DELETE CASCADE`.
    ///
    /// # Errors
    /// [`DomainError::PreconditionFailed`] when no row is at `expected` any
    /// more.
    async fn delete<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        expected: time::OffsetDateTime,
    ) -> Result<(), DomainError>;

    /// Adopt a re-declaration's Schema Default — the definition arm of the
    /// effective recency — stamping `last_change_at` and `updated_at`.
    ///
    /// Only a revive calls this: an active declaration's definition never
    /// changes in place, it rides a new major. The value type is not here to
    /// change: the setting's own GTS type is registered with it, and the
    /// registry does not replace a registered type.
    async fn set_default<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
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

    /// The active declaration of a category that holds `leaf_slug`, if any —
    /// the one a revive of a retired declaration with that leaf would collide
    /// with.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn find_active_by_leaf<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        category_id: Uuid,
        leaf_slug: &str,
    ) -> Result<Option<Declaration>, DomainError> {
        Ok(self
            .find_by_category(conn, scope, category_id)
            .await?
            .into_iter()
            .find(|d| d.status.is_active() && d.leaf_slug == leaf_slug))
    }

    /// List declarations for the caller, leaving out those `hidden` for the
    /// tenant whose root-to-self chain is `hidden_for` — in the query, so the
    /// page is cut and counted after the exclusion. An empty chain excludes
    /// nothing. The listing carries the size of the whole filtered set. With
    /// `flagged_at`, only the declarations whose own override at that tenant
    /// is flagged for review — also in the query.
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
        flagged_at: Option<Uuid>,
        query: &ODataQuery,
    ) -> Result<Listing<Declaration>, DomainError>;

    /// One batch of the narrowed browse: the same page as [`Self::list`], each
    /// candidate marked with whether its declaration text matched — one read
    /// over the batch's ids with the search's predicate expressions, never a
    /// conjunct of the page query — and carrying its position in the
    /// listing. The `query`'s `filter_hash` is what a cursor minted from a
    /// position is bound to.
    ///
    /// # Errors
    /// [`DomainError::Validation`] as for [`Self::list`]; [`DomainError`] when
    /// a read fails.
    #[allow(clippy::too_many_arguments)]
    async fn list_candidates<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        visibility: &DomainVisibility,
        hidden_for: &[Uuid],
        flagged_at: Option<Uuid>,
        query: &ODataQuery,
        narrowing: &BrowseNarrowing<'_>,
    ) -> Result<Page<BrowseCandidate>, DomainError>;

    /// Which of `keys` name a declaration the caller may see: within the
    /// caller's grant and administrative domains and not `hidden` on the
    /// chain `hidden_for` — the three gates of [`Self::list`], in one read
    /// however many keys are asked.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn visible_keys<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        visibility: &DomainVisibility,
        hidden_for: &[Uuid],
        keys: &[String],
    ) -> Result<Vec<String>, DomainError>;

    /// The declarations with these ids, in one read.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn find_by_ids<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        ids: &[Uuid],
    ) -> Result<Vec<Declaration>, DomainError>;

    /// Count the declarations of each of `category_ids` as [`Self::list`]
    /// would list them for the same caller — the same scope, visibility and
    /// `hidden` exclusion, every status — split by mode and status.
    ///
    /// A category with nothing to count is absent from the answer; the
    /// caller reads that as an empty tally.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn tally_by_category<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        visibility: &DomainVisibility,
        hidden_for: &[Uuid],
        category_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, CategoryTally>, DomainError>;
}
