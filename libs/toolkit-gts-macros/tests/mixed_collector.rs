//! A non-empty local manifest must not conceal omitted legacy declarations.

gts_kit::declare_gts_inventory!();

gts_kit::local::gts_instance_raw!({
    "id": "gts.test.cf.local_fixture.widget.v1~test.cf.local_fixture.local.v1",
    "name": "local",
});

gts_kit::gts_instance_raw!({
    "id": "gts.test.cf.local_fixture.widget.v1~test.cf.local_fixture.legacy.v1",
    "name": "legacy",
});

#[test]
fn a_mixed_declaring_crate_cannot_report_a_partial_manifest() {
    let error = std::panic::catch_unwind(gts_declarations)
        .expect_err("a non-empty local collector must diagnose the omitted declaration");
    let diagnostic = error
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| error.downcast_ref::<&str>().copied())
        .expect("the rejection has a readable diagnostic");
    assert!(diagnostic.contains("local::"), "{diagnostic}");
    assert!(diagnostic.contains("legacy.v1"), "{diagnostic}");
}
