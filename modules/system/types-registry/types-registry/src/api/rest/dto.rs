//! REST DTOs for the Types Registry module.

use uuid::Uuid;

use gts::GtsIdSegment;
use types_registry_sdk::{GtsEntity, RegisterResult, RegisterSummary, SegmentMatchScope};

/// DTO for a GTS ID segment.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request, response)]
pub struct GtsIdSegmentDto {
    /// Vendor component of the segment.
    pub vendor: String,
    /// Package component of the segment.
    pub package: String,
    /// Namespace component of the segment.
    pub namespace: String,
    /// Type name component of the segment.
    pub type_name: String,
    /// Major version number.
    pub ver_major: u32,
}

impl From<&GtsIdSegment> for GtsIdSegmentDto {
    fn from(segment: &GtsIdSegment) -> Self {
        Self {
            vendor: segment.vendor.clone(),
            package: segment.package.clone(),
            namespace: segment.namespace.clone(),
            type_name: segment.type_name.clone(),
            ver_major: segment.ver_major,
        }
    }
}

/// Response DTO for a GTS entity.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request, response)]
pub struct GtsEntityDto {
    /// Deterministic UUID generated from the GTS ID.
    pub id: Uuid,
    /// The full GTS identifier string.
    pub gts_id: String,
    /// All parsed segments from the GTS ID.
    pub segments: Vec<GtsIdSegmentDto>,
    /// Whether this entity is a schema (type definition).
    ///
    /// - `true`: This is a type definition (GTS ID ends with `~`)
    /// - `false`: This is an instance (GTS ID does not end with `~`)
    pub is_schema: bool,
    /// The entity content (schema for types, object for instances).
    pub content: serde_json::Value,
    /// Optional description of the entity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl From<GtsEntity> for GtsEntityDto {
    fn from(entity: GtsEntity) -> Self {
        Self {
            id: entity.id,
            gts_id: entity.gts_id.clone(),
            segments: entity.segments.iter().map(GtsIdSegmentDto::from).collect(),
            is_schema: entity.is_schema,
            content: entity.content.clone(),
            description: entity.description.clone(),
        }
    }
}

/// Request DTO for registering GTS entities.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request)]
pub struct RegisterEntitiesRequest {
    /// Array of GTS entities to register.
    pub entities: Vec<serde_json::Value>,
}

/// Result of registering a single entity.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request, response)]
#[serde(tag = "status")]
pub enum RegisterResultDto {
    /// Successfully registered entity.
    #[serde(rename = "ok")]
    Ok {
        /// The registered entity.
        entity: GtsEntityDto,
    },
    /// Failed to register entity.
    #[serde(rename = "error")]
    Error {
        /// The GTS ID that was attempted, if available.
        #[serde(skip_serializing_if = "Option::is_none")]
        gts_id: Option<String>,
        /// Error message.
        error: String,
    },
}

impl From<RegisterResult> for RegisterResultDto {
    fn from(result: RegisterResult) -> Self {
        match result {
            RegisterResult::Ok(entity) => Self::Ok {
                entity: entity.into(),
            },
            RegisterResult::Err { gts_id, error } => Self::Error {
                gts_id,
                error: error.to_string(),
            },
        }
    }
}

/// Response DTO for batch registration.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(response)]
pub struct RegisterEntitiesResponse {
    /// Summary of the registration operation.
    pub summary: RegisterSummaryDto,
    /// Results for each entity in the request.
    pub results: Vec<RegisterResultDto>,
}

/// Summary of a batch registration operation.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request, response)]
pub struct RegisterSummaryDto {
    /// Total number of entities processed.
    pub total: usize,
    /// Number of successfully registered entities.
    pub succeeded: usize,
    /// Number of failed registrations.
    pub failed: usize,
}

impl From<RegisterSummary> for RegisterSummaryDto {
    fn from(summary: RegisterSummary) -> Self {
        Self {
            total: summary.total(),
            succeeded: summary.succeeded,
            failed: summary.failed,
        }
    }
}

/// Query parameters for listing GTS entities.
#[derive(Debug, Clone, Default)]
#[modkit_macros::api_dto(request)]
pub struct ListEntitiesQuery {
    /// Optional wildcard pattern for GTS ID matching.
    #[serde(default)]
    pub pattern: Option<String>,
    /// Filter by schema type: true for types, false for instances.
    #[serde(default)]
    pub is_schema: Option<bool>,
    /// Filter by vendor.
    #[serde(default)]
    pub vendor: Option<String>,
    /// Filter by package.
    #[serde(default)]
    pub package: Option<String>,
    /// Filter by namespace.
    #[serde(default)]
    pub namespace: Option<String>,
    /// Segment match scope: "primary" or "any" (default).
    #[serde(default)]
    pub segment_scope: Option<String>,
}

impl ListEntitiesQuery {
    /// Converts this DTO to the SDK `ListQuery`.
    #[must_use]
    pub fn to_list_query(&self) -> types_registry_sdk::ListQuery {
        let mut query = types_registry_sdk::ListQuery::default();

        if let Some(ref pattern) = self.pattern {
            query = query.with_pattern(pattern);
        }

        if let Some(is_schema) = self.is_schema {
            query = query.with_is_type(is_schema);
        }

        if let Some(ref vendor) = self.vendor {
            query = query.with_vendor(vendor);
        }

        if let Some(ref package) = self.package {
            query = query.with_package(package);
        }

        if let Some(ref namespace) = self.namespace {
            query = query.with_namespace(namespace);
        }

        if let Some(ref scope) = self.segment_scope {
            match scope.as_str() {
                "primary" => query = query.with_segment_scope(SegmentMatchScope::Primary),
                "any" => query = query.with_segment_scope(SegmentMatchScope::Any),
                _ => {}
            }
        }

        query
    }
}

/// Response DTO for listing GTS entities.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(response)]
pub struct ListEntitiesResponse {
    /// The list of entities.
    pub entities: Vec<GtsEntityDto>,
    /// Total count of entities returned.
    pub count: usize,
}
#[cfg(test)]
#[path = "dto_tests.rs"]
mod tests;
