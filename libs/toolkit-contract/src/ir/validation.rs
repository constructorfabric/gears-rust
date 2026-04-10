use super::binding::{HttpBindingIr, HttpFieldBinding, HttpMethod, HttpMethodBindingIr};
use super::contract::ContractIr;
use std::collections::HashSet;
use std::fmt;

#[derive(Debug, Clone)]
pub struct ValidationError {
    pub location: String,
    pub message: String,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.location, self.message)
    }
}

/// Validates a [`ContractIr`] for structural well-formedness.
///
/// # Errors
/// Returns a vector of [`ValidationError`] describing every problem found
/// (empty name/module/version, no methods, empty or duplicate method names).
pub fn validate_contract(ir: &ContractIr) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();

    if ir.name.is_empty() {
        errors.push(ValidationError {
            location: "ContractIr".to_owned(),
            message: "contract name must not be empty".to_owned(),
        });
    }

    if ir.gear.is_empty() {
        errors.push(ValidationError {
            location: "ContractIr".to_owned(),
            message: "gear must not be empty".to_owned(),
        });
    }

    if ir.version.is_empty() {
        errors.push(ValidationError {
            location: "ContractIr".to_owned(),
            message: "version must not be empty".to_owned(),
        });
    }

    if ir.methods.is_empty() {
        errors.push(ValidationError {
            location: "ContractIr".to_owned(),
            message: "must have at least one method".to_owned(),
        });
    }

    let mut seen_names: HashSet<&str> = HashSet::new();
    for method in &ir.methods {
        if method.name.is_empty() {
            errors.push(ValidationError {
                location: format!("ContractIr.methods[{}]", method.name),
                message: "method name must not be empty".to_owned(),
            });
        } else if !seen_names.insert(&method.name) {
            errors.push(ValidationError {
                location: format!("ContractIr.methods[{0}]", method.name),
                message: format!("duplicate method name: {}", method.name),
            });
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Validates an [`HttpBindingIr`] against its [`ContractIr`].
///
/// # Errors
/// Returns a vector of [`ValidationError`] when `base_path` is malformed or method
/// coverage / path-template / field-binding checks fail.
pub fn validate_http_binding(
    contract: &ContractIr,
    binding: &HttpBindingIr,
) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();

    if binding.base_path.is_empty() {
        errors.push(ValidationError {
            location: "HttpBindingIr".to_owned(),
            message: "base_path must not be empty".to_owned(),
        });
    } else if !binding.base_path.starts_with('/') {
        errors.push(ValidationError {
            location: "HttpBindingIr".to_owned(),
            message: format!("base_path must start with '/': got '{}'", binding.base_path),
        });
    }

    validate_method_coverage(contract, binding, &mut errors);

    for method_binding in &binding.methods {
        validate_single_method_binding(contract, method_binding, &mut errors);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn validate_method_coverage(
    contract: &ContractIr,
    binding: &HttpBindingIr,
    errors: &mut Vec<ValidationError>,
) {
    let contract_method_names: HashSet<&str> =
        contract.methods.iter().map(|m| m.name.as_str()).collect();
    let mut binding_method_names: HashSet<&str> = HashSet::new();

    for method in &binding.methods {
        let name = method.method_name.as_str();
        if !binding_method_names.insert(name) {
            errors.push(ValidationError {
                location: format!("HttpBindingIr.methods[{name}]"),
                message: format!("duplicate binding for contract method: {name}"),
            });
        }
    }

    for name in &contract_method_names {
        if !binding_method_names.contains(name) {
            errors.push(ValidationError {
                location: format!("HttpBindingIr.methods[{name}]"),
                message: format!("missing binding for contract method: {name}"),
            });
        }
    }

    for name in &binding_method_names {
        if !contract_method_names.contains(name) {
            errors.push(ValidationError {
                location: format!("HttpBindingIr.methods[{name}]"),
                message: format!("binding for unknown method not in contract: {name}"),
            });
        }
    }
}

fn validate_single_method_binding(
    contract: &ContractIr,
    method_binding: &HttpMethodBindingIr,
    errors: &mut Vec<ValidationError>,
) {
    let method_loc = format!("HttpBindingIr.methods[{}]", method_binding.method_name);

    validate_body_constraint(method_binding, &method_loc, errors);
    validate_path_params(method_binding, &method_loc, errors);
    validate_path_template_braces(method_binding, &method_loc, errors);
    validate_field_references(contract, method_binding, &method_loc, errors);
    validate_single_body_binding(method_binding, &method_loc, errors);
}

fn validate_body_constraint(
    method_binding: &HttpMethodBindingIr,
    method_loc: &str,
    errors: &mut Vec<ValidationError>,
) {
    if !matches!(
        method_binding.http_method,
        HttpMethod::Get | HttpMethod::Delete
    ) {
        return;
    }

    let has_body = method_binding
        .field_bindings
        .iter()
        .any(|fb| matches!(fb, HttpFieldBinding::Body));

    if has_body {
        let verb = match method_binding.http_method {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
            HttpMethod::Put => "PUT",
            HttpMethod::Delete => "DELETE",
        };
        errors.push(ValidationError {
            location: method_loc.to_owned(),
            message: format!("{verb} method must not have Body field binding"),
        });
    }
}

fn validate_path_params(
    method_binding: &HttpMethodBindingIr,
    method_loc: &str,
    errors: &mut Vec<ValidationError>,
) {
    let template_params = extract_path_params(&method_binding.path_template);
    let path_binding_params: HashSet<&str> = method_binding
        .field_bindings
        .iter()
        .filter_map(|fb| {
            if let HttpFieldBinding::Path { param, .. } = fb {
                Some(param.as_str())
            } else {
                None
            }
        })
        .collect();

    for param in &template_params {
        if !path_binding_params.contains(param.as_str()) {
            errors.push(ValidationError {
                location: method_loc.to_owned(),
                message: format!(
                    "path template parameter '{{{param}}}' has no corresponding Path field binding"
                ),
            });
        }
    }
}

fn validate_field_references(
    contract: &ContractIr,
    method_binding: &HttpMethodBindingIr,
    method_loc: &str,
    errors: &mut Vec<ValidationError>,
) {
    let Some(contract_method) = contract
        .methods
        .iter()
        .find(|m| m.name == method_binding.method_name)
    else {
        return;
    };

    let input_field_names: HashSet<&str> = contract_method
        .input
        .fields
        .iter()
        .map(|f| f.name.as_str())
        .collect();

    for fb in &method_binding.field_bindings {
        let (kind, field) = match fb {
            HttpFieldBinding::Path { field, .. } => ("Path", field),
            HttpFieldBinding::Query { field, .. } => ("Query", field),
            HttpFieldBinding::Header { field, .. } => ("Header", field),
            HttpFieldBinding::Body => continue,
        };
        if !input_field_names.contains(field.as_str()) {
            errors.push(ValidationError {
                location: method_loc.to_owned(),
                message: format!(
                    "{kind} binding references field '{field}' not found in contract method input"
                ),
            });
        }
    }
}

fn validate_single_body_binding(
    method_binding: &HttpMethodBindingIr,
    method_loc: &str,
    errors: &mut Vec<ValidationError>,
) {
    let body_count = method_binding
        .field_bindings
        .iter()
        .filter(|fb| matches!(fb, HttpFieldBinding::Body))
        .count();
    if body_count > 1 {
        errors.push(ValidationError {
            location: method_loc.to_owned(),
            message: format!(
                "method has {body_count} Body bindings; at most one Body binding is allowed"
            ),
        });
    }
}

fn validate_path_template_braces(
    method_binding: &HttpMethodBindingIr,
    method_loc: &str,
    errors: &mut Vec<ValidationError>,
) {
    let template = &method_binding.path_template;
    let mut depth = 0i32;
    let mut current_param = String::new();
    let mut in_param = false;
    for ch in template.chars() {
        match ch {
            '{' => {
                if in_param {
                    errors.push(ValidationError {
                        location: method_loc.to_owned(),
                        message: format!(
                            "path template '{template}' has nested '{{' before matching '}}'"
                        ),
                    });
                    return;
                }
                in_param = true;
                depth += 1;
                current_param.clear();
            }
            '}' => {
                if !in_param {
                    errors.push(ValidationError {
                        location: method_loc.to_owned(),
                        message: format!("path template '{template}' has unmatched '}}'"),
                    });
                    return;
                }
                if current_param.is_empty() {
                    errors.push(ValidationError {
                        location: method_loc.to_owned(),
                        message: format!("path template '{template}' has empty parameter '{{}}'"),
                    });
                }
                if !current_param
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
                    || current_param
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_digit())
                {
                    errors.push(ValidationError {
                        location: method_loc.to_owned(),
                        message: format!(
                            "path template '{template}' parameter '{{{current_param}}}' is not a valid identifier"
                        ),
                    });
                }
                in_param = false;
                depth -= 1;
                current_param.clear();
            }
            '/' => {
                if in_param {
                    errors.push(ValidationError {
                        location: method_loc.to_owned(),
                        message: format!(
                            "path template '{template}' has unclosed '{{' before path separator"
                        ),
                    });
                    return;
                }
            }
            other => {
                if in_param {
                    current_param.push(other);
                }
            }
        }
    }
    if depth != 0 {
        errors.push(ValidationError {
            location: method_loc.to_owned(),
            message: format!("path template '{template}' has unbalanced braces"),
        });
    }
}

fn extract_path_params(template: &str) -> Vec<String> {
    let mut params = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        if let Some(end) = rest[start..].find('}') {
            let param = &rest[start + 1..start + end];
            if !param.is_empty() {
                params.push(param.to_owned());
            }
            rest = &rest[start + end + 1..];
        } else {
            break;
        }
    }
    params
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::ir::contract::{
        FieldIr, Idempotency, InputShape, MethodIr, MethodKind, PrimitiveType, ServiceIr, TypeRef,
    };

    fn one_method_contract() -> ContractIr {
        ServiceIr {
            name: "Svc".into(),
            gear: "m".into(),
            version: "v1".into(),
            methods: vec![MethodIr {
                name: "do_thing".into(),
                kind: MethodKind::Unary,
                input: InputShape {
                    fields: vec![FieldIr {
                        name: "id".into(),
                        ty: TypeRef::Primitive(PrimitiveType::String),
                        optional: false,
                        role: crate::ir::contract::FieldRole::Wire,
                    }],
                },
                output: TypeRef::Named("Out".into()),
                error: None,
                idempotency: Idempotency::SafeRead,
                optional: false,
            }],
        }
    }

    #[test]
    fn rejects_query_binding_to_unknown_field() {
        let contract = one_method_contract();
        let binding = HttpBindingIr {
            base_path: "/api".into(),
            methods: vec![HttpMethodBindingIr {
                method_name: "do_thing".into(),
                http_method: HttpMethod::Get,
                path_template: "/things".into(),
                field_bindings: vec![HttpFieldBinding::Query {
                    field: "missing".into(),
                    param: "missing".into(),
                }],
                retryable: false,
                streaming: false,
                optional: false,
            }],
        };
        let errs = validate_http_binding(&contract, &binding).unwrap_err();
        assert!(
            errs.iter().any(|e| e
                .message
                .contains("Query binding references field 'missing'")),
            "expected query field-ref error, got: {errs:?}"
        );
    }

    #[test]
    fn rejects_header_binding_to_unknown_field() {
        let contract = one_method_contract();
        let binding = HttpBindingIr {
            base_path: "/api".into(),
            methods: vec![HttpMethodBindingIr {
                method_name: "do_thing".into(),
                http_method: HttpMethod::Get,
                path_template: "/things".into(),
                field_bindings: vec![HttpFieldBinding::Header {
                    field: "nope".into(),
                    header: "X-Nope".into(),
                }],
                retryable: false,
                streaming: false,
                optional: false,
            }],
        };
        let errs = validate_http_binding(&contract, &binding).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.message.contains("Header binding references field 'nope'")),
            "expected header field-ref error, got: {errs:?}"
        );
    }

    #[test]
    fn rejects_duplicate_body_bindings() {
        let contract = one_method_contract();
        let binding = HttpBindingIr {
            base_path: "/api".into(),
            methods: vec![HttpMethodBindingIr {
                method_name: "do_thing".into(),
                http_method: HttpMethod::Post,
                path_template: "/things".into(),
                field_bindings: vec![HttpFieldBinding::Body, HttpFieldBinding::Body],
                retryable: false,
                streaming: false,
                optional: false,
            }],
        };
        let errs = validate_http_binding(&contract, &binding).unwrap_err();
        assert!(
            errs.iter().any(|e| e.message.contains("Body bindings")),
            "expected duplicate Body error, got: {errs:?}"
        );
    }

    #[test]
    fn rejects_base_path_without_leading_slash() {
        let contract = one_method_contract();
        let binding = HttpBindingIr {
            base_path: "api/m/v1".into(),
            methods: vec![HttpMethodBindingIr {
                method_name: "do_thing".into(),
                http_method: HttpMethod::Get,
                path_template: "/things".into(),
                field_bindings: vec![],
                retryable: false,
                streaming: false,
                optional: false,
            }],
        };
        let errs = validate_http_binding(&contract, &binding).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.message.contains("base_path must start with '/'")),
            "expected base_path slash error, got: {errs:?}"
        );
    }

    #[test]
    fn rejects_unbalanced_path_template_braces() {
        let contract = one_method_contract();
        let binding = HttpBindingIr {
            base_path: "/api".into(),
            methods: vec![HttpMethodBindingIr {
                method_name: "do_thing".into(),
                http_method: HttpMethod::Get,
                path_template: "/things/{id".into(),
                field_bindings: vec![HttpFieldBinding::Path {
                    field: "id".into(),
                    param: "id".into(),
                }],
                retryable: false,
                streaming: false,
                optional: false,
            }],
        };
        let errs = validate_http_binding(&contract, &binding).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.message.contains("unclosed '{'")
                    || e.message.contains("unbalanced braces")),
            "expected unbalanced brace error, got: {errs:?}"
        );
    }

    #[test]
    fn accepts_valid_binding() {
        let contract = one_method_contract();
        let binding = HttpBindingIr {
            base_path: "/api".into(),
            methods: vec![HttpMethodBindingIr {
                method_name: "do_thing".into(),
                http_method: HttpMethod::Get,
                path_template: "/things/{id}".into(),
                field_bindings: vec![HttpFieldBinding::Path {
                    field: "id".into(),
                    param: "id".into(),
                }],
                retryable: false,
                streaming: false,
                optional: false,
            }],
        };
        validate_http_binding(&contract, &binding).expect("valid binding should pass");
    }
}
