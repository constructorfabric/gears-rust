//! An `OpenApiRegistry` that records every registered `OperationSpec`, so the
//! route tests can read back the operations a `register_*_routes` declares.

use std::sync::Mutex;

use axum::http::Method;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationSpec;

#[derive(Default)]
pub(super) struct RecordingOpenApiRegistry {
    pub(super) operations: Mutex<Vec<OperationSpec>>,
}

impl OpenApiRegistry for RecordingOpenApiRegistry {
    fn register_operation(&self, spec: &OperationSpec) {
        self.operations
            .lock()
            .expect("operations lock")
            .push(spec.clone());
    }

    fn ensure_schema_raw(
        &self,
        name: &str,
        _schemas: Vec<(
            String,
            utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
        )>,
    ) -> String {
        name.to_owned()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// `(status, content_type, description)` of every response `path` declares,
/// in registration order. An empty content type is a body-less response.
pub(super) fn declared_responses(
    registry: &RecordingOpenApiRegistry,
    path: &str,
) -> Vec<(u16, &'static str, String)> {
    let operations = registry.operations.lock().expect("operations lock");
    let [operation] = operations
        .iter()
        .filter(|operation| operation.path == path)
        .collect::<Vec<_>>()[..]
    else {
        panic!("expected one operation on {path}");
    };
    response_rows(operation)
}

/// [`declared_responses`] of the `method` operation on `path`, for a path
/// that several methods share.
pub(super) fn declared_method_responses(
    registry: &RecordingOpenApiRegistry,
    method: &Method,
    path: &str,
) -> Vec<(u16, &'static str, String)> {
    let operations = registry.operations.lock().expect("operations lock");
    let [operation] = operations
        .iter()
        .filter(|operation| operation.method == *method && operation.path == path)
        .collect::<Vec<_>>()[..]
    else {
        panic!("expected one {method} operation on {path}");
    };
    response_rows(operation)
}

fn response_rows(operation: &OperationSpec) -> Vec<(u16, &'static str, String)> {
    operation
        .responses
        .iter()
        .map(|response| {
            (
                response.status,
                response.content_type,
                response.description.clone(),
            )
        })
        .collect()
}
