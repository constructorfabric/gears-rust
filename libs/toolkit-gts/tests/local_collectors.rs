//! Merge real crate-local macro output from two separately compiled crates.

use toolkit_gts::gts_id;
use toolkit_gts::local::{DeclarationSource, merge_declarations};

toolkit_gts::declare_gts_inventory!();

toolkit_gts::local::gts_instance_raw!({
    "id": gts_id!("test.cf.local_fixture.widget.v1~test.cf.local_fixture.test_crate.v1"),
    "name": "test-crate",
});

#[test]
fn the_crates_merge_into_one_sorted_list() {
    let merged = merge_declarations([
        DeclarationSource::new("test crate", gts_declarations()),
        DeclarationSource::new("fixture", declarations_fixture::gts_declarations()),
        // The same declarations again: equal duplicates collapse.
        DeclarationSource::new("fixture again", declarations_fixture::gts_declarations()),
    ])
    .expect("no conflict");

    assert_eq!(merged.len(), declarations_fixture::DECLARED_IDS.len() + 1);
    assert!(merged.windows(2).all(|w| w[0].0 < w[1].0));
}
