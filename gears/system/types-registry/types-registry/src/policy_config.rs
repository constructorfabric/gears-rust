//! Deserializable registration-policy configuration.

use serde::Deserialize;

/// One registration-policy entry: either or both parameters (DESIGN §3.2).
///
/// Both are `Option` because *omitting* a parameter is meaningful and distinct
/// from setting it closed: a matching entry that omits one is **skipped** for
/// that parameter, and a less-specific entry may still supply it. Collapsing
/// the absent case onto `[]` / `false` would make a narrow entry silently close
/// what a broader one opened.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct PolicyEntry {
    /// Vendors admitted in the candidate's **last** identifier segment. `["*"]`
    /// admits any vendor. The selected set *replaces* a less-specific one.
    pub allowed_vendors: Option<Vec<String>>,
    /// Whether an entity in this region may be tenant-owned.
    ///
    /// Parsed and validated, **inert in P0**: SPEC §9 fixes every row to
    /// `ownership_scope = 1`, so nothing can be tenant-owned in the first place.
    /// Kept rather than rejected because a P1-ready deployment carries it, and
    /// rejecting a valid configuration would fail a boot for no reason.
    pub tenant_ownable: Option<bool>,
}
