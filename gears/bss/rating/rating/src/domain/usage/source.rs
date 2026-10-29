//! Canonical usage sources and their checkpoints (feature 04 §3.3; DESIGN §3.7
//! `bss_rating__source_checkpoint`, [12 §4.6]).
//!
//! Each configured feed subscription is one source: `source_id = "usage:" +
//! UUIDv5(NS_RATING_SOURCE, canonical JSON {sorted gts_type_ids, service_identity, configured
//! tenant scope})`. The same id names the configuration entry, the Cluster leadership
//! `usage-feed-{source_uuid}`, the checkpoint row, the metrics label, the source-loss rows and the
//! filter a cursor is used with. The tenant scope is configuration, never inferred from what the
//! feed returns.
//!
//! [12 §4.6]: ../../../../docs/DESIGN.md#contract-12-4-6

use std::collections::BTreeSet;
use std::fmt;

use serde_json::{Value, json};
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::authz::{SERVICE_SUBJECT_ID, SERVICE_SUBJECT_TYPE};
use crate::domain::digest::canonical_json_text;
use crate::domain::error::DomainError;
use crate::domain::usage::feed::{FeedCursor, FeedSubscription, MeterId};

/// The `UUIDv5` namespace of usage source ids.
pub const NS_RATING_SOURCE: Uuid = Uuid::from_u128(0x2b7e_4c19_6d3a_5f80_9e1c_7a2b_3c4d_5e6f);

/// The prefix of a usage source id.
pub const USAGE_SOURCE_PREFIX: &str = "usage:";

/// The most root tenants one source's scope may name.
pub const MAX_SCOPE_ROOTS: usize = 100;

/// A source's configured tenant scope: the roots of the tenant subtrees it reads, sorted and
/// deduplicated, 1 to [`MAX_SCOPE_ROOTS`].
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TenantScope(Vec<Uuid>);

impl TenantScope {
    /// The scope of `roots`.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidArgument`] on `tenant_scope` when there are no roots or more than
    /// [`MAX_SCOPE_ROOTS`], or a root is the nil UUID.
    pub fn new(roots: impl IntoIterator<Item = Uuid>) -> Result<Self, DomainError> {
        let roots: BTreeSet<Uuid> = roots.into_iter().collect();
        if roots.is_empty() || roots.len() > MAX_SCOPE_ROOTS || roots.contains(&Uuid::nil()) {
            return Err(DomainError::InvalidArgument {
                field: "tenant_scope",
                detail: format!(
                    "a tenant scope names 1 to {MAX_SCOPE_ROOTS} root tenants, none of them nil"
                ),
                reason: bss_rating_sdk::errors::reason::INVALID_REQUEST,
            });
        }
        Ok(Self(roots.into_iter().collect()))
    }

    /// The roots, sorted.
    #[must_use]
    pub fn roots(&self) -> &[Uuid] {
        &self.0
    }

    /// The tenant a source-loss row of the scope belongs to: its first root in UUID order
    /// (DESIGN §3.7 `bss_rating__source_loss`).
    #[must_use]
    pub fn owner(&self) -> Uuid {
        // A scope always holds at least one root.
        self.0.first().copied().unwrap_or_else(Uuid::nil)
    }

    /// Whether every root of `other` is a root of this scope.
    #[must_use]
    pub fn contains_all(&self, other: &Self) -> bool {
        other
            .0
            .iter()
            .all(|root| self.0.binary_search(root).is_ok())
    }
}

/// The service identity a source id names (DESIGN §4.8): Rating's subject type and id.
#[must_use]
pub fn service_identity() -> String {
    format!(
        "{SERVICE_SUBJECT_TYPE}:{}",
        SERVICE_SUBJECT_ID.as_hyphenated()
    )
}

/// One configured feed subscription: its meters and its tenant scope, read as Rating's service
/// identity.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceDefinition {
    subscription: FeedSubscription,
    tenant_scope: TenantScope,
    service_identity: String,
}

impl SourceDefinition {
    /// The source reading `subscription` for `tenant_scope` as the current service identity.
    #[must_use]
    pub fn new(subscription: FeedSubscription, tenant_scope: TenantScope) -> Self {
        Self {
            subscription,
            tenant_scope,
            service_identity: service_identity(),
        }
    }

    /// The meters it reads.
    #[must_use]
    pub const fn subscription(&self) -> &FeedSubscription {
        &self.subscription
    }

    /// Its configured tenant scope.
    #[must_use]
    pub const fn tenant_scope(&self) -> &TenantScope {
        &self.tenant_scope
    }

    /// The canonical JSON the source id is derived from, and the stored `source_definition`.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "gts_type_ids": self.subscription.meters().iter().map(MeterId::as_str).collect::<Vec<_>>(),
            "service_identity": self.service_identity,
            "tenant_scope": self.tenant_scope.roots(),
        })
    }

    /// A stored `source_definition` read back. Its service identity is the stored one, so a
    /// source stored before the identity changes (T-D-83) keeps its id and drains as the old
    /// source it is.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidArgument`] when it is not the shape [`Self::to_json`] writes.
    pub fn from_json(value: &Value) -> Result<Self, DomainError> {
        let malformed = || DomainError::InvalidArgument {
            field: "source_definition",
            detail: "not a usage source definition".to_owned(),
            reason: bss_rating_sdk::errors::reason::INVALID_REQUEST,
        };
        let meters = value
            .get("gts_type_ids")
            .and_then(Value::as_array)
            .ok_or_else(malformed)?
            .iter()
            .map(|meter| {
                meter
                    .as_str()
                    .ok_or_else(malformed)
                    .and_then(MeterId::parse)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let roots = value
            .get("tenant_scope")
            .and_then(Value::as_array)
            .ok_or_else(malformed)?
            .iter()
            .map(|root| {
                root.as_str()
                    .and_then(|text| Uuid::parse_str(text).ok())
                    .ok_or_else(malformed)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let service_identity = value
            .get("service_identity")
            .and_then(Value::as_str)
            .filter(|identity| !identity.is_empty())
            .ok_or_else(malformed)?
            .to_owned();
        Ok(Self {
            subscription: FeedSubscription::new(meters)?,
            tenant_scope: TenantScope::new(roots)?,
            service_identity,
        })
    }

    /// The canonical source id.
    #[must_use]
    pub fn source_id(&self) -> SourceId {
        // @cpt-begin:cpt-cf-bss-rating-algo-intake-source-identity:p1:inst-src-id
        let name = canonical_json_text(&self.to_json());
        SourceId(Uuid::new_v5(&NS_RATING_SOURCE, name.as_bytes()))
        // @cpt-end:cpt-cf-bss-rating-algo-intake-source-identity:p1:inst-src-id
    }
}

/// A usage source id, `usage:{uuid}`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceId(Uuid);

impl SourceId {
    /// The source id `usage:{uuid}` names.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        text.strip_prefix(USAGE_SOURCE_PREFIX)
            .and_then(|uuid| Uuid::parse_str(uuid).ok())
            .map(Self)
    }

    /// The UUID: the leaf of the Cluster leadership `usage-feed-{uuid}`.
    #[must_use]
    pub const fn uuid(self) -> Uuid {
        self.0
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{USAGE_SOURCE_PREFIX}{}", self.0.as_hyphenated())
    }
}

/// The lifecycle of a source (DESIGN §3.7).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceState {
    /// Configured and read.
    Active,
    /// No longer configured: read to its current end, then retired.
    Draining,
    /// Not read.
    Retired,
}

impl SourceState {
    /// The stored name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Draining => "draining",
            Self::Retired => "retired",
        }
    }

    /// The state a stored name denotes.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        [Self::Active, Self::Draining, Self::Retired]
            .into_iter()
            .find(|state| state.as_str() == name)
    }
}

/// A conditional move between two source states.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Transition {
    /// The state the source must be in.
    pub from: SourceState,
    /// The state it moves to.
    pub to: SourceState,
}

impl Transition {
    /// A draining source read to its end.
    pub const RETIRE_DRAINED: Self = Self {
        from: SourceState::Draining,
        to: SourceState::Retired,
    };
}

/// A source's checkpoint row.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    /// The source.
    pub source_id: SourceId,
    /// What it reads.
    pub definition: SourceDefinition,
    /// The cursor after the last committed page; `None` reads from `Oldest`.
    pub cursor: Option<FeedCursor>,
    /// Its lifecycle state.
    pub state: SourceState,
    /// When the cursor (or the state) was last written.
    pub updated_at: OffsetDateTime,
    /// The configuration generation the usage sources were last reconciled under
    /// (`usage_feed.config_generation`); an older configuration does not reconcile.
    pub config_generation: u64,
}

/// What reconciling the stored sources with the configuration does (feature 04 §3.3
/// `inst-src-change`, `inst-src-widened`).
#[domain_model]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourcePlan {
    /// Configured sources with no row: created `active`, reading from `Oldest`.
    pub create: Vec<SourceDefinition>,
    /// Configured sources stored `draining` or `retired`: made `active` again, cursor kept.
    pub activate: Vec<SourceId>,
    /// Active sources no longer configured: read to their end, then retired.
    pub drain: Vec<SourceId>,
    /// Active sources whose scope was narrowed on purpose: retired without draining.
    pub retire: Vec<SourceId>,
    /// The tenants a created source exposes that the source with its meters did not cover: one
    /// `scope_widened_beyond_retention` loss on the created source, against those tenants.
    pub widened: Vec<(SourceId, TenantScope)>,
}

impl SourcePlan {
    /// Whether it changes nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.create.is_empty()
            && self.activate.is_empty()
            && self.drain.is_empty()
            && self.retire.is_empty()
    }
}

/// Plans the reconciliation of `stored` usage sources with `configured` ones. Pure: the caller
/// applies the plan in one transaction.
#[must_use]
pub fn plan_sources(configured: &[SourceDefinition], stored: &[Checkpoint]) -> SourcePlan {
    // @cpt-begin:cpt-cf-bss-rating-algo-intake-source-identity:p1:inst-src-change
    let mut plan = SourcePlan::default();
    let configured_ids: BTreeSet<SourceId> =
        configured.iter().map(SourceDefinition::source_id).collect();
    for definition in configured {
        let id = definition.source_id();
        match stored.iter().find(|row| row.source_id == id) {
            None => {
                if let Ok(exposed) = TenantScope::new(widened_tenants(definition, stored)) {
                    plan.widened.push((id, exposed));
                }
                plan.create.push(definition.clone());
            }
            Some(row) if row.state != SourceState::Active => plan.activate.push(id),
            Some(_) => {}
        }
    }
    for row in stored {
        if row.state != SourceState::Active || configured_ids.contains(&row.source_id) {
            continue;
        }
        let narrowed = configured.iter().any(|definition| {
            definition.subscription() == row.definition.subscription()
                && row
                    .definition
                    .tenant_scope()
                    .contains_all(definition.tenant_scope())
                && definition.tenant_scope() != row.definition.tenant_scope()
        });
        if narrowed {
            plan.retire.push(row.source_id);
        } else {
            plan.drain.push(row.source_id);
        }
    }
    plan
    // @cpt-end:cpt-cf-bss-rating-algo-intake-source-identity:p1:inst-src-change
}

/// The roots `definition` adds to the not-retired stored sources with the same meters, or none
/// when no such source exists (a source with a new meter set starts from `Oldest` anyway).
fn widened_tenants(definition: &SourceDefinition, stored: &[Checkpoint]) -> Vec<Uuid> {
    // @cpt-begin:cpt-cf-bss-rating-algo-intake-source-identity:p1:inst-src-widened
    let previous: Vec<&Checkpoint> = stored
        .iter()
        .filter(|row| {
            row.state != SourceState::Retired
                && row.definition.subscription() == definition.subscription()
        })
        .collect();
    if previous.is_empty() {
        return Vec::new();
    }
    definition
        .tenant_scope()
        .roots()
        .iter()
        .filter(|root| {
            !previous
                .iter()
                .any(|row| row.definition.tenant_scope().roots().contains(root))
        })
        .copied()
        .collect()
    // @cpt-end:cpt-cf-bss-rating-algo-intake-source-identity:p1:inst-src-widened
}

#[cfg(test)]
#[path = "source_tests.rs"]
mod tests;
