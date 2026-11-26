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

/// Every contract method has a required route on its plane. (`api_rest_test` matches
/// each route's verb and path to the operation the registry serves.)
#[test]
fn every_contract_method_is_bound_on_its_plane() {
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
        for method in &ir.methods {
            assert!(!method.optional, "{} is optional in the IR", method.name);
            let bound = binding
                .methods
                .iter()
                .find(|bound| bound.method_name == method.name)
                .unwrap_or_else(|| panic!("{} has no route", method.name));
            assert!(!bound.optional, "{} is bound as optional", method.name);
        }
    }
}
