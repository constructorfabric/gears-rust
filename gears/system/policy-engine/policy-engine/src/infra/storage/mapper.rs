//! Entity <-> domain conversions for the content tables.
//!
//! The `smallint` state decodes through [`VersionState::from_code`]; an
//! unknown code or a malformed JSON array is a [`RepoError::Database`] naming
//! the row, never a panic.

use sea_orm::ActiveValue::Set;
use serde_json::Value;
use uuid::Uuid;

use super::entity::{assignment, bundle, bundle_version, document};
use crate::domain::model::{
    Assignment, AssignmentId, Bundle, BundleId, BundleVersion, Document, DocumentId, VersionId,
    VersionState,
};
use crate::domain::repos::RepoError;

// The entity code constants are the ones the migrations' partial index
// expressions use; they must equal the domain codes.
const _: () = {
    assert!(bundle_version::STATE_DRAFT == VersionState::Draft.code());
    assert!(bundle_version::STATE_ACTIVE == VersionState::Active.code());
    assert!(bundle_version::STATE_SUPERSEDED == VersionState::Superseded.code());
};

fn corrupt(table: &str, id: impl std::fmt::Display, what: impl std::fmt::Display) -> RepoError {
    RepoError::Database(format!("{table} row {id}: {what}"))
}

// ---------------------------------------------------------------------------
// Bundle
// ---------------------------------------------------------------------------

/// Stored bundle -> domain. An absent description reads as empty.
#[must_use]
pub fn bundle_from_model(m: bundle::Model) -> Bundle {
    Bundle {
        id: BundleId(m.id),
        owner_tenant_id: m.owner_tenant_id,
        name: m.name,
        description: m.description.unwrap_or_default(),
        created_at: m.created_at,
        created_by: m.created_by,
        updated_at: m.updated_at,
    }
}

/// Domain bundle -> insertable row. An empty description is stored as null.
#[must_use]
pub fn bundle_active_model(b: &Bundle) -> bundle::ActiveModel {
    bundle::ActiveModel {
        id: Set(b.id.0),
        owner_tenant_id: Set(b.owner_tenant_id),
        name: Set(b.name.clone()),
        description: Set((!b.description.is_empty()).then(|| b.description.clone())),
        created_at: Set(b.created_at),
        created_by: Set(b.created_by),
        updated_at: Set(b.updated_at),
    }
}

// ---------------------------------------------------------------------------
// Version and documents
// ---------------------------------------------------------------------------

/// Stored version -> domain header (`documents` empty).
///
/// # Errors
///
/// `Database` for an unknown state code.
pub fn version_header_from_model(m: &bundle_version::Model) -> Result<BundleVersion, RepoError> {
    let state = VersionState::from_code(m.state).ok_or_else(|| {
        corrupt(
            "policy_engine__bundle_version",
            m.id,
            format_args!("unknown state code {}", m.state),
        )
    })?;
    Ok(BundleVersion {
        id: VersionId(m.id),
        bundle_id: BundleId(m.bundle_id),
        owner_tenant_id: m.owner_tenant_id,
        ordinal: m.ordinal,
        state,
        created_at: m.created_at,
        activated_at: m.activated_at,
        activated_by: m.activated_by,
        documents: Vec::new(),
    })
}

/// Domain version -> insertable row (the version row only).
#[must_use]
pub fn version_active_model(v: &BundleVersion) -> bundle_version::ActiveModel {
    bundle_version::ActiveModel {
        id: Set(v.id.0),
        bundle_id: Set(v.bundle_id.0),
        owner_tenant_id: Set(v.owner_tenant_id),
        ordinal: Set(v.ordinal),
        state: Set(v.state.code()),
        created_at: Set(v.created_at),
        activated_at: Set(v.activated_at),
        activated_by: Set(v.activated_by),
    }
}

/// Stored document -> domain.
///
/// # Errors
///
/// `Database` when `resource_types` or `actions` is not a JSON array of text.
pub fn document_from_model(m: &document::Model) -> Result<Document, RepoError> {
    let strings = |column: &str, json: &Value| {
        serde_json::from_value::<Vec<String>>(json.clone()).map_err(|_| {
            corrupt(
                "policy_engine__document",
                m.id,
                format_args!("{column} is not a JSON array of text"),
            )
        })
    };
    Ok(Document {
        id: DocumentId(m.id),
        name: m.name.clone(),
        content: m.content.clone(),
        resource_types: strings("resource_types", &m.resource_types)?,
        actions: strings("actions", &m.actions)?,
    })
}

/// Domain document -> insertable row.
#[must_use]
pub fn document_active_model(
    version_id: VersionId,
    owner_tenant_id: Uuid,
    d: &Document,
) -> document::ActiveModel {
    document::ActiveModel {
        id: Set(d.id.0),
        version_id: Set(version_id.0),
        owner_tenant_id: Set(owner_tenant_id),
        name: Set(d.name.clone()),
        content: Set(d.content.clone()),
        resource_types: Set(serde_json::json!(d.resource_types)),
        actions: Set(serde_json::json!(d.actions)),
    }
}

// ---------------------------------------------------------------------------
// Assignment
// ---------------------------------------------------------------------------

/// Stored assignment -> domain.
#[must_use]
pub fn assignment_from_model(m: &assignment::Model) -> Assignment {
    Assignment {
        id: AssignmentId(m.id),
        bundle_id: BundleId(m.bundle_id),
        tenant_id: m.tenant_id,
        owner_tenant_id: m.owner_tenant_id,
        enforce: m.enforce,
        created_at: m.created_at,
        updated_at: m.updated_at,
    }
}

/// Domain assignment -> insertable row.
#[must_use]
pub fn assignment_active_model(a: &Assignment) -> assignment::ActiveModel {
    assignment::ActiveModel {
        id: Set(a.id.0),
        bundle_id: Set(a.bundle_id.0),
        tenant_id: Set(a.tenant_id),
        owner_tenant_id: Set(a.owner_tenant_id),
        enforce: Set(a.enforce),
        created_at: Set(a.created_at),
        updated_at: Set(a.updated_at),
    }
}
