//! How a caller names one entity: its GTS identifier or its Registry Reference.

use std::fmt;

use gts::GtsId;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// A key's ceiling in bytes, on reads and writes alike: a GTS identifier runs to 1024.
pub const MAX_KEY_LEN: usize = 1024;

/// GTS identifier or deterministic Registry Reference for the same row.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EntityKey {
    GtsId(String),
    Uuid(Uuid),
}

impl EntityKey {
    /// Parse a UUID as a Registry Reference; otherwise keep the GTS identifier.
    ///
    /// Unambiguous because a GTS identifier always starts with `gts.`.
    #[must_use]
    pub fn parse(key: &str) -> Self {
        match Uuid::parse_str(key) {
            Ok(uuid) => Self::Uuid(uuid),
            Err(_) => Self::GtsId(key.to_owned()),
        }
    }

    /// The identifier, when this key spells one.
    #[must_use]
    pub fn gts_id(&self) -> Option<&str> {
        match self {
            Self::GtsId(gts_id) => Some(gts_id),
            Self::Uuid(_) => None,
        }
    }

    /// The Registry Reference both spellings of one entity share; `None` for an
    /// identifier that does not parse.
    #[must_use]
    pub fn gts_uuid(&self) -> Option<Uuid> {
        match self {
            Self::GtsId(gts_id) => GtsId::try_new(gts_id).ok().map(|id| id.to_uuid()),
            Self::Uuid(gts_uuid) => Some(*gts_uuid),
        }
    }
}

/// The stored and echoed spelling, which [`EntityKey::parse`] reads back.
impl fmt::Display for EntityKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GtsId(gts_id) => f.write_str(gts_id),
            Self::Uuid(gts_uuid) => write!(f, "{}", gts_uuid.hyphenated()),
        }
    }
}

#[cfg(test)]
#[path = "key_tests.rs"]
mod key_tests;
