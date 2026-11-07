//! Crate-local collectors next to the process-wide inventory in one binary:
//! each crate's `gts_declarations()` sees only its own `local` declarations,
//! while the process-wide inventory still sees all of them.

use gts_kit::gts_id;

// This test crate is a second declaring crate.
gts_kit::declare_gts_inventory!();

gts_kit::local::gts_instance_raw!({
    "id": gts_id!("test.cf.local_fixture.widget.v1~test.cf.local_fixture.test_crate.v1"),
    "name": "test-crate",
});

fn ids(declarations: &[(String, String)]) -> Vec<&str> {
    declarations.iter().map(|(id, _)| id.as_str()).collect()
}

use declarations_fixture::DECLARED_IDS as FIXTURE_IDS;

#[test]
fn a_crate_collects_only_its_own_local_declarations() {
    let fixture = declarations_fixture::gts_declarations();
    assert_eq!(
        ids(&fixture),
        FIXTURE_IDS,
        "sorted; no cfg-off or foreign entry"
    );

    let own = gts_declarations();
    assert_eq!(
        ids(&own),
        [gts_id!(
            "test.cf.local_fixture.widget.v1~test.cf.local_fixture.test_crate.v1"
        )]
    );
}

#[test]
fn declarations_are_json_text() {
    let fixture = declarations_fixture::gts_declarations();
    let content = |id: &str| -> serde_json::Value {
        let (_, text) = fixture.iter().find(|(i, _)| i == id).expect("declared");
        serde_json::from_str(text).expect("JSON")
    };

    let typed = content(FIXTURE_IDS[5]);
    assert_eq!(
        (&typed["id"], &typed["name"]),
        (&FIXTURE_IDS[5].into(), &"typed".into())
    );
    let boxed = content(FIXTURE_IDS[0]);
    assert!(
        boxed["$id"]
            .as_str()
            .is_some_and(|id| id.ends_with(FIXTURE_IDS[0])),
        "{boxed}"
    );
}

#[test]
fn local_declarations_stay_in_the_process_wide_inventory() {
    let global: Vec<&str> = gts_kit::inventory::iter::<gts_kit::InventoryTypeSchema>
        .into_iter()
        .map(|e| e.type_id)
        .chain(
            gts_kit::inventory::iter::<gts_kit::InventoryInstance>
                .into_iter()
                .map(|e| e.instance_id),
        )
        .collect();

    for id in FIXTURE_IDS.iter().chain(&[gts_id!(
        "test.cf.local_fixture.widget.v1~test.cf.local_fixture.test_crate.v1"
    )]) {
        assert_eq!(
            global.iter().filter(|g| *g == id).count(),
            1,
            "{id} is in the process-wide inventory exactly once"
        );
    }
    for compiled_out in [
        gts_id!("test.cf.local_fixture.widget.v1~test.cf.local_fixture.cfg_off.v1"),
        gts_id!("test.cf.local_fixture.cfg_off_type.v1~"),
    ] {
        assert!(!global.contains(&compiled_out), "{compiled_out}");
    }
}

// Match the other ToolKit trybuild suites: snapshots record stable diagnostics,
// while coverage uses a nightly compiler with different import suggestions.
#[cfg(not(coverage_nightly))]
#[test]
fn ui() {
    trybuild::TestCases::new().compile_fail("tests/ui/*.rs");
}
