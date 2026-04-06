use super::*;
use syn::parse_quote;

// ==================== BASIC FUNCTIONALITY ====================

#[test]
fn test_expand_simple_struct() {
    let input: DeriveInput = parse_quote! {
        pub struct User {
            pub id: String,
            pub name: String,
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(output_str.contains("DomainModel"));
    assert!(!output_str.contains("compile_error"));
}

#[test]
fn test_expand_unit_struct() {
    let input: DeriveInput = parse_quote! {
        pub struct Marker;
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(output_str.contains("DomainModel"));
    assert!(!output_str.contains("compile_error"));
}

#[test]
fn test_expand_enum() {
    let input: DeriveInput = parse_quote! {
        pub enum Status {
            Active,
            Inactive { reason: String },
            Pending(i32),
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(output_str.contains("DomainModel"));
    assert!(!output_str.contains("compile_error"));
}

#[test]
fn test_generic_struct() {
    let input: DeriveInput = parse_quote! {
        pub struct Container<T> {
            pub value: T,
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(output_str.contains("DomainModel"));
    assert!(!output_str.contains("compile_error"));
}

#[test]
fn test_union_rejected() {
    let input: DeriveInput = parse_quote! {
        pub union BadUnion {
            x: i32,
            y: f32,
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(output_str.contains("compile_error"));
    assert!(output_str.contains("union"));
}

// ==================== FORBIDDEN CRATES ====================

#[test]
fn test_forbidden_http_crate() {
    let input: DeriveInput = parse_quote! {
        pub struct BadModel {
            pub status: http::StatusCode,
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(output_str.contains("compile_error"));
    assert!(output_str.contains("http"));
}

#[test]
fn test_forbidden_sqlx_crate() {
    let input: DeriveInput = parse_quote! {
        pub struct BadModel {
            pub pool: sqlx::PgPool,
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(output_str.contains("compile_error"));
    assert!(output_str.contains("sqlx"));
}

#[test]
fn test_forbidden_axum_crate() {
    let input: DeriveInput = parse_quote! {
        pub struct BadModel {
            pub req: axum::extract::Request,
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(output_str.contains("compile_error"));
    assert!(output_str.contains("axum"));
}

#[test]
fn test_forbidden_type_in_option() {
    let input: DeriveInput = parse_quote! {
        pub struct BadModel {
            pub maybe_status: Option<http::StatusCode>,
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(output_str.contains("compile_error"));
    assert!(output_str.contains("http"));
}

#[test]
fn test_enum_with_forbidden_type() {
    let input: DeriveInput = parse_quote! {
        pub enum BadStatus {
            Ok,
            HttpError(http::StatusCode),
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(output_str.contains("compile_error"));
    assert!(output_str.contains("http"));
}

// ==================== FORBIDDEN PATH PREFIXES ====================

#[test]
fn test_forbidden_std_fs() {
    let input: DeriveInput = parse_quote! {
        pub struct BadModel {
            pub file: std::fs::File,
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(output_str.contains("compile_error"));
    assert!(output_str.contains("std::fs"));
}

#[test]
fn test_forbidden_tokio_fs() {
    let input: DeriveInput = parse_quote! {
        pub struct BadModel {
            pub file: tokio::fs::File,
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(output_str.contains("compile_error"));
    assert!(output_str.contains("tokio::fs"));
}

#[test]
fn test_forbidden_type_in_trait_object_generic() {
    // dyn Iterator<Item = http::StatusCode> should be blocked
    let input: DeriveInput = parse_quote! {
        pub struct BadModel {
            pub iter: Box<dyn Iterator<Item = http::StatusCode>>,
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(output_str.contains("compile_error"));
    assert!(output_str.contains("http"));
}

#[test]
fn test_forbidden_type_in_intermediate_segment_generic() {
    // Outer<http::StatusCode>::Inner should be blocked
    let input: DeriveInput = parse_quote! {
        pub struct BadModel {
            pub field: Outer<http::StatusCode>::Inner,
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(output_str.contains("compile_error"));
    assert!(output_str.contains("http"));
}

#[test]
fn test_forbidden_type_in_qself() {
    // <http::StatusCode as SomeTrait>::Output should be blocked
    let input: DeriveInput = parse_quote! {
        pub struct BadModel {
            pub field: <http::StatusCode as Default>::Output,
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(output_str.contains("compile_error"));
    assert!(output_str.contains("http"));
}

// ==================== FORBIDDEN TYPE NAMES (DB-specific) ====================

#[test]
fn test_forbidden_pgpool_unqualified() {
    let input: DeriveInput = parse_quote! {
        pub struct BadModel {
            pub pool: PgPool,
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(output_str.contains("compile_error"));
    assert!(output_str.contains("PgPool"));
}

#[test]
fn test_forbidden_database_connection() {
    let input: DeriveInput = parse_quote! {
        pub struct BadModel {
            pub conn: DatabaseConnection,
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(output_str.contains("compile_error"));
    assert!(output_str.contains("DatabaseConnection"));
}

// ==================== ALLOWED TYPES (no false positives) ====================

#[test]
fn test_allowed_common_types() {
    let input: DeriveInput = parse_quote! {
        pub struct GoodModel {
            pub id: uuid::Uuid,
            pub name: String,
            pub count: i32,
            pub items: Vec<String>,
            pub metadata: Option<serde_json::Value>,
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(!output_str.contains("compile_error"));
    assert!(output_str.contains("DomainModel"));
}

#[test]
fn test_allowed_company_api_path() {
    // "company_api::Thing" should NOT be blocked
    // (first segment is "company_api", not "api")
    let input: DeriveInput = parse_quote! {
        pub struct GoodModel {
            pub thing: company_api::Thing,
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(!output_str.contains("compile_error"));
    assert!(output_str.contains("DomainModel"));
}

#[test]
fn test_allowed_my_infra_path() {
    // "my_infra::Repo" should NOT be blocked
    // (first segment is "my_infra", not "infra")
    let input: DeriveInput = parse_quote! {
        pub struct GoodModel {
            pub repo: my_infra::Repo,
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(!output_str.contains("compile_error"));
    assert!(output_str.contains("DomainModel"));
}

#[test]
fn test_allowed_domain_status_code() {
    // User-defined "StatusCode" should be allowed
    // (it's not in FORBIDDEN_TYPE_NAMES anymore)
    let input: DeriveInput = parse_quote! {
        pub struct GoodModel {
            pub status: my_domain::StatusCode,
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(!output_str.contains("compile_error"));
    assert!(output_str.contains("DomainModel"));
}

#[test]
fn test_allowed_domain_request() {
    // User-defined "Request" should be allowed
    let input: DeriveInput = parse_quote! {
        pub struct GoodModel {
            pub req: domain::Request,
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(!output_str.contains("compile_error"));
    assert!(output_str.contains("DomainModel"));
}

#[test]
fn test_allowed_domain_response() {
    // User-defined "Response" should be allowed
    let input: DeriveInput = parse_quote! {
        pub struct GoodModel {
            pub resp: domain::Response,
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(!output_str.contains("compile_error"));
    assert!(output_str.contains("DomainModel"));
}

#[test]
fn test_allowed_std_other_modules() {
    // std::collections, std::sync etc should be allowed
    // (only std::fs is forbidden)
    let input: DeriveInput = parse_quote! {
        pub struct GoodModel {
            pub data: std::collections::HashMap<String, i32>,
            pub lock: std::sync::Arc<String>,
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(!output_str.contains("compile_error"));
    assert!(output_str.contains("DomainModel"));
}

#[test]
fn test_allowed_capricorn_api_path() {
    // "Domain::capricorn_api::Object" should NOT be blocked
    // (first segment is "Domain", not "api")
    let input: DeriveInput = parse_quote! {
        pub struct GoodModel {
            pub obj: Domain::capricorn_api::Object,
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(!output_str.contains("compile_error"));
    assert!(output_str.contains("DomainModel"));
}

#[test]
fn test_allowed_infra_in_domain_layer() {
    // "infra::Repo" is now allowed (architectural decision left to dylint)
    let input: DeriveInput = parse_quote! {
        pub struct GoodModel {
            pub repo: infra::Repo,
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(!output_str.contains("compile_error"));
    assert!(output_str.contains("DomainModel"));
}

#[test]
fn test_allowed_api_in_domain_layer() {
    // "api::Handler" is now allowed (architectural decision left to dylint)
    let input: DeriveInput = parse_quote! {
        pub struct GoodModel {
            pub handler: api::Handler,
        }
    };

    let output = expand_domain_model(&input);
    let output_str = output.to_string();

    assert!(!output_str.contains("compile_error"));
    assert!(output_str.contains("DomainModel"));
}
