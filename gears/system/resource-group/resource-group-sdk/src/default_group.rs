//! Stable identity shared by default-group producers and consumers.

use uuid::Uuid;

/// Derive the canonical default-group ID for a tenant.
///
/// This namespace/name pair is a persisted identity contract: changing it would
/// orphan existing references. The `UUIDv5` namespace is the tenant's raw UUID,
/// and the name is qualified by object kind to avoid unrelated default IDs.
/// This helper confers no permission to create the group; use the authenticated
/// [`crate::ResourceGroupDefaults`] capability for that operation.
#[must_use]
pub fn default_group_id(tenant_id: Uuid) -> Uuid {
    Uuid::new_v5(&tenant_id, b"resource-group:default")
}

#[cfg(test)]
mod tests {
    use super::default_group_id;
    use uuid::{Uuid, uuid};

    #[test]
    fn default_identity_preserves_existing_consumer_vectors() {
        // Literal vectors computed independently with Python's uuid.uuid5.
        let cases: [(Uuid, Uuid); 3] = [
            (
                uuid!("00000000-0000-0000-0000-000000000001"),
                uuid!("4eba9605-3259-53ac-94d6-bedd2bc5847b"),
            ),
            (
                uuid!("11111111-2222-3333-4444-555555555555"),
                uuid!("60e5319c-9d5f-550d-bdff-0df9423765df"),
            ),
            (
                uuid!("abcdefab-cdef-abcd-efab-cdefabcdefab"),
                uuid!("c2504789-c33b-51ed-9e3f-ef44067e92c6"),
            ),
        ];
        for (tenant, expected) in cases {
            assert_eq!(default_group_id(tenant), expected);
        }
    }
}
