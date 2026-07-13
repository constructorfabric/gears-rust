//! AM-internal validated `type_id` type for the tenant-metadata flow.
//!
//! [`ParsedTypeId`] is the service-layer guard that turns a raw
//! wire `type_id` string into a typed value before any registry or
//! repo call. Four checks run in order:
//!
//! 1. Parse the wire `type_id` string via [`gts::GtsId::try_new`] —
//!    rejects malformed GTS syntax.
//! 2. Require the root segment match
//!    [`METADATA_ROOT_SEGMENT`] (`cf.core.am.tenant_metadata.v1`) —
//!    rejects schemas from other namespaces.
//! 3. Require at least one chained user-registered schema segment
//!    after the root.
//! 4. Require schema-shape (`GtsId::is_type` — every segment ends
//!    with `~`); reject instance-id shapes.
//!
//! On success [`ParsedTypeId`] also caches the deterministic
//! `UUIDv5` derived through [`gts::GtsId::to_uuid`] — same namespace
//! the upstream `gts` crate uses internally, so AM and any sibling
//! consuming the `gts` crate directly agree on the storage-side
//! `schema_uuid` mapping.
//!
//! All validation failures collapse onto
//! [`DomainError::MetadataValidation`] which surfaces as
//! `CanonicalError::InvalidArgument` (HTTP 400) at the AM canonical
//! boundary. The SDK ships raw `String` for `type_id` and never
//! sees the granular validation error variants.

use gts::{GtsId, GtsTypeId};
use toolkit_gts::GTS_ID_PREFIX;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Root segment every parsed chain MUST start with. Stored without
/// the `gts.` prefix and trailing `~` because `GtsIdSegment::segment`
/// exposes only the segment body.
const METADATA_ROOT_SEGMENT: &str = "cf.core.am.tenant_metadata.v1";

/// Parsed-and-validated chained metadata schema id, paired with its
/// deterministic `UUIDv5`. AM-internal — never crosses the SDK
/// boundary.
///
/// Construct via [`ParsedTypeId::parse`]. The wire-shape boundary
/// (REST handler, SDK trait `GtsTypeId` input) calls `parse` as the
/// first step on every public metadata method.
#[domain_model]
#[derive(Debug)]
pub(crate) struct ParsedTypeId {
    raw: GtsTypeId,
    uuid: Uuid,
}

impl ParsedTypeId {
    /// Validate and parse a wire-shape `type_id` string.
    ///
    /// # Errors
    ///
    /// [`DomainError::MetadataValidation`] with a `detail` describing the
    /// specific failure mode (malformed GTS, wrong root segment,
    /// missing chained segment, instance-id shape).
    pub(crate) fn parse(s: &str) -> Result<Self, DomainError> {
        let parsed = GtsId::try_new(s).map_err(|err| DomainError::MetadataValidation {
            detail: format!("malformed metadata schema id: {err}"),
        })?;

        let segments = parsed.segments();
        if segments.len() < 2 {
            return Err(DomainError::MetadataValidation {
                detail: format!(
                    "metadata schema id `{}` is missing a chained user-registered segment \
                     after the root (`{GTS_ID_PREFIX}{METADATA_ROOT_SEGMENT}`)",
                    parsed.as_ref()
                ),
            });
        }

        // `GtsIdSegment.segment` includes the trailing `~`; strip
        // before comparing against the root constant.
        let root_str = segments[0].raw().trim_end_matches('~');
        if root_str != METADATA_ROOT_SEGMENT {
            return Err(DomainError::MetadataValidation {
                detail: format!(
                    "metadata schema id must start with `{GTS_ID_PREFIX}{METADATA_ROOT_SEGMENT}`, \
                     got `{GTS_ID_PREFIX}{root_str}`"
                ),
            });
        }

        // Schema-shape: every segment of a schema chain ends with `~`.
        // An instance id whose tail segment lacks `~` parses cleanly
        // as a `GtsId` but is NOT a schema chain — reject at the
        // boundary so the downstream `schema_uuid` lookup does not
        // surface as a confusing 404.
        if !parsed.is_type() {
            return Err(DomainError::MetadataValidation {
                detail: format!(
                    "metadata schema id `{}` is an instance id, not a schema chain",
                    parsed.as_ref()
                ),
            });
        }

        // `gts::GtsId::to_uuid()` hashes `self.id.as_bytes()` under the
        // upstream `GTS_NS` (= `Uuid::new_v5(&NAMESPACE_URL, b"gts")`).
        // Single source of truth shared with every sibling that
        // imports the `gts` crate.
        let uuid = parsed.to_uuid();

        // Use parsed.as_ref(), not the original `s`: GtsId::try_new trims
        // whitespace; storing the trimmed form keeps schema_uuid
        // consistent with reverse-hydration.
        Ok(Self {
            raw: GtsTypeId::new(parsed.as_ref()),
            uuid,
        })
    }

    /// Borrow the chained id as a string slice (verbatim, no
    /// re-formatting). Used by PEP `TYPE_ID` attribute and
    /// `MetadataEntry.type_id` echo on read responses.
    pub(crate) fn as_str(&self) -> &str {
        self.raw.as_ref()
    }

    /// Borrow the underlying [`gts::GtsTypeId`] — platform-standard
    /// marker for "this string is a GTS schema id". Preferred over
    /// [`Self::as_str`] when handing the id off to an API that takes
    /// the typed `GtsTypeId` form (e.g. the
    /// [`crate::domain::metadata::registry::MetadataSchemaRegistry`]
    /// trait surface).
    pub(crate) const fn as_gts(&self) -> &GtsTypeId {
        &self.raw
    }

    /// Borrow the deterministic `UUIDv5` — the storage-side PK
    /// component on `(tenant_id, schema_uuid)`. O(1) field read; no
    /// hash work on call.
    pub(crate) const fn uuid(&self) -> Uuid {
        self.uuid
    }

    /// Consume into the normalised string form. Used when the caller
    /// no longer needs the UUID and wants to echo the validated id
    /// back as a `MetadataEntry.type_id` field without an extra
    /// `to_owned()`.
    #[allow(
        dead_code,
        reason = "kept on the surface for callers that prefer the normalised \
                  string echo; current flow uses the caller-supplied String."
    )]
    pub(crate) fn into_string(self) -> String {
        self.raw.into()
    }
}

#[cfg(test)]
#[path = "type_id_tests.rs"]
mod tests;
