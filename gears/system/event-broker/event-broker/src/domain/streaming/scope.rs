//! The consume scope a PDP grants, compiled once and checked per event.
//!
//! The PDP answers a consume question with an `AccessScope`: an OR of
//! constraints, each an AND of filters. The answer is decided once per event,
//! so it is compiled here into a form that is checked with no await and no
//! allocation. A constraint this build cannot enforce is dropped whole, never
//! one filter of it: dropping a disjunct narrows the grant, while skipping a
//! filter would widen it.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use authz_resolver_sdk::TenantMode;
use gts::{GtsId, GtsIdPattern, GtsTypeId};
use toolkit::domain_model;
use toolkit_security::{AccessScope, ScopeFilter, ScopeValue, pep_properties};
use uuid::Uuid;

use crate::domain::model::Event;

/// The property a consume scope uses to name the event's type.
pub const EVENT_TYPE: &str = "event_type";

/// The request property a consume question lists its candidate event types
/// in. A property of the request only: no scope filters on it.
pub(crate) const EVENT_TYPES: &str = "event_types";

/// The property a consume scope uses to name the event's subject type.
pub const EVENT_SUBJECT_TYPE: &str = "event_subject_type";

/// The properties a consume scope may filter on, and so the
/// `supported_properties` a consume PDP call advertises: the event's subject
/// tenant, its subject owner, its type and its subject type.
pub const CONSUME_PROPS: [&str; 4] = [
    pep_properties::OWNER_TENANT_ID,
    pep_properties::OWNER_ID,
    EVENT_TYPE,
    EVENT_SUBJECT_TYPE,
];

/// A consume scope ready to decide events: an OR of compiled constraints.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledScope {
    any_of: Vec<CompiledConstraint>,
}

/// One kept constraint: the tenant check AND every predicate in `rest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompiledConstraint {
    tenant: TenantCheck,
    rest: Box<[EventPredicate]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TenantCheck {
    One(Uuid),
    Set(Arc<HashSet<Uuid>>),
    /// Only in root-only groups: in a subtree group every kept constraint must
    /// name the tenant.
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::enum_variant_names,
    reason = "each variant is the `in` over one event field, named after that field"
)]
enum EventPredicate {
    TypeIn(HashSet<GtsTypeId>),
    /// Ids only, never parsed ones: a parsed id costs several times its text
    /// in every cached scope, so the JOIN post-check parses what it needs.
    SubjectTypeIn(HashSet<GtsTypeId>),
    /// Holds for an event whose `subject_owner` is in the set; an event with
    /// no owner never matches.
    OwnerIn(HashSet<Uuid>),
}

/// What delivery does with the events of one type under one group's scope,
/// from [`CompiledScope::plan_for`] or from a single-type evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TypePlan {
    /// No constraint can admit an event of the type: each is skipped.
    Deny,
    /// The constraints that can admit an event of the type; an event is
    /// delivered when one of them admits it.
    Admit(Arc<[CompiledConstraint]>),
}

/// The subject types each kept constraint can deliver to a JOIN, from
/// [`CompiledScope::admitted_subject_types`].
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedSubjectTypes {
    any_of: Vec<SubjectTypesOf>,
}

/// What one kept constraint admits by subject type.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SubjectTypesOf {
    /// The constraint has no subject-type filter.
    Any,
    /// The types every subject-type filter of the constraint names and the
    /// candidates allow, in id order.
    Only(Box<[GtsId]>),
}

/// The event field a [`CONSUME_PROPS`] property filters on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeField {
    Tenant,
    Owner,
    Type,
    SubjectType,
}

/// Why `compile` dropped one constraint. Value-free: no tenant id, type id or
/// property value.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// A filter other than `eq` or `in`.
    UnsupportedFilter,
    /// A filter on a property outside [`CONSUME_PROPS`].
    UnknownProperty,
    /// A tenant or owner value that is not a UUID, or a type value that is
    /// not an exact GTS type id.
    InvalidValue,
    /// An empty `in` list, or tenant filters with no tenant in common.
    MatchesNothing,
    /// A subtree constraint that names no tenant.
    NoTenantFilter,
}

/// No constraint of a consume scope survived `compile`.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no constraint of the consume scope can be enforced")]
pub struct ScopeNotEnforceable {
    /// One reason per dropped constraint, in the scope's order.
    pub dropped: Vec<DropReason>,
}

/// Shares one allocation between equal tenant sets.
///
/// A subtree answer can hold 100,000 tenants (about 2.2 MB), and every group
/// of the same tenant context gets the same set, so each distinct live set is
/// kept once. The map holds only `Weak`s: a set lives as long as some compiled
/// scope holds it, and every `intern` first prunes the entries nobody holds,
/// so the map is bounded by the live sets.
///
/// The lock is a `std::sync::Mutex`, held for a prune, a lookup and a push,
/// never across an await. It recovers from poison: no critical section can
/// leave the map invalid (see `sets`).
#[domain_model]
#[derive(Debug, Default)]
pub struct TenantSetInterner {
    sets: Mutex<Buckets>,
}

/// The interner's map: a key over the set's members to the sets under it.
type Buckets = HashMap<u64, Vec<Weak<HashSet<Uuid>>>>;

impl CompiledScope {
    /// Compiles a PDP consume answer for one tenant context.
    ///
    /// Drops (OR: dropping narrows) every constraint holding a filter that is
    /// not `eq`/`in` on a [`CONSUME_PROPS`] property with valid values, every
    /// constraint that matches nothing, and, under [`TenantMode::Subtree`],
    /// every constraint without a tenant filter. An allow-all answer is one
    /// constraint with no filter.
    ///
    /// # Errors
    ///
    /// [`ScopeNotEnforceable`] when no constraint remains, with one reason per
    /// dropped constraint.
    pub fn compile(
        scope: &AccessScope,
        mode: &TenantMode,
        interner: &TenantSetInterner,
    ) -> Result<Self, ScopeNotEnforceable> {
        if scope.is_unconstrained() {
            return CompiledConstraint::compile(&[], mode, interner)
                .map(|kept| Self { any_of: vec![kept] })
                .map_err(|reason| ScopeNotEnforceable {
                    dropped: vec![reason],
                });
        }
        let mut any_of = Vec::new();
        let mut dropped = Vec::new();
        for constraint in scope.constraints() {
            match CompiledConstraint::compile(constraint.filters(), mode, interner) {
                Ok(kept) => any_of.push(kept),
                Err(reason) => dropped.push(reason),
            }
        }
        if any_of.is_empty() {
            return Err(ScopeNotEnforceable { dropped });
        }
        Ok(Self { any_of })
    }

    /// Whether the scope lets the consumer read `event`.
    #[must_use]
    pub fn admits(&self, event: &Event) -> bool {
        self.any_of.iter().any(|kept| kept.admits(event))
    }

    /// Whether some kept constraint admits events of type `event_type`,
    /// whatever their tenant: the tenant reach is the PDP's decision on the
    /// request, not this check's. A constraint with no type filter admits
    /// every type. The type is a concrete registered id, so it is looked up
    /// by exact id.
    #[must_use]
    pub fn admits_event_type(&self, event_type: &GtsTypeId) -> bool {
        self.any_of.iter().any(|kept| {
            kept.rest.iter().all(|predicate| match predicate {
                EventPredicate::TypeIn(types) => types.contains(event_type),
                EventPredicate::SubjectTypeIn(_) | EventPredicate::OwnerIn(_) => true,
            })
        })
    }

    /// The plan for events of type `event_type`: the kept constraints whose
    /// type filters all name it, a constraint with no type filter included.
    /// [`TypePlan::Deny`] when none is left.
    #[must_use]
    pub(crate) fn plan_for(&self, event_type: &GtsTypeId) -> TypePlan {
        let kept: Arc<[CompiledConstraint]> = self
            .any_of
            .iter()
            .filter(|kept| {
                kept.rest.iter().all(|predicate| match predicate {
                    EventPredicate::TypeIn(types) => types.contains(event_type),
                    EventPredicate::SubjectTypeIn(_) | EventPredicate::OwnerIn(_) => true,
                })
            })
            .cloned()
            .collect();
        if kept.is_empty() {
            TypePlan::Deny
        } else {
            TypePlan::Admit(kept)
        }
    }

    /// The subject types each kept constraint can deliver, whatever the
    /// tenant, among those `allowed` (the patterns the candidates allow), for
    /// [`AdmittedSubjectTypes::admits_under`] to answer per selector.
    ///
    /// A constraint with no subject-type filter admits any subject type.
    /// Otherwise it admits a type T when T is named by every subject-type
    /// filter of the constraint, as delivery requires, and T matches some
    /// allowed pattern, as publish requires. Only the ids every filter names
    /// are parsed, on each call: the scope keeps none parsed.
    #[must_use]
    pub fn admitted_subject_types(&self, allowed: &[&GtsIdPattern]) -> AdmittedSubjectTypes {
        AdmittedSubjectTypes {
            any_of: self
                .any_of
                .iter()
                .map(|kept| kept.admitted_subject_types(allowed))
                .collect(),
        }
    }
}

impl AdmittedSubjectTypes {
    /// Whether some kept constraint admits a subject type `selector` matches
    /// as delivery does: the type itself, a type derived from it, or, when
    /// `selector` names no minor version, any minor version of it.
    #[must_use]
    pub fn admits_under(&self, selector: &GtsIdPattern) -> bool {
        self.any_of.iter().any(|kept| match kept {
            SubjectTypesOf::Any => true,
            SubjectTypesOf::Only(types) => types
                .iter()
                .any(|subject_type| subject_type.matches_pattern(selector)),
        })
    }
}

impl TypePlan {
    /// Whether the plan lets the consumer read `event`, an event of the
    /// type it was made for. No allocation and no await.
    #[must_use]
    pub(crate) fn admits(&self, event: &Event) -> bool {
        match self {
            Self::Deny => false,
            Self::Admit(kept) => kept.iter().any(|kept| kept.admits(event)),
        }
    }
}

impl CompiledConstraint {
    /// The first filter that cannot be enforced names the reason.
    fn compile(
        filters: &[ScopeFilter],
        mode: &TenantMode,
        interner: &TenantSetInterner,
    ) -> Result<Self, DropReason> {
        let mut tenants: Option<HashSet<Uuid>> = None;
        let mut rest = Vec::new();
        for filter in filters {
            let values = match filter {
                ScopeFilter::Eq(eq) => std::slice::from_ref(eq.value()),
                ScopeFilter::In(r#in) => r#in.values(),
                // `InGroup` and `InGroupSubtree` need a membership lookup the
                // broker does not have. `InTenantSubtree` needs the event's
                // ancestor chain, which the broker does not read yet. A
                // variant this build does not know is a restriction it cannot
                // apply.
                _ => return Err(DropReason::UnsupportedFilter),
            };
            let field = ScopeField::of(filter.property())?;
            if values.is_empty() {
                return Err(DropReason::MatchesNothing);
            }
            match field {
                ScopeField::Tenant => {
                    let named = TenantCheck::read(values)?;
                    match &mut tenants {
                        Some(common) => common.retain(|tenant| named.contains(tenant)),
                        None => tenants = Some(named),
                    }
                }
                ScopeField::Owner => rest.push(EventPredicate::owner_in(values)?),
                ScopeField::Type => rest.push(EventPredicate::type_in(values)?),
                ScopeField::SubjectType => rest.push(EventPredicate::subject_type_in(values)?),
            }
        }
        let tenant = match tenants {
            Some(common) => TenantCheck::compile(common, interner)?,
            None => match mode {
                TenantMode::Subtree => return Err(DropReason::NoTenantFilter),
                TenantMode::RootOnly => TenantCheck::Any,
            },
        };
        Ok(Self {
            tenant,
            rest: rest.into_boxed_slice(),
        })
    }

    fn admits(&self, event: &Event) -> bool {
        self.tenant.holds(event) && self.rest.iter().all(|predicate| predicate.holds(event))
    }

    /// The constraint's part of [`CompiledScope::admitted_subject_types`].
    ///
    /// The parse cannot fail, since `EventPredicate::read` admitted each id
    /// only once it parsed as a type id, so the skip of an id that does not
    /// parse has no test. Were it reached, leaving the id out could only
    /// refuse a JOIN, never widen delivery: the post-check grants nothing.
    fn admitted_subject_types(&self, allowed: &[&GtsIdPattern]) -> SubjectTypesOf {
        let filters: Vec<&HashSet<GtsTypeId>> = self
            .rest
            .iter()
            .filter_map(EventPredicate::subject_types)
            .collect();
        let [first, others @ ..] = filters.as_slice() else {
            return SubjectTypesOf::Any;
        };
        let mut admitted: Vec<GtsId> = first
            .iter()
            .filter(|id| others.iter().all(|other| other.contains(*id)))
            .filter_map(|id| GtsId::try_new(id.as_ref()).ok())
            .filter(|subject_type| {
                allowed
                    .iter()
                    .any(|pattern| subject_type.matches_pattern(pattern))
            })
            .collect();
        admitted.sort_unstable_by(|left, right| left.id().cmp(right.id()));
        SubjectTypesOf::Only(admitted.into_boxed_slice())
    }
}

impl ScopeField {
    /// Any property outside [`CONSUME_PROPS`] names a field this build
    /// cannot check, so its constraint is dropped.
    fn of(property: &str) -> Result<Self, DropReason> {
        match property {
            pep_properties::OWNER_TENANT_ID => Ok(Self::Tenant),
            pep_properties::OWNER_ID => Ok(Self::Owner),
            EVENT_TYPE => Ok(Self::Type),
            EVENT_SUBJECT_TYPE => Ok(Self::SubjectType),
            _ => Err(DropReason::UnknownProperty),
        }
    }
}

impl TenantCheck {
    fn read(values: &[ScopeValue]) -> Result<HashSet<Uuid>, DropReason> {
        values
            .iter()
            .map(|value| value.as_uuid().ok_or(DropReason::InvalidValue))
            .collect()
    }

    fn compile(tenants: HashSet<Uuid>, interner: &TenantSetInterner) -> Result<Self, DropReason> {
        let mut named = tenants.iter();
        match (named.next(), named.next()) {
            (None, _) => Err(DropReason::MatchesNothing),
            (Some(&tenant), None) => Ok(Self::One(tenant)),
            _ => Ok(Self::Set(interner.intern(tenants))),
        }
    }

    fn holds(&self, event: &Event) -> bool {
        match self {
            Self::One(tenant) => *tenant == event.subject_tenant,
            Self::Set(tenants) => tenants.contains(&event.subject_tenant),
            Self::Any => true,
        }
    }
}

impl EventPredicate {
    fn owner_in(values: &[ScopeValue]) -> Result<Self, DropReason> {
        Ok(Self::OwnerIn(TenantCheck::read(values)?))
    }

    fn type_in(values: &[ScopeValue]) -> Result<Self, DropReason> {
        Ok(Self::TypeIn(
            Self::read(values)?.iter().map(Self::type_id).collect(),
        ))
    }

    fn subject_type_in(values: &[ScopeValue]) -> Result<Self, DropReason> {
        Ok(Self::SubjectTypeIn(
            Self::read(values)?.iter().map(Self::type_id).collect(),
        ))
    }

    /// Exact type ids only: a pattern is never matched as a pattern.
    fn read(values: &[ScopeValue]) -> Result<Vec<GtsId>, DropReason> {
        values
            .iter()
            .map(|value| match value {
                ScopeValue::String(raw) => GtsId::try_new(raw)
                    .ok()
                    .filter(GtsId::is_type)
                    .ok_or(DropReason::InvalidValue),
                _ => Err(DropReason::InvalidValue),
            })
            .collect()
    }

    /// The id `GtsTypeId::try_new` would build: it is the same parse and the
    /// same type check, which `read` has made.
    fn type_id(id: &GtsId) -> GtsTypeId {
        GtsTypeId::new(id.id())
    }

    fn subject_types(&self) -> Option<&HashSet<GtsTypeId>> {
        match self {
            Self::TypeIn(_) | Self::OwnerIn(_) => None,
            Self::SubjectTypeIn(ids) => Some(ids),
        }
    }

    fn holds(&self, event: &Event) -> bool {
        match self {
            Self::TypeIn(types) => types.contains(&event.r#type),
            Self::SubjectTypeIn(ids) => ids.contains(&event.subject_type),
            Self::OwnerIn(owners) => event
                .subject_owner
                .is_some_and(|owner| owners.contains(&owner)),
        }
    }
}

impl TenantSetInterner {
    /// An interner that holds no set yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The shared allocation of `set`: an earlier one equal to it while some
    /// scope still holds it, otherwise `set` itself, now shared.
    pub fn intern(&self, set: HashSet<Uuid>) -> Arc<HashSet<Uuid>> {
        let key = set
            .iter()
            .map(|tenant| {
                let mut hasher = DefaultHasher::new();
                tenant.hash(&mut hasher);
                hasher.finish()
            })
            .fold(
                u64::try_from(set.len()).unwrap_or(u64::MAX),
                u64::wrapping_add,
            );
        self.intern_keyed(key, set)
    }

    /// `key` only picks the bucket; a bucket compares whole sets, so a key
    /// collision costs a scan, never a wrong set.
    fn intern_keyed(&self, key: u64, set: HashSet<Uuid>) -> Arc<HashSet<Uuid>> {
        let mut sets = self.sets();
        sets.retain(|_, bucket| {
            bucket.retain(|held| held.strong_count() > 0);
            !bucket.is_empty()
        });
        let bucket = sets.entry(key).or_default();
        if let Some(shared) = bucket
            .iter()
            .filter_map(Weak::upgrade)
            .find(|held| **held == set)
        {
            return shared;
        }
        let shared = Arc::new(set);
        bucket.push(Arc::downgrade(&shared));
        shared
    }

    /// Recovers a poisoned guard rather than refusing every later compile.
    ///
    /// A panic inside a critical section here (a prune, a lookup, a push)
    /// leaves the map valid: at worst a dead `Weak` or a missing entry, which
    /// costs one duplicate set. Refusing instead would be sticky and stop every
    /// later scope from compiling for the life of the process.
    fn sets(&self) -> MutexGuard<'_, Buckets> {
        self.sets.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
#[path = "scope_tests.rs"]
mod scope_tests;
