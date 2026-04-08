// Test file for invalid schema_id in struct_to_gts_schema attributes

use gts::GtsInstanceId;
use gts_macros::struct_to_gts_schema;

#[derive(Debug)]
#[struct_to_gts_schema(
    dir_path = "schemas",
    base = true,
    // Should NOT trigger - valid GTS schema_id string
    schema_id = "gts.x.test.entities.product.v1~",
    description = "Product entity",
    properties = "id"
)]
pub struct ProductV1<P: gts::GtsSchema> {
    pub id: GtsInstanceId,
    pub properties: P,
}

// NOTE: Structs with invalid schema_ids (missing tilde, hyphen, wildcard) were removed.
// gts 0.8.0's struct_to_gts_schema macro now validates schema_ids at compile time,
// rejecting them before the lint can run. String literal checks in main() still
// cover these scenarios at the lint level.

fn main() {
    // Error 1: Incomplete chained segments (missing type component)
    // Should trigger DE0901 - invalid GTS string
    let _id1 = ProductV1::<()>::gts_make_instance_id("vendor.package.sku.abc.v1~a.b.c");

    // Error 2: Incomplete segment (missing type component)
    // Should trigger DE0901 - invalid GTS format
    let _id2 = ProductV1::<()>::gts_make_instance_id("vendor.package.sku.v1");

    // Error 3: Type schema (ends with ~) - gts_make_instance_id must not accept schemas
    // Should trigger DE0901 - invalid GTS entity type (schema instead of instance)
    let _id3 = ProductV1::<()>::gts_make_instance_id("vendor.package.sku.abc.v1~");

    // Error 4: Wildcard - gts_make_instance_id must not accept wildcards
    // Should trigger DE0901 - invalid GTS format
    let _id4 = ProductV1::<()>::gts_make_instance_id("vendor.package.*.abc.v1");

    // Error 5: Multiple segments (contains ~) - gts_make_instance_id must accept only ONE instance segment
    // Should trigger DE0901 - invalid GTS string
    let _id1 = ProductV1::<()>::gts_make_instance_id("vendor.package.sku.abc.v1~a.b.c.d.v1");

    // Error 6: invalid GTS segment
    // Should trigger DE0901 - invalid GTS segment
    let _s = "gts.x.core.lic.feat.v1~x.core.global.base";

    // Error 7: Invalid GTS identifier (no trailing type segment)
    // Should trigger DE0901 - invalid GTS indentifier
    let _s = "gts.x.core.events.type.v1";

    // Error 8: GTS wildcard is not allowed in regular strings
    // Should trigger DE0901 - invalid GTS
    let _s = "gts.x.core.events.type.*";

    // Valid case for comparison
    // Should NOT trigger - valid GTS instance segment
    let _id_valid = ProductV1::<()>::gts_make_instance_id("vendor.package.sku.abc.v1");

    // Use the bad pattern to suppress unused warning
    _use_bad_pattern();
}

// Error 9: GTS wildcard in const without _WILDCARD suffix
// Should trigger DE0901 - invalid GTS wildcard const name (must end with _WILDCARD)
const BAD_PATTERN: &str = "gts.x.core.srr.resource.v1~*";

fn _use_bad_pattern() {
    let _ = BAD_PATTERN;
}
