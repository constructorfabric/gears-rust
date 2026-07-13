// Test file for valid GTS strings and gts-macros annotations - should not trigger DE0901

use gts::{GtsIdPattern, GtsInstanceId};
use gts_macros::struct_to_gts_schema;
use toolkit_gts::{GTS_ID_PREFIX, gts_id};

#[derive(Debug)]
#[struct_to_gts_schema(
    dir_path = "schemas",
    base = true,
    // Should NOT trigger DE0901 - valid GTS schema_id string
    type_id = gts_id!("example.core.events.topic.v1~"),
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
    type_id = gts_id!("example.core.events.type.v1~"),
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
    type_id = gts_id!("example.core.events.type.v1~cf.core.audit.event.v1~"),
    description = "Audit event",
    properties = "user_id"
)]
pub struct AuditEventV1 {
    pub user_id: String,
}

// Should NOT trigger DE0901 - wildcard const has _WILDCARD suffix
fn srr_wildcard() -> String {
    format!("{GTS_ID_PREFIX}example.core.srr.resource.v1~*")
}

fn main() {
    // Should NOT trigger DE0901 - valid GTS instance segment
    let _id = EventTopicV1::<()>::gts_make_instance_id("example.commerce.orders.orders.v1.0");

    // Should NOT trigger DE0901 - valid GTS type schema string
    let _s1 = gts_id!("example.core.events.type.v1~");

    // Should NOT trigger DE0901 - valid GTS type schema string with inheritance
    let _s2 = gts_id!("example.core.events.type.v1~cf.core.audit.event.v1~");

    // Should NOT trigger DE0901 - strings inside starts_with() should be ignored
    let _check = "some.invalid.gts.string".starts_with(GTS_ID_PREFIX);
    // Should NOT trigger DE0901 - strings inside starts_with() should be ignored
    let _check2 =
        "another.invalid.gts.string".starts_with(&format!("{GTS_ID_PREFIX}example.core."));

    // Should NOT trigger DE0901 - GtsIdPattern::try_new() accepts wildcard patterns
    let _wc1 = GtsIdPattern::try_new(&format!("{GTS_ID_PREFIX}example.core.srr.resource.v1~*"));

    // Should NOT trigger DE0901 - GtsIdPattern::try_new() accepts wildcard with sub-prefix
    let _wc2 = GtsIdPattern::try_new(&format!(
        "{GTS_ID_PREFIX}example.core.srr.resource.v1~example.*"
    ));

    // Should NOT trigger DE0901 - gts::GtsIdPattern::try_new() qualified path form
    let _wc3 = gts::GtsIdPattern::try_new(&format!("{GTS_ID_PREFIX}example.core.events.type.v1~*"));

    // Should NOT trigger DE0901 - const holding wildcard used with GtsIdPattern::try_new()
    let _wc4 = GtsIdPattern::try_new(&srr_wildcard());
}

// Vendor checks are skipped in #[cfg(test)] modules - any vendor is allowed
#[cfg(test)]
mod test_vendors_allowed {
    use toolkit_gts::gts_id;

    // Should NOT trigger DE0901 - vendor checks are skipped in test code
    fn _test_acme_vendor() {
        let _s = gts_id!("acme.core.events.type.v1~");
    }
}
