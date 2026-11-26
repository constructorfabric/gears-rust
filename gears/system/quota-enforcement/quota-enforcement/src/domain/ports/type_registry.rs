//! Output port for reads of metric and subject types from the platform
//! `types-registry` (ADR-0012). Its only production implementation is
//! `infra::types_registry::PlatformTypeRegistry`.

use std::collections::HashMap;

use async_trait::async_trait;
use gts::{GtsTypeId, PayloadValidator};
use serde_json::Value;
use toolkit_macros::domain_model;

use crate::domain::error::DomainError;

/// The registry's freshness token of a type as QE read it: a conditional read
/// with it returns a document only when the type changed.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeEtag(pub Vec<u8>);

/// A registered type as the registry serves it now.
#[domain_model]
#[derive(Debug, Clone)]
pub struct RegisteredType {
    /// The type id.
    pub id: GtsTypeId,
    /// `x-gts-abstract: true` on the type's own schema.
    pub is_abstract: bool,
    /// Chain-merged trait values. `Value::Null` when the chain declares none.
    pub effective_traits: Value,
    /// The resolved schema.
    pub schema: Value,
    /// The payload validator compiled from the resolved schema, or why there
    /// is none (an abstract type, a schema that does not compile).
    pub payload: Result<PayloadValidator, String>,
    /// The token of this content.
    pub etag: TypeEtag,
}

/// The registry's answer for one type.
#[domain_model]
#[derive(Debug, Clone)]
pub enum TypeRead {
    /// The type's current content.
    Found(Box<RegisteredType>),
    /// The type did not change since the token the read carried.
    Unchanged,
    /// The registry does not know the type, or holds it as deleted.
    Absent,
}

/// The registry as the catalogue needs it.
#[async_trait]
pub trait TypeRegistry: Send + Sync {
    /// Read `reads` from the registry itself, bypassing any client cache,
    /// each conditional on its token when it carries one, within the
    /// adapter's time limit. Every asked id is answered.
    ///
    /// # Errors
    ///
    /// [`DomainError::TypesRegistryUnavailable`] when the read fails or
    /// reaches its time limit.
    async fn read_types(
        &self,
        reads: Vec<(GtsTypeId, Option<TypeEtag>)>,
    ) -> Result<HashMap<GtsTypeId, TypeRead>, DomainError>;
}
