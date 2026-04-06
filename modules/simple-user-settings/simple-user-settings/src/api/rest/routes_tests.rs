use super::License;
use modkit::api::operation_builder::LicenseFeature;

#[test]
fn test_license_as_ref() {
    let license = License;
    assert_eq!(
        license.as_ref(),
        "gts.x.core.lic.feat.v1~x.core.global.base.v1"
    );
}

#[test]
fn test_license_implements_license_feature() {
    fn assert_license_feature<T: LicenseFeature>(_: &T) {}
    let license = License;
    assert_license_feature(&license);
}
