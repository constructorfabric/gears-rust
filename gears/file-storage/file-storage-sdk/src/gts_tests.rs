use super::*;

#[test]
fn file_type_resource_has_exact_expected_value() {
    // Pinning the literal: the Authorization Service matches on this string, so
    // a silent change here would break per-type access decisions.
    assert_eq!(FILE_TYPE_RESOURCE, "gts.cf.fstorage.file.type.v1~");
}

#[test]
fn file_type_resource_follows_gts_type_format() {
    assert!(
        FILE_TYPE_RESOURCE.starts_with("gts.cf.fstorage.file.type.v1"),
        "must be the fstorage file-type family: {FILE_TYPE_RESOURCE}"
    );
    assert!(
        FILE_TYPE_RESOURCE.ends_with('~'),
        "a GTS type identifier must terminate with '~': {FILE_TYPE_RESOURCE}"
    );
}
