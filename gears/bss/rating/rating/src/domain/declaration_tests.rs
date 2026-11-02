use bss_products_sdk::derived::canonical_bytes;
use bss_rating_core::engine::current::input::{DerivedExpr, GranuleFold};

use super::{digest, to_core};
use crate::domain::digest::Sha256Hex;
use crate::test_providers::products::cloudlet_declaration;

#[test]
fn a_declaration_keeps_products_digest_and_reads_as_the_core_s() {
    let declaration = cloudlet_declaration();
    assert_eq!(
        digest(&declaration),
        Sha256Hex::of_bytes(&canonical_bytes(&declaration))
    );
    let core = to_core(&declaration);
    assert_eq!(core.output_unit, "cloudlet-hour");
    assert_eq!(core.inputs[0].granule_fold, GranuleFold::Sum);
    assert!(matches!(core.formula, DerivedExpr::DivConst { .. }));
    assert_eq!(core.output_scale, 6);
}
