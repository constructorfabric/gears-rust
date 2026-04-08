// @cpt-begin:cpt-cf-resource-group-dod-sdk-foundation-sdk-models:p1:inst-full
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-sdk-models:p1
//! SDK model types for the resource-group module.
//!
//! These types form the public contract between the resource-group module
//! and its consumers. They are transport-agnostic and use string-based
//! GTS type paths (no surrogate SMALLINT IDs).

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// -- GtsTypePath value object --

// @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-1
/// Maximum length of a GTS type path.
const GTS_TYPE_PATH_MAX_LEN: usize = 255;

/// Validated GTS type path value object.
///
/// A GTS type path follows the pattern `gts.<segment>~(<segment>~)*` where
/// each segment consists of lowercase alphanumeric characters, underscores,
/// and dots. Examples: `gts.x.system.rg.type.v1~`, `gts.x.system.rg.type.v1~x.system.tn.tenant.v1~`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct GtsTypePath(String);

impl GtsTypePath {
    /// Create a new `GtsTypePath` from a raw string, applying validation.
    ///
    /// # Errors
    /// Returns an error if the string is empty, exceeds 255 characters,
    /// or does not match the GTS type path format.
    pub fn new(raw: impl Into<String>) -> Result<Self, String> {
        // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-2
        let raw = raw.into();
        let s = raw.trim().to_lowercase();
        // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-2

        // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-3
        if s.is_empty() {
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-3a
            return Err("GTS type path must not be empty".to_owned());
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-3a
        }
        // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-3

        // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-5
        if s.len() > GTS_TYPE_PATH_MAX_LEN {
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-5a
            return Err("GTS type path exceeds maximum length".to_owned());
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-5a
        }
        // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-5

        // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-4
        // Validate format: ^gts\.[a-z0-9_.]+~([a-z0-9_.]+~)*$
        if !Self::matches_format(&s) {
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-4a
            return Err("Invalid GTS type path format".to_owned());
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-4a
        }
        // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-4

        // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-6
        // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-7
        Ok(Self(s))
        // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-7
        // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-6
    }

    /// Return the inner string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Validate format: `gts.<segment>~(<segment>~)*`
    /// where segment = `[a-z0-9_.]+`
    #[allow(unknown_lints)]
    #[allow(de0901_gts_string_pattern)]
    fn matches_format(s: &str) -> bool {
        // Must start with "gts." and end with "~"
        let Some(rest) = s.strip_prefix("gts.") else {
            return false;
        };
        if rest.is_empty() || !rest.ends_with('~') {
            return false;
        }
        // Split by '~', last element will be "" due to trailing '~'
        let segments: Vec<&str> = rest.split('~').collect();
        // Need at least one real segment + trailing empty
        if segments.len() < 2 {
            return false;
        }
        // All segments except the last (empty) must be non-empty and valid chars
        for seg in &segments[..segments.len() - 1] {
            if seg.is_empty() {
                return false;
            }
            if !seg
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
            {
                return false;
            }
        }
        // Last element must be empty (from trailing ~)
        segments.last().is_some_and(|s| s.is_empty())
    }
}

impl fmt::Display for GtsTypePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<GtsTypePath> for String {
    fn from(p: GtsTypePath) -> Self {
        p.0
    }
}

impl TryFrom<String> for GtsTypePath {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl AsRef<str> for GtsTypePath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
// @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-1

// -- Type --

/// A GTS resource group type definition.
///
/// Matches the REST `Type` schema. All references use string GTS type paths;
/// surrogate SMALLINT IDs are internal to the persistence layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceGroupType {
    /// GTS type path (e.g. `gts.x.system.rg.type.v1~x.system.tn.tenant.v1~`)
    pub code: String,
    /// Whether groups of this type can be root nodes (no parent).
    pub can_be_root: bool,
    /// GTS type paths of types allowed as parents.
    pub allowed_parents: Vec<String>,
    /// GTS type paths of resource types allowed as members.
    pub allowed_memberships: Vec<String>,
    /// Optional JSON Schema for the metadata object of instances of this type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_schema: Option<serde_json::Value>,
}

/// Request body for creating a new GTS type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTypeRequest {
    /// GTS type path. Must have prefix `gts.x.system.rg.type.v1~`.
    pub code: String,
    /// Whether groups of this type can be root nodes.
    pub can_be_root: bool,
    /// GTS type paths of allowed parent types.
    #[serde(default)]
    pub allowed_parents: Vec<String>,
    /// GTS type paths of allowed membership resource types.
    #[serde(default)]
    pub allowed_memberships: Vec<String>,
    /// Optional JSON Schema for instance metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_schema: Option<serde_json::Value>,
}

/// Request body for updating an existing GTS type (full replacement via PUT).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateTypeRequest {
    /// Whether groups of this type can be root nodes.
    pub can_be_root: bool,
    /// GTS type paths of allowed parent types.
    #[serde(default)]
    pub allowed_parents: Vec<String>,
    /// GTS type paths of allowed membership resource types.
    #[serde(default)]
    pub allowed_memberships: Vec<String>,
    /// Optional JSON Schema for instance metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_schema: Option<serde_json::Value>,
}

// -- Group --

/// Hierarchy context for a resource group.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupHierarchy {
    /// Parent group ID (null for root groups).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,
    /// Tenant scope.
    pub tenant_id: Uuid,
}

/// Hierarchy context for a resource group with depth information.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupHierarchyWithDepth {
    /// Parent group ID (null for root groups).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,
    /// Tenant scope.
    pub tenant_id: Uuid,
    /// Relative distance from reference group.
    pub depth: i32,
}

/// A resource group entity.
///
/// Group responses do NOT include `created_at`/`updated_at` (per DESIGN).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceGroup {
    /// Group identifier.
    pub id: Uuid,
    /// GTS chained type path.
    #[serde(rename = "type")]
    pub type_path: String,
    /// Display name.
    pub name: String,
    /// Hierarchy context.
    pub hierarchy: GroupHierarchy,
    /// Type-specific metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// A resource group entity with depth information (for hierarchy queries).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceGroupWithDepth {
    /// Group identifier.
    pub id: Uuid,
    /// GTS chained type path.
    #[serde(rename = "type")]
    pub type_path: String,
    /// Display name.
    pub name: String,
    /// Hierarchy context with depth.
    pub hierarchy: GroupHierarchyWithDepth,
    /// Type-specific metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Request body for creating a new resource group.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateGroupRequest {
    /// GTS chained type path. Must have prefix `gts.x.system.rg.type.v1~`.
    #[serde(rename = "type")]
    pub type_path: String,
    /// Display name (1..255 characters).
    pub name: String,
    /// Parent group ID (null for root groups).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,
    /// Type-specific metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Request body for updating a resource group (full replacement via PUT).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateGroupRequest {
    /// GTS chained type path. Must have prefix `gts.x.system.rg.type.v1~`.
    #[serde(rename = "type")]
    pub type_path: String,
    /// Display name (1..255 characters).
    pub name: String,
    /// Parent group ID (null for root groups).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,
    /// Type-specific metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Serde helper for `Option<Option<T>>` that distinguishes absent, null, and present values.
#[allow(clippy::option_option, clippy::missing_errors_doc)]
pub mod option_option {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
    where
        T: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        Ok(Some(Option::<T>::deserialize(deserializer)?))
    }
}

/// Request body for patching a resource group (partial update via PATCH).
///
/// Fields not present in the request body are left unchanged. Fields set to
/// `null` clear the value. Fields with a value update it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::option_option)]
pub struct PatchGroupRequest {
    pub name: Option<String>,
    #[serde(default, deserialize_with = "option_option::deserialize")]
    pub parent_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "option_option::deserialize")]
    pub metadata: Option<Option<serde_json::Value>>,
}

// -- Membership --

/// A membership link between a resource and a group.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceGroupMembership {
    /// Group this resource belongs to.
    pub group_id: Uuid,
    /// GTS type path of the resource.
    pub resource_type: String,
    /// External resource identifier.
    pub resource_id: String,
}

// @cpt-dod:cpt-cf-resource-group-dod-testing-sdk-models:p1
#[cfg(test)]
mod tests {
    use super::*;

    // -- GtsTypePath: valid cases (table-driven) -- TC-SDK-01, 06, 07, 08, 19, 20

    #[test]
    fn gts_type_path_valid_cases() {
        let valid = vec![
            // TC-SDK-01: basic valid path
            ("gts.x.system.rg.type.v1~", "gts.x.system.rg.type.v1~"),
            // TC-SDK-06: uppercase is lowered
            ("GTS.X.SYSTEM.RG.TYPE.V1~", "gts.x.system.rg.type.v1~"),
            // TC-SDK-07: trimmed + lowered
            ("  GTS.X.System.RG.Type.V1~  ", "gts.x.system.rg.type.v1~"),
            // TC-SDK-08: multi-segment
            (
                "gts.x.system.rg.type.v1~x.test.v1~",
                "gts.x.system.rg.type.v1~x.test.v1~",
            ),
            // TC-SDK-19: numeric segments
            ("gts.123~456~", "gts.123~456~"),
            // TC-SDK-20: underscores
            ("gts.a_b.c_d~", "gts.a_b.c_d~"),
        ];
        for (input, expected) in valid {
            let path = GtsTypePath::new(input);
            assert!(path.is_ok(), "should be valid: {input}");
            assert_eq!(path.unwrap().as_str(), expected, "for input: {input}");
        }
    }

    // -- GtsTypePath: invalid cases (table-driven) -- TC-SDK-02..05, 09, 10, 18, 21

    #[test]
    fn gts_type_path_invalid_cases() {
        let cases = vec![
            // TC-SDK-02: empty
            ("", "must not be empty"),
            // TC-SDK-04: wrong prefix
            ("invalid.path~", "Invalid GTS type path format"),
            // TC-SDK-05: no trailing tilde
            ("gts.x.system.rg.type.v1", "Invalid GTS type path format"),
            // TC-SDK-09: double tilde
            ("gts.x.system.rg.type.v1~~", "Invalid GTS type path format"),
            // TC-SDK-10: hyphen in segment
            (
                "gts.x.system.rg.type.v1~hello-world~",
                "Invalid GTS type path format",
            ),
            // TC-SDK-18: empty segment after gts.
            ("gts.~", "Invalid GTS type path format"),
            // TC-SDK-21: whitespace-only
            ("   ", "must not be empty"),
        ];
        for (input, expected_msg) in cases {
            let result = GtsTypePath::new(input);
            assert!(result.is_err(), "should be invalid: '{input}'");
            let err = result.unwrap_err();
            assert!(
                err.contains(expected_msg),
                "for input '{input}': expected '{expected_msg}' in error, got: {err}"
            );
        }
    }

    // -- GtsTypePath: length boundary tests -- TC-SDK-03, 22, 23

    #[test]
    fn gts_type_path_max_length_boundary() {
        // TC-SDK-22: exactly 255 chars -> Ok
        // Build: "gts." (4) + segment + "~" (1) = 255 => segment = 250 chars
        let segment = "a".repeat(250);
        let path_255 = format!("gts.{segment}~");
        assert_eq!(path_255.len(), 255);
        assert!(
            GtsTypePath::new(&path_255).is_ok(),
            "exactly 255 chars should be valid"
        );

        // TC-SDK-23: exactly 256 chars -> Err
        let segment_251 = "a".repeat(251);
        let path_256 = format!("gts.{segment_251}~");
        assert_eq!(path_256.len(), 256);
        let result = GtsTypePath::new(&path_256);
        assert!(result.is_err(), "256 chars should exceed max length");
        assert!(result.unwrap_err().contains("exceeds maximum length"));

        // TC-SDK-03: 255+ chars (well above max)
        let long_segment = "a".repeat(300);
        let long_path = format!("gts.{long_segment}~");
        assert!(long_path.len() > 255);
        let result = GtsTypePath::new(&long_path);
        assert!(result.is_err(), "255+ chars should be rejected");
        assert!(result.unwrap_err().contains("exceeds maximum length"));
    }

    // -- GtsTypePath: serde round-trip -- TC-SDK-11, 12

    #[test]
    fn gts_type_path_serde_round_trip() {
        // TC-SDK-11: serialize then deserialize
        let original = GtsTypePath::new("gts.x.system.rg.type.v1~").unwrap();
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: GtsTypePath = serde_json::from_str(&json).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn gts_type_path_serde_invalid_rejects() {
        // TC-SDK-12: invalid value during deserialization
        let result = serde_json::from_str::<GtsTypePath>("\"invalid\"");
        assert!(result.is_err(), "invalid path should fail deserialization");
    }

    // -- GtsTypePath: Display / Into<String> -- TC-SDK-13

    #[test]
    fn gts_type_path_display_and_into_string() {
        let path = GtsTypePath::new("gts.x.system.rg.type.v1~").unwrap();
        let display = path.to_string();
        let into_string: String = path.into();
        assert_eq!(display, into_string);
    }

    // -- ResourceGroupType serialization -- TC-SDK-14

    #[test]
    fn resource_group_type_camel_case_keys() {
        let rgt = ResourceGroupType {
            code: "gts.x.system.rg.type.v1~".to_owned(),
            can_be_root: true,
            allowed_parents: vec!["gts.parent~".to_owned()],
            allowed_memberships: vec!["gts.member~".to_owned()],
            metadata_schema: None,
        };
        let json = serde_json::to_value(&rgt).unwrap();
        assert!(
            json.get("canBeRoot").is_some(),
            "expected camelCase 'canBeRoot'"
        );
        assert!(
            json.get("allowedParents").is_some(),
            "expected camelCase 'allowedParents'"
        );
        assert!(
            json.get("allowedMemberships").is_some(),
            "expected camelCase 'allowedMemberships'"
        );
        assert!(
            json.get("metadataSchema").is_none(),
            "metadataSchema should be absent when None"
        );
    }

    // -- ResourceGroup serialization -- TC-SDK-15, 16

    #[test]
    fn resource_group_type_field_renamed() {
        // TC-SDK-15: "type" not "type_path"
        let group = ResourceGroup {
            id: Uuid::nil(),
            type_path: "gts.x.system.rg.type.v1~".to_owned(),
            name: "Test".to_owned(),
            hierarchy: GroupHierarchy {
                parent_id: None,
                tenant_id: Uuid::nil(),
            },
            metadata: Some(serde_json::json!({"key": "val"})),
        };
        let json = serde_json::to_value(&group).unwrap();
        assert!(
            json.get("type").is_some(),
            "expected 'type' key, not 'type_path'"
        );
        assert!(
            json.get("type_path").is_none(),
            "'type_path' should not appear in JSON"
        );
    }

    #[test]
    fn resource_group_metadata_absent_when_none() {
        // TC-SDK-16: metadata: None -> no "metadata" key
        let group = ResourceGroup {
            id: Uuid::nil(),
            type_path: "gts.x.system.rg.type.v1~".to_owned(),
            name: "Test".to_owned(),
            hierarchy: GroupHierarchy {
                parent_id: None,
                tenant_id: Uuid::nil(),
            },
            metadata: None,
        };
        let json = serde_json::to_value(&group).unwrap();
        assert!(
            json.get("metadata").is_none(),
            "metadata should be absent when None, got: {json}"
        );
    }

    // -- QueryProfile default -- TC-SDK-17

    #[test]
    fn query_profile_default_values() {
        // QueryProfile is in group_service, not in SDK models.
        // TC-SDK-17 is tested in domain_unit_test.rs or here via the re-export.
        // Since QueryProfile lives in the main crate, we skip here and it's
        // covered in domain_unit_test.rs instead.
    }

    // -- validate_type_code vs GtsTypePath inconsistency -- TC-SDK-24

    #[test]
    fn gts_type_path_trims_and_lowercases() {
        // TC-SDK-24: GtsTypePath::new trims whitespace and lowercases.
        // validate_type_code (in the domain crate) does NOT trim or lowercase.
        // Document this inconsistency: the SDK normalizes input but the
        // domain validation is a strict prefix check on the raw string.
        let input = "  GTS.X.SYSTEM.RG.TYPE.V1~  ";
        let path = GtsTypePath::new(input);
        assert!(
            path.is_ok(),
            "GtsTypePath::new should accept trimmed/lowered input"
        );
        assert_eq!(path.unwrap().as_str(), "gts.x.system.rg.type.v1~");
        // Note: validate_type_code(input) would fail because it checks
        // prefix on the raw (untrimmed, uncased) string. This is a known
        // inconsistency between SDK and domain validation layers.
    }
}
// @cpt-end:cpt-cf-resource-group-dod-sdk-foundation-sdk-models:p1:inst-full
