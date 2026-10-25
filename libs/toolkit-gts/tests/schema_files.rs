//! Drift guard for the checked-in schema files under `schemas/`.
//!
//! Base types are defined by Rust structs; the files are copies of the
//! macro-generated schemas so that doc-level tooling (IDE validators,
//! `gts-validator`, gears that ship JSON instances of these types) can
//! resolve them. Regenerate a file from the accessor when this fails.

use toolkit_gts::AuthzPermissionV1;

#[test]
fn authz_permission_schema_file_matches_struct() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/schemas/gts.cf.toolkit.authz.permission.v1~.schema.json"
    );
    let on_disk: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read schema file"))
            .expect("schema file is valid JSON");
    let generated: serde_json::Value =
        serde_json::from_str(&AuthzPermissionV1::gts_schema_with_refs_as_string())
            .expect("generated schema is valid JSON");
    assert_eq!(
        on_disk, generated,
        "{path} is out of date; regenerate it from AuthzPermissionV1::gts_schema_with_refs_as_string_pretty()"
    );
}
