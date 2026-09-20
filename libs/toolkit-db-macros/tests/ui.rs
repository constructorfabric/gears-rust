#![allow(clippy::unwrap_used, clippy::expect_used)]

// Compile-fail tests for the Scopable derive macro.
// IMPORTANT: These files must not import external crates like sea_orm or uuid.
// We only validate macro input diagnostics here.
//
// Note: We don't test successful macro expansions here because they would require
// importing the toolkit-db crate. The macro is tested in actual usage in the main codebase.

#[test]
#[cfg(not(coverage_nightly))]
fn ui() {
    let t = trybuild::TestCases::new();

    // Error cases: Basic validation
    t.compile_fail("tests/ui/err_unknown_attr.rs");
    t.compile_fail("tests/ui/err_non_struct.rs");
    t.compile_fail("tests/ui/err_duplicate_tenant_col.rs");
    t.compile_fail("tests/ui/err_dimension_column_not_an_identifier.rs");
    t.compile_fail("tests/ui/err_dimension_column_empty.rs");

    // Error cases: Missing explicit decisions
    t.compile_fail("tests/ui/err_missing_tenant_decision.rs");
    t.compile_fail("tests/ui/err_missing_resource_decision.rs");
    t.compile_fail("tests/ui/err_missing_owner_decision.rs");
    t.compile_fail("tests/ui/err_missing_type_decision.rs");

    // Error cases: Conflicting attributes
    t.compile_fail("tests/ui/err_conflicting_tenant.rs");
    t.compile_fail("tests/ui/err_conflicting_resource.rs");

    // Error cases: Unrestricted with other flags
    t.compile_fail("tests/ui/err_unrestricted_with_tenant.rs");
    t.compile_fail("tests/ui/err_unrestricted_with_resource.rs");
    t.compile_fail("tests/ui/err_unrestricted_with_owner.rs");
    t.compile_fail("tests/ui/err_unrestricted_with_type.rs");
    t.compile_fail("tests/ui/err_unrestricted_with_no_tenant.rs");
    t.compile_fail("tests/ui/err_unrestricted_after_tenant.rs");

    // Error cases: pep_prop validation
    t.compile_fail("tests/ui/err_pep_reserved_owner_tenant_id.rs");
    t.compile_fail("tests/ui/err_pep_reserved_id.rs");
    t.compile_fail("tests/ui/err_pep_reserved_owner_id.rs");
    t.compile_fail("tests/ui/err_pep_duplicate_property.rs");
    t.compile_fail("tests/ui/err_pep_column_not_an_identifier.rs");
    t.compile_fail("tests/ui/err_unrestricted_with_pep.rs");
    t.compile_fail("tests/ui/err_pep_before_unrestricted.rs");

    // Note: Compile-pass tests (ok_*.rs) exist on disk for documentation but are
    // not registered here — successful expansion requires the toolkit-db crate which
    // is not available in the trybuild environment. The macro is tested in actual
    // usage across the main codebase.
}

/// Every `err_*.rs` in `tests/ui` is registered above.
///
/// A fixture nobody runs is worse than no fixture: it looks like coverage in a
/// directory listing, and its `.stderr` drifts without anything noticing.
/// `err_unrestricted_with_owner`, `..._with_type` and `..._with_no_tenant` sat
/// here unregistered long enough to record a message the macro never emitted.
#[test]
fn every_error_fixture_is_registered() {
    // Live lines only, and a call rather than a mention of the path. A
    // registration commented out while debugging leaves the path in the file,
    // and a path in a comment is exactly the fixture that no longer runs --
    // which is what this test is here to notice.
    //
    // `//` only: a block comment would need a parser, and this file is a list
    // of calls, so it has never had one.
    let registrations: Vec<&str> = include_str!("ui.rs")
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("//"))
        .collect();

    let mut orphans: Vec<String> = std::fs::read_dir("tests/ui")
        .expect("the fixture directory must be readable")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension()? != "rs" {
                return None;
            }
            let name = path.file_name()?.to_str()?.to_owned();
            if !name.starts_with("err_") {
                return None;
            }
            let call = format!("compile_fail(\"tests/ui/{name}\")");
            let registered = registrations.iter().any(|line| line.contains(&call));
            (!registered).then_some(name)
        })
        .collect();
    orphans.sort();

    assert!(
        orphans.is_empty(),
        "these fixtures are on disk but never run: {orphans:?}"
    );
}
