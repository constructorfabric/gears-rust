//! Every supported ToolKit GTS macro must accept a suffix wrapped in `gts_id!`.

use toolkit_canonical_errors::resource_error;
use toolkit_gts::{GtsInstanceId, gts_id, gts_instance, gts_instance_raw, gts_type_schema};

#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.de0904.tests.type.v1~"),
    description = "DE0904 macro-marker test type",
    properties = "id,name",
    base = true
)]
struct TestTypeV1 {
    id: GtsInstanceId,
    name: String,
}

gts_instance_raw!({
    "id": gts_id!("cf.de0904.tests.type.v1~cf.de0904.tests.raw.v1"),
    "name": "raw",
});

gts_instance! {
    TestTypeV1 {
        id: gts_id!("cf.de0904.tests.type.v1~cf.de0904.tests.typed.v1"),
        name: "typed".to_owned(),
    }
}

#[resource_error(toolkit_canonical_errors::gts_id!("cf.de0904.tests.type.v1~"))]
struct TestResourceError;

fn main() {
    let _ = TestResourceError::not_found("missing test resource");
}
