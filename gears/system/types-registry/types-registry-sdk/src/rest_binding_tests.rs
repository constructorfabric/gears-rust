use toolkit_contract::ir::validation::{validate_contract, validate_http_binding};

use super::*;
use crate::contract::{platform_types_registry_api_ir, types_registry_api_ir};

#[test]
fn both_bindings_validate_against_their_contracts() {
    for (ir, binding) in [
        (
            platform_types_registry_api_ir(),
            platform_types_registry_api_rest_http_binding(),
        ),
        (
            types_registry_api_ir(),
            types_registry_api_rest_http_binding(),
        ),
    ] {
        validate_contract(&ir).expect("a valid contract");
        validate_http_binding(&ir, &binding).expect("a valid binding");
    }
}

#[test]
fn the_bindings_name_each_planes_base_path() {
    assert_eq!(
        platform_types_registry_api_rest_http_binding().base_path,
        "/types-registry/v2/platform"
    );
    assert_eq!(
        types_registry_api_rest_http_binding().base_path,
        "/types-registry/v2"
    );
}
