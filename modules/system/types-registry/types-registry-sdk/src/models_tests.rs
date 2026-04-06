use super::*;

#[test]
fn test_gts_id_segment_from_gts_rust() {
    // GtsIdSegment::new(num, offset, segment_str) parses a GTS segment string
    let segment = GtsIdSegment::new(0, 0, "acme.core.events.user_created.v1~").unwrap();
    assert_eq!(segment.vendor, "acme");
    assert_eq!(segment.package, "core");
    assert_eq!(segment.namespace, "events");
    assert_eq!(segment.type_name, "user_created");
    assert_eq!(segment.ver_major, 1);
    assert!(segment.is_type);
}

#[test]
fn test_gts_entity_accessors() {
    let segment = GtsIdSegment::new(0, 0, "acme.core.events.user_created.v1~").unwrap();
    let entity = GtsEntity::new(
        Uuid::nil(),
        "gts.acme.core.events.user_created.v1~",
        vec![segment],
        true, // is_schema
        serde_json::json!({"type": "object"}),
        Some("A user created event".to_owned()),
    );

    assert!(entity.is_type());
    assert!(!entity.is_instance());
    assert_eq!(entity.vendor(), Some("acme"));
    assert_eq!(entity.package(), Some("core"));
    assert_eq!(entity.namespace(), Some("events"));

    // Test instance
    let instance = GtsEntity::new(
        Uuid::nil(),
        "gts.acme.core.events.user_created.v1~acme.core.instances.instance1.v1",
        vec![],
        false, // is_schema
        serde_json::json!({"data": "value"}),
        None,
    );
    assert!(!instance.is_type());
    assert!(instance.is_instance());
}

#[test]
fn test_list_query_builder() {
    let query = ListQuery::new()
        .with_pattern("gts.acme.*")
        .with_is_type(true)
        .with_vendor("acme")
        .with_package("core")
        .with_namespace("events");

    assert_eq!(query.pattern, Some("gts.acme.*".to_owned()));
    assert_eq!(query.is_type, Some(true));
    assert_eq!(query.vendor, Some("acme".to_owned()));
    assert_eq!(query.package, Some("core".to_owned()));
    assert_eq!(query.namespace, Some("events".to_owned()));
    assert_eq!(query.segment_scope, SegmentMatchScope::Any);
    assert!(!query.is_empty());
}

#[test]
fn test_list_query_empty() {
    let query = ListQuery::default();
    assert!(query.is_empty());
    assert_eq!(query.segment_scope, SegmentMatchScope::Any);
}

#[test]
fn test_segment_match_scope() {
    assert!(SegmentMatchScope::Primary.is_primary());
    assert!(!SegmentMatchScope::Primary.is_any());
    assert!(SegmentMatchScope::Any.is_any());
    assert!(!SegmentMatchScope::Any.is_primary());

    // Default is Any
    assert_eq!(SegmentMatchScope::default(), SegmentMatchScope::Any);
}

#[test]
fn test_list_query_with_segment_scope() {
    let query = ListQuery::new()
        .with_vendor("acme")
        .with_segment_scope(SegmentMatchScope::Any);

    assert_eq!(query.vendor, Some("acme".to_owned()));
    assert_eq!(query.segment_scope, SegmentMatchScope::Any);
}
