//! Whether one constraint of a policy scope reaches a subject, decided in
//! memory. The profile read and manage-facts both decide it here.

use toolkit_security::{ScopeConstraint, ScopeFilter, pep_properties};
use uuid::Uuid;

use super::subject_settings::SubjectKey;

/// The filters of `constraint` on other properties when it reaches the subject
/// at `key`, and none when it does not. A constraint reaches the subject only
/// when it names the subject's tenant, so a grant is always bound to it; it may
/// narrow the grant to named subjects with a resource id filter. A filter that
/// cannot be decided in memory (a group or tenant-subtree filter) reaches
/// nothing. The caller decides what the remaining filters grant.
pub fn reach(constraint: &ScopeConstraint, key: SubjectKey) -> Option<Vec<&ScopeFilter>> {
    let mut tenant_named = false;
    let mut rest = Vec::new();
    for filter in constraint.filters() {
        if !filter.is_representable_in_memory() {
            return None;
        }
        let values = filter.values();
        let holds = |id: Uuid| values.iter().any(|value| value.as_uuid() == Some(id));
        match filter.property() {
            pep_properties::OWNER_TENANT_ID if !holds(key.tenant_id) => return None,
            pep_properties::RESOURCE_ID if !holds(key.subject_id) => return None,
            pep_properties::OWNER_TENANT_ID => tenant_named = true,
            pep_properties::RESOURCE_ID => {}
            _ => rest.push(filter),
        }
    }
    tenant_named.then_some(rest)
}
