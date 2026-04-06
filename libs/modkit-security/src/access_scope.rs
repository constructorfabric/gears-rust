use std::fmt;
use uuid::Uuid;

/// A scalar value for scope filtering.
///
/// Used in [`ScopeFilter`] predicates to represent typed values.
/// JSON conversion happens at the PDP/PEP boundary (see the PEP compiler),
/// not inside the security model.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ScopeValue {
    /// UUID value (tenant IDs, resource IDs, etc.)
    Uuid(Uuid),
    /// String value (status, GTS type IDs, etc.)
    String(String),
    /// Integer value.
    Int(i64),
    /// Boolean value.
    Bool(bool),
}

impl ScopeValue {
    /// Try to extract a UUID from this value.
    ///
    /// Returns `Some` for `ScopeValue::Uuid` directly, and for
    /// `ScopeValue::String` if the string is a valid UUID.
    #[must_use]
    pub fn as_uuid(&self) -> Option<Uuid> {
        match self {
            Self::Uuid(u) => Some(*u),
            Self::String(s) => Uuid::parse_str(s).ok(),
            Self::Int(_) | Self::Bool(_) => None,
        }
    }
}

impl fmt::Display for ScopeValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Uuid(u) => write!(f, "{u}"),
            Self::String(s) => write!(f, "{s}"),
            Self::Int(n) => write!(f, "{n}"),
            Self::Bool(b) => write!(f, "{b}"),
        }
    }
}

impl From<Uuid> for ScopeValue {
    #[inline]
    fn from(u: Uuid) -> Self {
        Self::Uuid(u)
    }
}

impl From<&Uuid> for ScopeValue {
    #[inline]
    fn from(u: &Uuid) -> Self {
        Self::Uuid(*u)
    }
}

impl From<String> for ScopeValue {
    #[inline]
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

impl From<&str> for ScopeValue {
    #[inline]
    fn from(s: &str) -> Self {
        Self::String(s.to_owned())
    }
}

impl From<i64> for ScopeValue {
    #[inline]
    fn from(n: i64) -> Self {
        Self::Int(n)
    }
}

impl From<bool> for ScopeValue {
    #[inline]
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

/// Well-known authorization property names.
///
/// These constants are shared between the PEP compiler and the ORM condition
/// builder (`ScopableEntity::resolve_property()`), ensuring a single source of
/// truth for property names.
pub mod pep_properties {
    /// Tenant-ownership property. Typically maps to the `tenant_id` column.
    pub const OWNER_TENANT_ID: &str = "owner_tenant_id";

    /// Resource identity property. Typically maps to the primary key column.
    pub const RESOURCE_ID: &str = "id";

    /// Owner (user) identity property. Typically maps to an `owner_id` column.
    pub const OWNER_ID: &str = "owner_id";
}

/// A single scope filter — a typed predicate on a named resource property.
///
/// The property name (e.g., `"owner_tenant_id"`, `"id"`) is an authorization
/// concept. Mapping to DB columns is done by `ScopableEntity::resolve_property()`.
///
/// Variants mirror the predicate types from the PDP response:
/// - [`ScopeFilter::Eq`] — equality (`property = value`)
/// - [`ScopeFilter::In`] — set membership (`property IN (values)`)
///
/// ## Future extensions
///
/// Additional filter types (`in_tenant_subtree`, `in_group`,
/// `in_group_subtree`) are planned. See the authorization design document
/// (`docs/arch/authorization/DESIGN.md`) for the full predicate taxonomy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScopeFilter {
    /// Equality: `property = value`.
    Eq(EqScopeFilter),
    /// Set membership: `property IN (values)`.
    In(InScopeFilter),
}

/// Equality scope filter: `property = value`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EqScopeFilter {
    /// Authorization property name (e.g., `pep_properties::OWNER_TENANT_ID`).
    property: String,
    /// The value to match.
    value: ScopeValue,
}

/// Set membership scope filter: `property IN (values)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InScopeFilter {
    /// Authorization property name (e.g., `pep_properties::OWNER_TENANT_ID`).
    property: String,
    /// The set of values to match against.
    values: Vec<ScopeValue>,
}

impl EqScopeFilter {
    /// Create an equality scope filter.
    #[must_use]
    pub fn new(property: impl Into<String>, value: impl Into<ScopeValue>) -> Self {
        Self {
            property: property.into(),
            value: value.into(),
        }
    }

    /// The authorization property name.
    #[inline]
    #[must_use]
    pub fn property(&self) -> &str {
        &self.property
    }

    /// The filter value.
    #[inline]
    #[must_use]
    pub fn value(&self) -> &ScopeValue {
        &self.value
    }
}

impl InScopeFilter {
    /// Create a set membership scope filter.
    #[must_use]
    pub fn new(property: impl Into<String>, values: Vec<ScopeValue>) -> Self {
        Self {
            property: property.into(),
            values,
        }
    }

    /// Create from an iterator of convertible values.
    #[must_use]
    pub fn from_values<V: Into<ScopeValue>>(
        property: impl Into<String>,
        values: impl IntoIterator<Item = V>,
    ) -> Self {
        Self {
            property: property.into(),
            values: values.into_iter().map(Into::into).collect(),
        }
    }

    /// The authorization property name.
    #[inline]
    #[must_use]
    pub fn property(&self) -> &str {
        &self.property
    }

    /// The filter values.
    #[inline]
    #[must_use]
    pub fn values(&self) -> &[ScopeValue] {
        &self.values
    }
}

impl ScopeFilter {
    /// Create an equality filter (`property = value`).
    #[must_use]
    pub fn eq(property: impl Into<String>, value: impl Into<ScopeValue>) -> Self {
        Self::Eq(EqScopeFilter::new(property, value))
    }

    /// Create a set membership filter (`property IN (values)`).
    #[must_use]
    pub fn r#in(property: impl Into<String>, values: Vec<ScopeValue>) -> Self {
        Self::In(InScopeFilter::new(property, values))
    }

    /// Create a set membership filter from UUID values (convenience).
    #[must_use]
    pub fn in_uuids(property: impl Into<String>, uuids: Vec<Uuid>) -> Self {
        Self::In(InScopeFilter::new(
            property,
            uuids.into_iter().map(ScopeValue::Uuid).collect(),
        ))
    }

    /// The authorization property name.
    #[must_use]
    pub fn property(&self) -> &str {
        match self {
            Self::Eq(f) => f.property(),
            Self::In(f) => f.property(),
        }
    }

    /// Collect all values as a slice-like view for iteration.
    ///
    /// For `Eq`, returns a single-element slice; for `In`, returns the values slice.
    #[must_use]
    pub fn values(&self) -> ScopeFilterValues<'_> {
        match self {
            Self::Eq(f) => ScopeFilterValues::Single(&f.value),
            Self::In(f) => ScopeFilterValues::Multiple(&f.values),
        }
    }

    /// Extract filter values as UUIDs, skipping non-UUID entries.
    ///
    /// Useful when the caller knows the property holds UUID values
    /// (e.g., `owner_tenant_id`, `id`).
    #[must_use]
    pub fn uuid_values(&self) -> Vec<Uuid> {
        self.values()
            .iter()
            .filter_map(ScopeValue::as_uuid)
            .collect()
    }
}

/// Iterator adapter for [`ScopeFilter::values()`].
///
/// Provides a uniform way to iterate over filter values regardless of
/// whether the filter is `Eq` (single value) or `In` (multiple values).
#[derive(Clone, Debug)]
pub enum ScopeFilterValues<'a> {
    /// Single value from an `Eq` filter.
    Single(&'a ScopeValue),
    /// Multiple values from an `In` filter.
    Multiple(&'a [ScopeValue]),
}

impl<'a> ScopeFilterValues<'a> {
    /// Returns an iterator over the values.
    #[must_use]
    pub fn iter(&self) -> ScopeFilterValuesIter<'a> {
        match self {
            Self::Single(v) => ScopeFilterValuesIter::Single(Some(v)),
            Self::Multiple(vs) => ScopeFilterValuesIter::Multiple(vs.iter()),
        }
    }

    /// Returns `true` if any value matches the given predicate.
    #[must_use]
    pub fn contains(&self, value: &ScopeValue) -> bool {
        self.iter().any(|v| v == value)
    }
}

impl<'a> IntoIterator for ScopeFilterValues<'a> {
    type Item = &'a ScopeValue;
    type IntoIter = ScopeFilterValuesIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a> IntoIterator for &ScopeFilterValues<'a> {
    type Item = &'a ScopeValue;
    type IntoIter = ScopeFilterValuesIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over [`ScopeFilterValues`].
pub enum ScopeFilterValuesIter<'a> {
    /// Yields a single value.
    Single(Option<&'a ScopeValue>),
    /// Yields from a slice.
    Multiple(std::slice::Iter<'a, ScopeValue>),
}

impl<'a> Iterator for ScopeFilterValuesIter<'a> {
    type Item = &'a ScopeValue;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Single(v) => v.take(),
            Self::Multiple(iter) => iter.next(),
        }
    }
}

/// A conjunction (AND) of scope filters — one access path.
///
/// All filters within a constraint must match simultaneously for a row
/// to be accessible via this path.
#[derive(Clone, Debug, PartialEq)]
pub struct ScopeConstraint {
    filters: Vec<ScopeFilter>,
}

impl ScopeConstraint {
    /// Create a new scope constraint from a list of filters.
    #[must_use]
    pub fn new(filters: Vec<ScopeFilter>) -> Self {
        Self { filters }
    }

    /// The filters in this constraint (AND-ed together).
    #[inline]
    #[must_use]
    pub fn filters(&self) -> &[ScopeFilter] {
        &self.filters
    }

    /// Returns `true` if this constraint has no filters.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }
}

/// A disjunction (OR) of scope constraints defining what data is accessible.
///
/// Each constraint is an independent access path (OR-ed). Filters within a
/// constraint are AND-ed. An unconstrained scope bypasses row-level filtering.
///
/// # Examples
///
/// ```
/// use modkit_security::access_scope::{AccessScope, ScopeConstraint, ScopeFilter, pep_properties};
/// use uuid::Uuid;
///
/// // deny-all (default)
/// let scope = AccessScope::deny_all();
/// assert!(scope.is_deny_all());
///
/// // single tenant
/// let tid = Uuid::new_v4();
/// let scope = AccessScope::for_tenant(tid);
/// assert!(!scope.is_deny_all());
/// assert!(scope.contains_uuid(pep_properties::OWNER_TENANT_ID, tid));
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct AccessScope {
    constraints: Vec<ScopeConstraint>,
    unconstrained: bool,
}

impl Default for AccessScope {
    /// Default is deny-all: no constraints and not unconstrained.
    fn default() -> Self {
        Self::deny_all()
    }
}

impl AccessScope {
    // ── Constructors ────────────────────────────────────────────────

    /// Create an access scope from a list of constraints (OR-ed).
    #[must_use]
    pub fn from_constraints(constraints: Vec<ScopeConstraint>) -> Self {
        Self {
            constraints,
            unconstrained: false,
        }
    }

    /// Create an access scope with a single constraint.
    #[must_use]
    pub fn single(constraint: ScopeConstraint) -> Self {
        Self::from_constraints(vec![constraint])
    }

    /// Create an "allow all" (unconstrained) scope.
    ///
    /// This represents a legitimate PDP decision with no row-level filtering.
    /// Not a bypass — it's a valid authorization outcome.
    #[must_use]
    pub fn allow_all() -> Self {
        Self {
            constraints: Vec::new(),
            unconstrained: true,
        }
    }

    /// Create a "deny all" scope (no access).
    #[must_use]
    pub fn deny_all() -> Self {
        Self {
            constraints: Vec::new(),
            unconstrained: false,
        }
    }

    // ── Convenience constructors ────────────────────────────────────

    /// Create a scope for a set of tenant IDs.
    #[must_use]
    pub fn for_tenants(ids: Vec<Uuid>) -> Self {
        Self::single(ScopeConstraint::new(vec![ScopeFilter::in_uuids(
            pep_properties::OWNER_TENANT_ID,
            ids,
        )]))
    }

    /// Create a scope for a single tenant ID.
    #[must_use]
    pub fn for_tenant(id: Uuid) -> Self {
        Self::for_tenants(vec![id])
    }

    /// Create a scope for a set of resource IDs.
    #[must_use]
    pub fn for_resources(ids: Vec<Uuid>) -> Self {
        Self::single(ScopeConstraint::new(vec![ScopeFilter::in_uuids(
            pep_properties::RESOURCE_ID,
            ids,
        )]))
    }

    /// Create a scope for a single resource ID.
    #[must_use]
    pub fn for_resource(id: Uuid) -> Self {
        Self::for_resources(vec![id])
    }

    // ── Accessors ───────────────────────────────────────────────────

    /// The constraints in this scope (OR-ed).
    #[inline]
    #[must_use]
    pub fn constraints(&self) -> &[ScopeConstraint] {
        &self.constraints
    }

    /// Returns `true` if this scope is unconstrained (allow-all).
    #[inline]
    #[must_use]
    pub fn is_unconstrained(&self) -> bool {
        self.unconstrained
    }

    /// Returns `true` if this scope denies all access.
    ///
    /// A scope is deny-all when it is not unconstrained and has no constraints.
    #[must_use]
    pub fn is_deny_all(&self) -> bool {
        !self.unconstrained && self.constraints.is_empty()
    }

    /// Collect all values for a given property across all constraints.
    #[must_use]
    pub fn all_values_for(&self, property: &str) -> Vec<&ScopeValue> {
        let mut result = Vec::new();
        for constraint in &self.constraints {
            for filter in constraint.filters() {
                if filter.property() == property {
                    result.extend(filter.values());
                }
            }
        }
        result
    }

    /// Collect all UUID values for a given property across all constraints.
    ///
    /// Convenience wrapper — skips non-UUID values.
    #[must_use]
    pub fn all_uuid_values_for(&self, property: &str) -> Vec<Uuid> {
        let mut result = Vec::new();
        for constraint in &self.constraints {
            for filter in constraint.filters() {
                if filter.property() == property {
                    result.extend(filter.uuid_values());
                }
            }
        }
        result
    }

    /// Check if any constraint has a filter matching the given property and value.
    #[must_use]
    pub fn contains_value(&self, property: &str, value: &ScopeValue) -> bool {
        self.constraints.iter().any(|c| {
            c.filters()
                .iter()
                .any(|f| f.property() == property && f.values().contains(value))
        })
    }

    /// Check if any constraint has a filter matching the given property and UUID.
    ///
    /// Matches both `ScopeValue::Uuid` and `ScopeValue::String` variants so
    /// that UUID-as-string values are treated consistently with
    /// [`AccessScope::all_uuid_values_for`], which also parses strings via
    /// [`ScopeValue::as_uuid`].
    #[must_use]
    pub fn contains_uuid(&self, property: &str, id: Uuid) -> bool {
        self.constraints.iter().any(|c| {
            c.filters().iter().any(|f| {
                f.property() == property && f.values().iter().any(|v| v.as_uuid() == Some(id))
            })
        })
    }

    /// Check if any constraint references the given property.
    #[must_use]
    pub fn has_property(&self, property: &str) -> bool {
        self.constraints
            .iter()
            .any(|c| c.filters().iter().any(|f| f.property() == property))
    }

    /// Create a new scope retaining only `owner_tenant_id` filters.
    ///
    /// Useful for entities declared with `no_owner` (e.g., messages, reactions),
    /// where `owner_id` constraints cannot be resolved and would cause fail-closed
    /// deny-all behaviour.
    ///
    /// - Unconstrained scopes become deny-all (fail-closed).
    /// - Constraints that contain no `owner_tenant_id` filter are dropped entirely.
    /// - If all constraints are dropped, the result is deny-all.
    #[must_use]
    pub fn tenant_only(&self) -> Self {
        self.retain_properties(&[pep_properties::OWNER_TENANT_ID])
    }

    /// Create a new scope retaining only `owner_tenant_id` and `owner_id` filters.
    ///
    /// Useful for entities that have both tenant and owner columns but no
    /// resource-level constraints (e.g., reactions scoped to the acting user).
    ///
    /// - Unconstrained scopes become deny-all (fail-closed).
    /// - Constraints that contain none of the retained properties are dropped.
    /// - If all constraints are dropped, the result is deny-all.
    #[must_use]
    pub fn tenant_and_owner(&self) -> Self {
        self.retain_properties(&[pep_properties::OWNER_TENANT_ID, pep_properties::OWNER_ID])
    }

    /// Create a new scope that guarantees an `owner_id` equality filter
    /// matching exactly the supplied `owner_id` is present in every constraint.
    ///
    /// **Intersection semantics**: if a constraint already contains an
    /// `owner_id` filter, the supplied value must be among its values —
    /// otherwise the constraint is dropped. When it matches, the filter is
    /// narrowed to exactly that single value.
    ///
    /// - **Unconstrained** → single constraint with only the `owner_id` filter.
    /// - **Deny-all** → stays deny-all.
    /// - **No existing owner filter** → `owner_id` is injected.
    /// - **Existing owner filter containing `owner_id`** → narrowed to `Eq`.
    /// - **Existing owner filter NOT containing `owner_id`** → constraint dropped
    ///   (constraints use OR semantics, so dropping one narrows access; dropping
    ///   all yields deny-all).
    ///
    /// Use this as a defence-in-depth measure for user-owned resources when
    /// the PDP may not always return `owner_id` constraints or may return a
    /// broader set than the current subject.
    #[must_use]
    pub fn ensure_owner(&self, owner_id: Uuid) -> Self {
        if self.is_deny_all() {
            return Self::deny_all();
        }

        let owner_filter = ScopeFilter::eq(pep_properties::OWNER_ID, owner_id);

        if self.unconstrained {
            return Self::single(ScopeConstraint::new(vec![owner_filter]));
        }

        let constraints = self
            .constraints
            .iter()
            .filter_map(|c| {
                let owner_filters: Vec<&ScopeFilter> = c
                    .filters()
                    .iter()
                    .filter(|f| f.property() == pep_properties::OWNER_ID)
                    .collect();

                if owner_filters.is_empty() {
                    let mut filters = c.filters().to_vec();
                    filters.push(owner_filter.clone());
                    return Some(ScopeConstraint::new(filters));
                }

                // Intersection semantics: ALL owner_id predicates must contain
                // the supplied owner_id, otherwise the constraint is dropped.
                let all_match = owner_filters
                    .iter()
                    .all(|f| f.values().iter().any(|v| v.as_uuid() == Some(owner_id)));
                if !all_match {
                    return None;
                }

                // Fast path: single Eq already matches → constraint unchanged.
                if owner_filters.len() == 1 && matches!(owner_filters[0], ScopeFilter::Eq(_)) {
                    return Some(c.clone());
                }

                // Replace all owner_id filters with a single Eq.
                let mut filters: Vec<ScopeFilter> = c
                    .filters()
                    .iter()
                    .filter(|f| f.property() != pep_properties::OWNER_ID)
                    .cloned()
                    .collect();
                filters.push(owner_filter.clone());
                Some(ScopeConstraint::new(filters))
            })
            .collect();

        Self::from_constraints(constraints)
    }

    /// Internal helper: build a new scope keeping only filters whose property
    /// is in the given whitelist.
    fn retain_properties(&self, properties: &[&str]) -> Self {
        if self.unconstrained {
            return Self::deny_all();
        }

        let constraints = self
            .constraints
            .iter()
            .filter_map(|c| {
                let kept: Vec<ScopeFilter> = c
                    .filters()
                    .iter()
                    .filter(|f| properties.contains(&f.property()))
                    .cloned()
                    .collect();

                if kept.is_empty() {
                    None
                } else {
                    Some(ScopeConstraint::new(kept))
                }
            })
            .collect();

        Self::from_constraints(constraints)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "access_scope_tests.rs"]
mod tests;
