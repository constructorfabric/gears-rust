//! Why record intake refuses a record. Kept apart from intake so the domain
//! error can carry a refusal without depending on intake.

use construct_sdk::reason;
use toolkit_macros::domain_model;

/// Why intake refuses a record.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalReason {
    /// The record breaks a rule of its type's schema, or of the base envelope.
    SchemaViolation,
    /// The record names a type the types registry does not know.
    UnknownType,
    /// The record's type does not derive from the record base type.
    NotARecordType,
    /// The record names an abstract type.
    AbstractType,
    /// The record carries an `id`, which a push must not.
    IdInPush,
    /// The connector is turned off for the tenant.
    ConnectorOff,
    /// Personalization is off for the record's subject.
    PersonalizationOff,
    /// An erasure of the record's subject is under way.
    ErasureInProgress,
}

impl RefusalReason {
    /// The reason code a refusal carries, from the SDK's `reason` module.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::SchemaViolation => reason::SCHEMA_VIOLATION,
            Self::UnknownType => reason::UNKNOWN_TYPE,
            Self::NotARecordType => reason::NOT_A_RECORD_TYPE,
            Self::AbstractType => reason::ABSTRACT_TYPE,
            Self::IdInPush => reason::ID_IN_PUSH,
            Self::ConnectorOff => reason::CONNECTOR_OFF,
            Self::PersonalizationOff => reason::PERSONALIZATION_OFF,
            Self::ErasureInProgress => reason::ERASURE_IN_PROGRESS,
        }
    }
}

/// Where in the record a refusal applies.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// The record as a whole.
    Whole,
    /// One member, as a JSON pointer such as `/payload/role`.
    Pointer(String),
}

impl Place {
    pub fn pointer(pointer: impl Into<String>) -> Self {
        Self::Pointer(pointer.into())
    }
}

/// The wire name of a place: the JSON pointer, or `(record)` for the record as
/// a whole.
impl std::fmt::Display for Place {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Whole => f.write_str("(record)"),
            Self::Pointer(pointer) => f.write_str(pointer),
        }
    }
}

/// A refused record. It names the record's type once the type is known to be
/// a well-formed type id, the place in the record, and the broken rule. It
/// never repeats a value from the record.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub reason: RefusalReason,
    pub type_id: Option<String>,
    pub place: Place,
    pub rule: String,
}

impl Refusal {
    pub fn new(
        reason: RefusalReason,
        type_id: Option<&str>,
        place: Place,
        rule: impl Into<String>,
    ) -> Self {
        Self {
            reason,
            type_id: type_id.map(str::to_owned),
            place,
            rule: rule.into(),
        }
    }
}
