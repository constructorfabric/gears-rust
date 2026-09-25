//! The one normalized field set of all three reads (SPEC §10.2). Storage fetches
//! only what it names; cursors and T29 validators use [`FieldSelection::canonical`].

use toolkit_macros::domain_model;

/// Declared in alphabetical order: that order is the canonical spelling.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntityField {
    Content,
    EffectiveTraits,
    EffectiveTraitsSchema,
    GtsId,
    GtsUuid,
    Kind,
    LifecycleStatus,
    Origin,
    Provenance,
    ResolvedSchema,
}

impl EntityField {
    pub const ALL: [Self; 10] = [
        Self::Content,
        Self::EffectiveTraits,
        Self::EffectiveTraitsSchema,
        Self::GtsId,
        Self::GtsUuid,
        Self::Kind,
        Self::LifecycleStatus,
        Self::Origin,
        Self::Provenance,
        Self::ResolvedSchema,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Content => "content",
            Self::EffectiveTraits => "effective_traits",
            Self::EffectiveTraitsSchema => "effective_traits_schema",
            Self::GtsId => "gts_id",
            Self::GtsUuid => "gts_uuid",
            Self::Kind => "kind",
            Self::LifecycleStatus => "lifecycle_status",
            Self::Origin => "origin",
            Self::Provenance => "provenance",
            Self::ResolvedSchema => "resolved_schema",
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|field| field.name() == name)
    }

    #[must_use]
    pub const fn is_document(self) -> bool {
        matches!(
            self,
            Self::Content
                | Self::ResolvedSchema
                | Self::EffectiveTraits
                | Self::EffectiveTraitsSchema
        )
    }

    const fn bit(self) -> u16 {
        1 << self as u16
    }
}

/// DESIGN §3.3 fields that P0 cannot answer without tenancy, so it neither
/// advertises nor synthesizes them.
const UNAVAILABLE: [&str; 2] = ["availability", "owned_by_context_tenant"];

/// A bitset, so equality is identity. [`FieldSelection::MANDATORY_FIELDS`] are
/// always members: naming one must not change a cursor or validator.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FieldSelection(u16);

impl Default for FieldSelection {
    /// The document-free default of all three reads (SPEC §10.2).
    fn default() -> Self {
        Self::of(&Self::DEFAULT_FIELDS)
    }
}

impl FieldSelection {
    /// Identity, kind and lifecycle: on every entity whatever `$select` names.
    pub const MANDATORY_FIELDS: [EntityField; 4] = [
        EntityField::GtsId,
        EntityField::GtsUuid,
        EntityField::Kind,
        EntityField::LifecycleStatus,
    ];

    pub const DEFAULT_FIELDS: [EntityField; 5] = [
        EntityField::GtsId,
        EntityField::GtsUuid,
        EntityField::Kind,
        EntityField::Origin,
        EntityField::LifecycleStatus,
    ];

    #[must_use]
    pub fn full() -> Self {
        Self::of(&EntityField::ALL)
    }

    fn of(fields: &[EntityField]) -> Self {
        Self(
            Self::MANDATORY_FIELDS
                .iter()
                .chain(fields)
                .fold(0, |bits, field| bits | field.bit()),
        )
    }

    /// Trimmed and case-insensitive, as `ToolKit` `OData` parses names.
    ///
    /// # Errors
    /// A [`SelectionError`] for an empty selection or segment, a duplicate after
    /// normalization, or a name that is unknown, unavailable in P0 or nested.
    pub fn parse<S: AsRef<str>>(names: &[S]) -> Result<Self, SelectionError> {
        if names.is_empty() {
            return Err(SelectionError::Empty);
        }
        let mut bits = Self::of(&[]).0;
        let mut seen: u16 = 0;
        for raw in names {
            let name = raw.as_ref().trim().to_lowercase();
            if name.is_empty() {
                return Err(SelectionError::EmptySegment);
            }
            if name.contains(['.', '/']) {
                return Err(SelectionError::Nested(name));
            }
            if UNAVAILABLE.contains(&name.as_str()) {
                return Err(SelectionError::Unavailable(name));
            }
            let Some(field) = EntityField::from_name(&name) else {
                return Err(SelectionError::Unknown(name));
            };
            if seen & field.bit() != 0 {
                return Err(SelectionError::Duplicate(name));
            }
            seen |= field.bit();
            bits |= field.bit();
        }
        Ok(Self(bits))
    }

    #[must_use]
    pub const fn contains(self, field: EntityField) -> bool {
        self.0 & field.bit() != 0
    }

    pub fn fields(self) -> impl Iterator<Item = EntityField> {
        EntityField::ALL
            .into_iter()
            .filter(move |field| self.contains(*field))
    }

    #[must_use]
    pub fn selects_any_document(self) -> bool {
        self.fields().any(EntityField::is_document)
    }

    /// Sorted and comma-joined; never the caller's raw text.
    #[must_use]
    pub fn canonical(self) -> String {
        self.fields()
            .map(EntityField::name)
            .collect::<Vec<_>>()
            .join(",")
    }
}

#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SelectionError {
    #[error("a selection must name at least one field")]
    Empty,
    #[error("a selection must not contain an empty field name")]
    EmptySegment,
    #[error("field '{0}' is selected more than once")]
    Duplicate(String),
    #[error("'{0}' is not a selectable field")]
    Unknown(String),
    #[error("field '{0}' is not available in this version")]
    Unavailable(String),
    #[error("'{0}' names a path inside a field; select the whole field")]
    Nested(String),
}

#[cfg(test)]
#[path = "selection_tests.rs"]
mod selection_tests;
