//! Content conversions and the write-time checks of the version lifecycle.
//!
//! [`check_write`] refuses content the store cannot hold or the limits do not
//! admit: an exceeded operational limit (`CONTENT_LIMIT_EXCEEDED`) and a
//! document name used twice. Everything else - syntax, entrypoint, denylisted
//! builtins, patterns, unknown types - is left to validation, so a draft can
//! be saved while incomplete.

use policy_engine_sdk::management::{self as sdk, DocumentSpec, ManagementError, VersionContent};
use uuid::Uuid;

use super::error;
use crate::domain::model::{
    Bundle, BundleVersion, ContentLimits, Document, DocumentId, VersionId, VersionState,
    document_name_valid, duplicate_names, limits,
};

// ---------------------------------------------------------------------------
// SDK -> domain
// ---------------------------------------------------------------------------

/// The domain documents of `content`, with fresh identities.
#[must_use]
pub fn documents_from_content(content: &VersionContent) -> Vec<Document> {
    content
        .documents
        .iter()
        .map(|spec| Document {
            id: DocumentId(Uuid::new_v4()),
            name: spec.name.clone(),
            content: spec.content.clone(),
            resource_types: spec.resource_types.clone(),
            actions: spec.actions.clone(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Domain -> SDK
// ---------------------------------------------------------------------------

/// The SDK form of a bundle, with its active version.
#[must_use]
pub fn bundle_to_sdk(bundle: &Bundle, active_version_id: Option<VersionId>) -> sdk::Bundle {
    sdk::Bundle {
        id: bundle.id,
        owner_tenant_id: bundle.owner_tenant_id,
        name: bundle.name.clone(),
        description: bundle.description.clone(),
        active_version_id,
        created_at: bundle.created_at,
        created_by: bundle.created_by,
        updated_at: bundle.updated_at,
    }
}

/// The SDK form of a version header.
#[must_use]
pub fn version_to_sdk(version: &BundleVersion) -> sdk::BundleVersion {
    sdk::BundleVersion {
        id: version.id,
        bundle_id: version.bundle_id,
        owner_tenant_id: version.owner_tenant_id,
        ordinal: version.ordinal,
        state: match version.state {
            VersionState::Draft => sdk::VersionState::Draft,
            VersionState::Active => sdk::VersionState::Active,
            VersionState::Superseded => sdk::VersionState::Superseded,
        },
        created_at: version.created_at,
        activated_at: version.activated_at,
        activated_by: version.activated_by,
    }
}

/// The SDK form of a version with its documents, ordered by name.
#[must_use]
pub fn detail_to_sdk(version: &BundleVersion) -> sdk::VersionDetail {
    let mut documents: Vec<sdk::Document> = version
        .documents
        .iter()
        .map(|d| sdk::Document {
            id: d.id,
            spec: DocumentSpec {
                name: d.name.clone(),
                content: d.content.clone(),
                resource_types: d.resource_types.clone(),
                actions: d.actions.clone(),
            },
        })
        .collect();
    documents.sort_by(|a, b| a.spec.name.cmp(&b.spec.name));
    sdk::VersionDetail {
        version: version_to_sdk(version),
        documents,
    }
}

// ---------------------------------------------------------------------------
// Write-time checks
// ---------------------------------------------------------------------------

/// Refuses content the limits do not admit or the store cannot hold.
///
/// # Errors
///
/// `CONTENT_LIMIT_EXCEEDED`, `INVALID_DOCUMENT_NAME`,
/// `DUPLICATE_DOCUMENT_NAME`.
pub fn check_write(limits: &ContentLimits, documents: &[Document]) -> Result<(), ManagementError> {
    let violations = limits.check(documents);
    if !violations.is_empty() {
        return Err(error::content_limit_exceeded(&violations));
    }
    if let Some(document) = documents.iter().find(|d| !document_name_valid(&d.name)) {
        return Err(ManagementError::InvalidDocumentName {
            name: document.name.clone(),
        });
    }
    match duplicate_names(documents).first() {
        Some((name, _)) => Err(ManagementError::DuplicateDocumentName {
            name: (*name).to_owned(),
        }),
        None => Ok(()),
    }
}

/// Refuses a bundle name or description the store must not hold: a blank
/// name, a control character in the name, or either longer than its
/// [`limits`] bound. `None` leaves a field unchecked (an update that keeps
/// it).
///
/// # Errors
///
/// `INVALID_BUNDLE` naming the field.
pub fn check_bundle(name: Option<&str>, description: Option<&str>) -> Result<(), ManagementError> {
    let invalid = |field: &str, reason: String| ManagementError::InvalidBundle {
        field: field.to_owned(),
        reason,
    };
    if let Some(name) = name {
        if name.trim().is_empty() || name.chars().any(char::is_control) {
            return Err(invalid(
                "name",
                "must not be blank or hold a control character".to_owned(),
            ));
        }
        if name.len() > limits::MAX_BUNDLE_NAME_BYTES {
            return Err(invalid(
                "name",
                format!("longer than {} bytes", limits::MAX_BUNDLE_NAME_BYTES),
            ));
        }
    }
    if let Some(description) = description
        && description.len() > limits::MAX_BUNDLE_DESCRIPTION_BYTES
    {
        return Err(invalid(
            "description",
            format!("longer than {} bytes", limits::MAX_BUNDLE_DESCRIPTION_BYTES),
        ));
    }
    Ok(())
}

/// The documents a draft seeded from `seed` starts with: the same content
/// under fresh identities.
#[must_use]
pub fn seed_documents(seed: &BundleVersion) -> Vec<Document> {
    seed.documents
        .iter()
        .cloned()
        .map(|mut document| {
            document.id = DocumentId(Uuid::new_v4());
            document
        })
        .collect()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "lifecycle_tests.rs"]
mod lifecycle_tests;
