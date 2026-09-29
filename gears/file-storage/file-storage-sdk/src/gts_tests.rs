use super::*;
use toolkit_gts::gts_id;

#[test]
fn file_type_resource_has_exact_expected_value() {
    // Pinning the literal: the Authorization Service matches on this string, so
    // a silent change here would break per-type access decisions.
    assert_eq!(FILE_TYPE_RESOURCE, gts_id!("cf.fstorage.file.type.v1~"));
}
