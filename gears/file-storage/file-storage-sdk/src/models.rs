//! Public model types for the file-storage gear.
//!
//! Contract-layer types only — no `serde`, no HTTP, no `utoipa`. REST DTOs live
//! in the impl crate under `api/rest/`. These are the transport-agnostic domain
//! types other gears and the impl layers share.

use time::OffsetDateTime;
use uuid::Uuid;

/// Immutable identity of a logical file (PRD: `File ID`).
pub type FileId = Uuid;

/// Identity of one immutable content blob of a file (PRD: `Version ID`).
pub type VersionId = Uuid;

/// The principal that owns a file (PRD `cpt-cf-file-storage-fr-file-ownership`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerKind {
    /// A platform user.
    User,
    /// A Gear (app), e.g. the LLM Gateway owning its generated media.
    App,
}

impl OwnerKind {
    /// The wire/DB spelling (`"user"` / `"app"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::App => "app",
        }
    }

    /// Parse from the DB/wire spelling; `None` for anything else.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Self::User),
            "app" => Some(Self::App),
            _ => None,
        }
    }
}

/// Lifecycle of a content version (PRD: `pending` → `available`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionStatus {
    /// Pre-registered, bytes may be uploading; not yet bindable as current.
    Pending,
    /// Bytes durably written and verified; bindable as the file's content.
    Available,
}

impl VersionStatus {
    /// The wire/DB spelling (`"pending"` / `"available"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Available => "available",
        }
    }

    /// Parse from the DB/wire spelling; `None` for anything else.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "available" => Some(Self::Available),
            _ => None,
        }
    }
}

/// A logical file: stable identity plus the current content pointer. Holds no
/// bytes — content lives in [`FileVersion`] objects on a storage backend.
// `file_id` mirrors the DB column name and the domain vocabulary.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub file_id: FileId,
    pub tenant_id: Uuid,
    pub owner_kind: OwnerKind,
    pub owner_id: Uuid,
    pub name: String,
    pub gts_file_type: String,
    /// `version_id` currently bound as live content; `None` until first bind.
    pub content_id: Option<VersionId>,
    /// Monotonic counter bumped on metadata-only writes (`If-Match-Metadata`).
    pub meta_version: i64,
    pub created_at: OffsetDateTime,
    pub last_modified_at: OffsetDateTime,
}

/// An immutable content version of a [`File`]. The backend object lives at
/// `/{file_id}/{version_id}` and is never mutated in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileVersion {
    pub file_id: FileId,
    pub version_id: VersionId,
    pub mime_type: String,
    pub size: i64,
    pub hash_algorithm: String,
    pub hash_value: Vec<u8>,
    pub status: VersionStatus,
    pub is_current: bool,
    pub backend_id: String,
    pub backend_path: String,
    pub created_at: OffsetDateTime,
}

/// One user-defined custom-metadata key/value pair attached to a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomMetadataEntry {
    pub key: String,
    pub value: String,
}

/// Data to create a new file (control-plane `POST /files`). The tenant is taken
/// from the authenticated caller, never from the request. The first content
/// version is pre-registered and bound after upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFile {
    pub owner_kind: OwnerKind,
    pub owner_id: Uuid,
    pub name: String,
    pub gts_file_type: String,
    /// Declared content type of the first version (validated against bytes).
    pub mime_type: String,
    pub custom_metadata: Vec<CustomMetadataEntry>,
}

/// Mandatory owner filter for listing (PRD `cpt-cf-file-storage-fr-list-files`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnerFilter {
    pub owner_kind: OwnerKind,
    pub owner_id: Uuid,
}

/// A parsed HTTP `Range` request over a content blob of known length
/// (PRD `cpt-cf-file-storage-fr-range-requests`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRange {
    /// `bytes=start-end` (both inclusive).
    Inclusive { start: u64, end: u64 },
    /// `bytes=start-` (start to end of content).
    OpenEnded { start: u64 },
    /// `bytes=-length` (the final `length` bytes).
    Suffix { length: u64 },
}

impl ByteRange {
    /// Resolve to a concrete inclusive `[start, end]` against `total` bytes.
    /// Returns `None` if the range is unsatisfiable for that length.
    #[must_use]
    pub fn resolve(self, total: u64) -> Option<(u64, u64)> {
        if total == 0 {
            return None;
        }
        match self {
            Self::Inclusive { start, end } => {
                if start >= total || start > end {
                    return None;
                }
                Some((start, end.min(total - 1)))
            }
            Self::OpenEnded { start } => {
                if start >= total {
                    return None;
                }
                Some((start, total - 1))
            }
            Self::Suffix { length } => {
                if length == 0 {
                    return None;
                }
                let len = length.min(total);
                Some((total - len, total - 1))
            }
        }
    }
}

/// JSON-Merge-Patch semantics for custom metadata: `Some(value)` upserts a key,
/// `None` deletes it; absent keys are unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CustomMetadataPatch {
    pub entries: Vec<(String, Option<String>)>,
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod models_tests;
