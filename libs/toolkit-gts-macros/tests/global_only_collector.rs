//! Opting into a local collector must not silently lose legacy declarations.

gts_kit::declare_gts_inventory!();

const GLOBAL: &str = "gts.test.cf.local_fixture.widget.v1~test.cf.local_fixture.global_only.v1";

#[gts_kit::gts_type_schema(
    dir_path = "schemas",
    type_id = "gts.test.cf.local_fixture.widget.v1~",
    description = "Legacy fixture type",
    properties = "id,name",
    base = true
)]
pub struct WidgetV1 {
    pub id: gts_kit::GtsInstanceId,
    pub name: String,
}

gts_kit::gts_instance! {
    WidgetV1 {
        id: "gts.test.cf.local_fixture.widget.v1~test.cf.local_fixture.typed_global.v1",
        name: "typed-global".to_owned(),
    }
}

gts_kit::gts_instance_raw!({
    "id": "gts.test.cf.local_fixture.widget.v1~test.cf.local_fixture.global_only.v1",
    "name": "global",
});

#[test]
fn a_global_only_declaring_crate_cannot_report_an_empty_manifest() {
    assert!(
        gts_kit::inventory::iter::<gts_kit::InventoryInstance>
            .into_iter()
            .any(|entry| entry.instance_id == GLOBAL),
        "legacy consumers still see global declarations"
    );
    let error = std::panic::catch_unwind(gts_declarations)
        .expect_err("a declared collector must diagnose global-only declarations");
    let diagnostic = error
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| error.downcast_ref::<&str>().copied())
        .expect("the rejection has a readable diagnostic");
    assert!(diagnostic.contains("local::"), "{diagnostic}");
    assert!(diagnostic.contains(GLOBAL), "{diagnostic}");
    assert!(diagnostic.contains("typed_global.v1"), "{diagnostic}");
    assert!(
        diagnostic.contains("gts.test.cf.local_fixture.widget.v1~, "),
        "the root schema is diagnosed as well: {diagnostic}"
    );
}
