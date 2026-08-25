//! The published read model's subject vocabulary.
//!
//! A version's rows are **per-subject deltas** and never a full tenant copy
//! (D-86, subject-typed by D-91): a version contains exactly the subjects of
//! the publish units that produced it. The reason is arithmetic, not taste —
//! interactive publishes coalesce on a 5-second SLO, and a full tenant copy per
//! version would multiply a tenant's whole catalog by its publish rate. It is
//! also what makes an old pin resolvable: resolving `(pin, subject)` reads that
//! subject's greatest completed version at or below the pin, so a version that
//! touched nothing of a subject costs nothing and hides nothing.
//!
//! The per-delta *size* is bounded separately by the D-121 projected-set
//! horizon, so a plan delta carries the rows and windows intersecting the
//! horizon rather than the plan's whole accumulated history.

use std::fmt;

use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// The scope-class sentinel of the classless `overlay_index` shard.
pub const GLOBAL_SCOPE: &str = "global";

/// What a read-model row is about.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SubjectKind {
    /// Published by a **plan publish unit**, which projects one plan-subject
    /// row (`subject_ref = plan_id`) from the plan's current revision — its
    /// truth rows, its revision-scoped children and its published price rows.
    /// Retirement is one of these units (D-128), which is why the projector
    /// sources the current revision rather than the published one.
    Plan,
    /// Published by a **`PriceOverlay` publish unit**, which projects one
    /// overlay-subject row: the overlay document (lines, amounts, dating,
    /// disclosure, lifecycle). It **never re-projects the targeted plans** —
    /// Tariffs joins overlays to base rows at evaluation, so a `global`-scope
    /// overlay commit writes one row rather than a tenant's worth.
    PriceOverlay,
    /// Published **additionally** by every overlay commit: the sharded live
    /// overlay set with each overlay's interval and precedence. Per-subject
    /// resolution answers "overlay X at pin V", but evaluation needs the
    /// *set*, and without an index the only path was a `DISTINCT subject_ref`
    /// scan over years of overlay deltas on the order-time read path (D-112).
    /// Sharded and horizon-bounded by D-133.
    OverlayIndex,
    /// Published by a **membership publish unit**, which projects one
    /// membership-subject row **per payer record** — so customer-group
    /// enrollment has a read-model representation of its own rather than
    /// riding a plan's.
    GroupMembership,
}

impl SubjectKind {
    /// Every subject kind, stable order.
    pub const ALL: &'static [Self] = &[
        Self::Plan,
        Self::PriceOverlay,
        Self::OverlayIndex,
        Self::GroupMembership,
    ];

    /// The persisted `subject_kind` token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::PriceOverlay => "price_overlay",
            Self::OverlayIndex => "overlay_index",
            Self::GroupMembership => "group_membership",
        }
    }
}

// There is no `SubjectKind::parse` here. It was
// documented as the reader of a "persisted `subject_kind` token" and had no
// non-test caller: both stored-token reads — `catalog_version_ref_repo::to_domain`
// and `read_model_repo`'s warm-subject list — go through `infra`'s `read_token`,
// which raises `RepoError::CorruptRow`. That is the right class and this was not:
// `DomainError::InvalidRequest` is a **caller-fault** class, so a corrupt stored
// row read through this would have reached the caller as a 4xx blaming their
// request. Its two tests read as a guard over the projector's read path and
// guarded nothing that runs. Giving the store's readers this function instead
// would have moved the wrong error class onto a live path, so the vocabulary
// keeps one parse and it is `read_token`'s.

impl fmt::Display for SubjectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The **valued** overlay scope classes.
///
/// `global` is deliberately absent: it is the *classless* scope, and it is
/// modelled as [`OverlayIndexShard::Global`] instead. That way a shard carrying
/// class `global` together with a scope value is not a state this type can
/// reach, and the six-value persisted enum is still covered — see
/// [`OverlayIndexShard::scope_class`].
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OverlayScopeClass {
    /// A reseller / partner scope.
    Partner,
    /// An organization-tier scope.
    OrgTier,
    /// A brand scope.
    Brand,
    /// A pricing-region scope.
    Region,
    /// A customer-group scope.
    CustomerGroup,
}

impl OverlayScopeClass {
    /// Every valued class, stable order.
    pub const ALL: &'static [Self] = &[
        Self::Partner,
        Self::OrgTier,
        Self::Brand,
        Self::Region,
        Self::CustomerGroup,
    ];

    /// The persisted `scope_class` token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Partner => "partner",
            Self::OrgTier => "org_tier",
            Self::Brand => "brand",
            Self::Region => "region",
            Self::CustomerGroup => "customer_group",
        }
    }
}

impl fmt::Display for OverlayScopeClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The shard key of an `overlay_index` subject: `(scope_class, scope_value)`
/// (D-133).
///
/// The index was one tenant-wide document rewritten whole on every commit and
/// retained on the seven-year truth horizon — O(commits x overlays) of storage
/// and O(overlays) of write amplification, on the object the order-time read
/// path touches. Sharded, a commit rewrites exactly one shard (two when a
/// revision moves the scope value), and an evaluation reads the at most six
/// shards a payer context can match as point lookups.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OverlayIndexShard {
    /// The classless shard, keyed by the [`GLOBAL_SCOPE`] sentinel.
    Global,
    /// A valued shard.
    Scoped {
        /// The scope class.
        class: OverlayScopeClass,
        /// The scope value, validated against its class universe by Slice 4 /
        /// Slice 9, not here.
        value: String,
    },
}

impl OverlayIndexShard {
    /// A valued shard.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidRequest`] when `value` is blank — a valued class
    /// with no value is not the global shard, it is a shard key that matches
    /// nothing.
    pub fn scoped(class: OverlayScopeClass, value: &str) -> Result<Self, DomainError> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(DomainError::InvalidRequest(format!(
                "overlay index shard of class {class} needs a scope value"
            )));
        }
        Ok(Self::Scoped {
            class,
            value: trimmed.to_owned(),
        })
    }

    /// Read a shard back from its persisted rendering.
    ///
    /// The inverse of [`Display`](std::fmt::Display), and it has to exist: the
    /// rendering **is** `subject_ref` on a ref row and on a delta row, so the
    /// projector reads a shard back from a string it wrote. A rendering with no
    /// parse would make the write and read sides of one column two conventions.
    ///
    /// **Split once, on the first separator.** A scope value is
    /// operator-authored and may contain a `/` — `partner/ac/me` is a reachable
    /// key — and a naive split would read it as class `partner`, value `ac`: a
    /// *different* shard, silently, filing the overlay where nothing reads it.
    /// The class never contains a separator, so the first one is the boundary.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidRequest`] for a key with no separator, an unknown
    /// class, a blank value, or the `global` sentinel carrying a value — the last
    /// because the classless shard is a variant rather than a class, so
    /// `global/acme` names nothing this type can be.
    pub fn parse(raw: &str) -> Result<Self, DomainError> {
        let unknown = || {
            DomainError::InvalidRequest(format!(
                "{raw} is not an overlay index shard key: expected `<scope class>/<value>` or \
                 `{GLOBAL_SCOPE}/{GLOBAL_SCOPE}`"
            ))
        };
        let (class, value) = raw.split_once('/').ok_or_else(unknown)?;
        if class == GLOBAL_SCOPE {
            // The sentinel is repeated on both halves by construction, so a
            // `global` class with anything else is not a shard this type has.
            return (value == GLOBAL_SCOPE)
                .then_some(Self::Global)
                .ok_or_else(unknown);
        }
        let class = OverlayScopeClass::ALL
            .iter()
            .copied()
            .find(|candidate| candidate.as_str() == class)
            .ok_or_else(unknown)?;
        Self::scoped(class, value)
    }

    /// The persisted `scope_class` component, including the `global` sentinel.
    #[must_use]
    pub const fn scope_class(&self) -> &'static str {
        match self {
            Self::Global => GLOBAL_SCOPE,
            Self::Scoped { class, .. } => class.as_str(),
        }
    }

    /// The persisted `scope_value` component. The classless shard repeats the
    /// sentinel rather than carrying an empty component: the pair is a
    /// composite key, and an empty half would make the classless shard's
    /// identity depend on a null-versus-empty convention.
    #[must_use]
    pub fn scope_value(&self) -> &str {
        match self {
            Self::Global => GLOBAL_SCOPE,
            Self::Scoped { value, .. } => value,
        }
    }
}

impl TryFrom<&crate::domain::overlay::ScopeSelector> for OverlayIndexShard {
    type Error = DomainError;

    /// The shard an overlay's authored scope files it under (D-112, D-133).
    ///
    /// **Where the two spellings of one concept meet.**
    /// [`ScopeSelector`](crate::domain::overlay::ScopeSelector) is the authoring
    /// vocabulary and its class carries a `Global` variant; this type is the
    /// shard vocabulary and does not, because the classless scope is a variant
    /// of the *shard* rather than a class with no value. Every projection of an
    /// overlay passes through here, so a class added to one enum and not the
    /// other stops compiling at this `match` instead of silently filing
    /// overlays under a shard nothing reads.
    ///
    /// **Fallible for one input no path can stage**, and written rather than
    /// assumed away for the reason `api::rest::publish::authorization_of` gives
    /// for its own unreachable refusal: `ScopeSelector::scoped` refuses a
    /// `Global` class with a value, and the store's biconditional
    /// `chk_pricing_price_overlay_scope_value` refuses it physically — but the
    /// variant's fields are constructible directly, and both other answers are
    /// wrong. Coercing to [`Self::Global`] drops the value and files a scoped
    /// overlay where **every** payer context matches it; panicking puts an
    /// `unreachable!()` on a value a route can hold.
    ///
    /// # Errors
    /// [`DomainError::InvalidRequest`] for a classless class carrying a value —
    /// an invariant breach rather than anything a caller did.
    fn try_from(selector: &crate::domain::overlay::ScopeSelector) -> Result<Self, Self::Error> {
        use crate::domain::overlay::ScopeClass;

        let Some(value) = selector.value() else {
            return Ok(Self::Global);
        };
        // No rest pattern: a seventh authoring class does not compile until it
        // has said which shard it belongs to.
        let class = match selector.class() {
            ScopeClass::Region => OverlayScopeClass::Region,
            ScopeClass::Brand => OverlayScopeClass::Brand,
            ScopeClass::OrgTier => OverlayScopeClass::OrgTier,
            ScopeClass::Partner => OverlayScopeClass::Partner,
            ScopeClass::CustomerGroup => OverlayScopeClass::CustomerGroup,
            ScopeClass::Global => {
                return Err(DomainError::InvalidRequest(format!(
                    "overlay scope is classless and carries the value {value}; the classless \
                     scope has no value, and filing this under the global shard would make \
                     every payer context match it"
                )));
            }
        };
        Self::scoped(class, value.as_str())
    }
}

impl fmt::Display for OverlayIndexShard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.scope_class(), self.scope_value())
    }
}

/// The identifier of one read-model subject.
///
/// Modelled as an enum so the shard key is **inseparable** from an
/// `overlay_index` reference: there is no constructor that yields an
/// `OverlayIndex` subject without one, so the unsharded tenant-wide index
/// D-133 removed is not expressible.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SubjectRef {
    /// A plan subject, keyed by `plan_id`.
    Plan(Uuid),
    /// An overlay subject, keyed by `price_overlay_id`.
    PriceOverlay(Uuid),
    /// An overlay-index shard.
    OverlayIndex(OverlayIndexShard),
    /// A membership subject, keyed by the payer membership record id.
    GroupMembership(Uuid),
}

impl SubjectRef {
    /// The subject kind this reference belongs to.
    #[must_use]
    pub const fn kind(&self) -> SubjectKind {
        match self {
            Self::Plan(_) => SubjectKind::Plan,
            Self::PriceOverlay(_) => SubjectKind::PriceOverlay,
            Self::OverlayIndex(_) => SubjectKind::OverlayIndex,
            Self::GroupMembership(_) => SubjectKind::GroupMembership,
        }
    }
}

impl fmt::Display for SubjectRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plan(id) | Self::PriceOverlay(id) | Self::GroupMembership(id) => {
                write!(f, "{id}")
            }
            Self::OverlayIndex(shard) => write!(f, "{shard}"),
        }
    }
}

#[cfg(test)]
#[path = "read_model_tests.rs"]
mod read_model_tests;
