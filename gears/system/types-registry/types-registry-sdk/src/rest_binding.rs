//! The HTTP binding descriptors `#[toolkit::provides(transports = [.., rest])]` validates
//! at startup against each contract's IR (`rest-server`).
//!
//! They describe the routes the registry serves for each method, relative to the plane's
//! base path. Headers have no binding kind: a submission's `Idempotency-Key` is the
//! method's `key` argument, sent by the hand-written client and left unbound here.

use toolkit_contract::ir::{HttpBindingIr, HttpFieldBinding, HttpMethod, HttpMethodBindingIr};

/// One method's route; every method of both contracts may be retried (a `SafeRead` or keyed).
fn method(
    name: &str,
    http_method: HttpMethod,
    path: &str,
    field_bindings: Vec<HttpFieldBinding>,
) -> HttpMethodBindingIr {
    HttpMethodBindingIr {
        method_name: name.to_owned(),
        http_method,
        path_template: path.to_owned(),
        field_bindings,
        retryable: true,
        streaming: false,
        stream_framing: toolkit_contract::ir::StreamFraming::default(),
        optional: false,
    }
}

/// The reads both planes serve, with the same shapes.
fn reads() -> Vec<HttpMethodBindingIr> {
    vec![
        method(
            "batch_get_entities",
            HttpMethod::Post,
            "/entities:batchGet",
            vec![HttpFieldBinding::Body],
        ),
        // The request becomes the discovery query string, parameter by parameter.
        method("list_entities", HttpMethod::Get, "/entities", Vec::new()),
    ]
}

/// [`PlatformTypesRegistryApi`](crate::PlatformTypesRegistryApi) on
/// `/types-registry/v2/platform`.
#[must_use]
pub fn platform_types_registry_api_rest_http_binding() -> HttpBindingIr {
    let mut methods = reads();
    methods.extend([
        method(
            "register_entities",
            HttpMethod::Post,
            "/entities",
            vec![HttpFieldBinding::Body],
        ),
        method(
            "delete_entities",
            HttpMethod::Post,
            "/entities:batchDelete",
            vec![HttpFieldBinding::Body],
        ),
        method(
            "get_operation",
            HttpMethod::Get,
            "/operations/{operation_id}",
            vec![HttpFieldBinding::Path {
                field: "operation_id".to_owned(),
                param: "operation_id".to_owned(),
            }],
        ),
    ]);
    HttpBindingIr {
        base_path: crate::paths::PLATFORM.to_owned(),
        methods,
    }
}

/// [`TypesRegistryApi`](crate::TypesRegistryApi) on the interim tenant base path
/// `/types-registry/v2`, which T38 promotes to `/types-registry/v1`.
#[must_use]
pub fn types_registry_api_rest_http_binding() -> HttpBindingIr {
    HttpBindingIr {
        base_path: crate::paths::TENANT.to_owned(),
        methods: reads(),
    }
}

#[cfg(test)]
#[path = "rest_binding_tests.rs"]
mod rest_binding_tests;
