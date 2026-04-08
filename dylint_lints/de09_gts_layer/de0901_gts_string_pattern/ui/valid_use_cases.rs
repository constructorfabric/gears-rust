// Test file for valid GTS strings and gts-macros annotations - should not trigger DE0901

use gts::{GtsInstanceId, GtsWildcard};
use gts_macros::struct_to_gts_schema;

#[derive(Debug)]
#[struct_to_gts_schema(
    dir_path = "schemas",
    base = true,
    // Should NOT trigger DE0901 - valid GTS schema_id string
    schema_id = "gts.x.core.events.topic.v1~",
    description = "Event Topic definition",
    properties = "id,name"
)]
pub struct EventTopicV1<T: gts::GtsSchema> {
    pub id: GtsInstanceId,
    pub name: String,
    pub properties: T,
}

#[derive(Debug)]
#[struct_to_gts_schema(
    dir_path = "schemas",
    base = true,
    // Should NOT trigger DE0901- valid GTS schema_id string
    schema_id = "gts.x.core.events.type.v1~",
    description = "Base event type definition",
    properties = "id"
)]
pub struct BaseEventTypeV1<P: gts::GtsSchema> {
    pub id: GtsInstanceId,
    pub properties: P,
}

#[derive(Debug)]
#[struct_to_gts_schema(
    dir_path = "schemas",
    base = BaseEventTypeV1,
    // Should NOT trigger DE0901 - valid GTS schema_id string with inheritance
    schema_id = "gts.x.core.events.type.v1~x.core.audit.event.v1~",
    description = "Audit event",
    properties = "user_id"
)]
pub struct AuditEventV1 {
    pub user_id: String,
}

// Should NOT trigger DE0901 - wildcard const has _WILDCARD suffix
const SRR_WILDCARD: &str = "gts.x.core.srr.resource.v1~*";

fn main() {
    // Should NOT trigger DE0901 - valid GTS instance segment
    let _id = EventTopicV1::<()>::gts_make_instance_id("x.commerce.orders.orders.v1.0");

    // Should NOT trigger DE0901 - valid GTS type schema string
    let _s1 = "gts.x.core.events.type.v1~";

    // Should NOT trigger DE0901 - valid GTS type schema string with inheritance
    let _s2 = "gts.x.core.events.type.v1~x.core.audit.event.v1~";

    // Should NOT trigger DE0901 - strings inside starts_with() should be ignored
    let _check = "some.invalid.gts.string".starts_with("gts.");
    // Should NOT trigger DE0901 - strings inside starts_with() should be ignored
    let _check2 = "another.invalid.gts.string".starts_with("gts.x.core.");

    // Should NOT trigger DE0901 - GtsWildcard::new() accepts wildcard patterns
    let _wc1 = GtsWildcard::new("gts.x.core.srr.resource.v1~*");

    // Should NOT trigger DE0901 - GtsWildcard::new() accepts wildcard with sub-prefix
    let _wc2 = GtsWildcard::new("gts.x.core.srr.resource.v1~acme.*");

    // Should NOT trigger DE0901 - gts::GtsWildcard::new() qualified path form
    let _wc3 = gts::GtsWildcard::new("gts.x.core.events.type.v1~*");

    // Should NOT trigger DE0901 - const holding wildcard used with GtsWildcard::new()
    let _wc4 = GtsWildcard::new(SRR_WILDCARD);
}
