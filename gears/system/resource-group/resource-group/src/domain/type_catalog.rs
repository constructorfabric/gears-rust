// Created: 2026-10-12 by Constructor Tech
//! Port to the catalog of resource-group types.
//!
//! A resource-group type is a GTS Type Schema living in the Types Registry;
//! this gear never stores types. The services read them through this port,
//! always before opening a transaction.

use std::collections::HashMap;

use uuid::Uuid;

use crate::domain::error::DomainError;

/// A resource-group (or membership resource) type as the gear needs it.
#[derive(Debug, Clone)]
pub struct RgTypeDef {
    /// Registry Reference from the Types Registry (`TypeSchema.type_uuid`).
    pub uuid: Uuid,
    /// Canonical GTS id string from the Types Registry (`TypeSchema.type_id`).
    pub code: String,
    /// Whether groups of this type may be roots.
    pub can_be_root: bool,
    /// GTS ids of types allowed as parents.
    pub allowed_parent_types: Vec<String>,
    /// GTS ids of resource types allowed as members.
    pub allowed_membership_types: Vec<String>,
    /// Resolved schema of the type, validated against `{"metadata": ..}`.
    pub schema: Option<serde_json::Value>,
}

/// Types resolved by one [`TypeCatalog::resolve`] call.
///
/// A key missing from a map means the type is unknown to the registry. Every
/// type resolved by code is also present in `by_uuid` under its Registry
/// Reference, so a caller can reuse one answer for later UUID lookups.
#[derive(Debug, Default, Clone)]
pub struct ResolvedTypes {
    /// Types asked by GTS id string, keyed by the asked string.
    pub by_code: HashMap<String, RgTypeDef>,
    /// Types keyed by Registry Reference (asked or learned through a code).
    pub by_uuid: HashMap<Uuid, RgTypeDef>,
}

/// Read access to resource-group types.
#[async_trait::async_trait]
pub trait TypeCatalog: Send + Sync {
    /// Resolve GTS id strings and Registry References together, in one
    /// registry request per 100 distinct keys. A missing key means the type
    /// is unknown to the registry (not an error); a code that is not a valid
    /// GTS type id is absent. Empty input makes no registry call.
    ///
    /// # Errors
    ///
    /// [`DomainError::TypesRegistryUnavailable`] when the registry fails.
    async fn resolve(&self, codes: &[String], uuids: &[Uuid])
    -> Result<ResolvedTypes, DomainError>;
}
