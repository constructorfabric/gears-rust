use super::*;

#[test]
fn usage_record_resource_type_name_is_pinned_wire_string() {
    // Pin the literal on-the-wire identifier rather than comparing two
    // constants that move together. Renaming or version-bumping the GTS id is
    // a wire-breaking change for PDP policies and storage-plugin routing —
    // this test must fail loudly when that happens.
    assert_eq!(USAGE_RECORD.name, "gts.cf.core.usage.record.v1~");
    assert_eq!(USAGE_RECORD_GTS, "gts.cf.core.usage.record.v1~");
}

#[test]
fn usage_record_supported_properties_invariants() {
    let props = USAGE_RECORD.supported_properties;
    assert!(
        props.contains(&pep_properties::OWNER_TENANT_ID),
        "OWNER_TENANT_ID must be advertised so PDP can constrain by tenant"
    );
    for expected in [
        properties::RESOURCE_ID,
        properties::RESOURCE_TYPE,
        properties::MODULE,
        properties::SUBJECT_ID,
        properties::SUBJECT_TYPE,
    ] {
        assert!(
            props.contains(&expected),
            "supported_properties must contain {expected} so the storage \
             plugin can translate PDP constraints over it"
        );
    }
    assert_eq!(
        props.len(),
        6,
        "supported_properties surface changed - update wire contract and \
         storage-plugin scope_to_sql accordingly"
    );
}
