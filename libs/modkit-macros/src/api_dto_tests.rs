use super::*;
use syn::parse_quote;

#[test]
fn test_validate_flags_valid_request() {
    let args: Punctuated<Ident, Token![,]> = parse_quote!(request);
    assert!(validate_flags(&args).is_ok());
}

#[test]
fn test_validate_flags_valid_response() {
    let args: Punctuated<Ident, Token![,]> = parse_quote!(response);
    assert!(validate_flags(&args).is_ok());
}

#[test]
fn test_validate_flags_valid_both() {
    let args: Punctuated<Ident, Token![,]> = parse_quote!(request, response);
    assert!(validate_flags(&args).is_ok());
}

#[test]
fn test_validate_flags_unknown_flag() {
    let args: Punctuated<Ident, Token![,]> = parse_quote!(unknown);
    let result = validate_flags(&args);
    assert!(result.is_err());
    let err_str = result.unwrap_err().to_string();
    assert!(err_str.contains("unknown flag 'unknown'"));
    assert!(err_str.contains("expected one of: request, response"));
}

#[test]
fn test_validate_flags_duplicate_request() {
    let args: Punctuated<Ident, Token![,]> = parse_quote!(request, request);
    let result = validate_flags(&args);
    assert!(result.is_err());
    let err_str = result.unwrap_err().to_string();
    assert!(err_str.contains("duplicate flag 'request'"));
}

#[test]
fn test_validate_flags_duplicate_response() {
    let args: Punctuated<Ident, Token![,]> = parse_quote!(response, response);
    let result = validate_flags(&args);
    assert!(result.is_err());
    let err_str = result.unwrap_err().to_string();
    assert!(err_str.contains("duplicate flag 'response'"));
}

#[test]
fn test_validate_flags_typo() {
    let args: Punctuated<Ident, Token![,]> = parse_quote!(requets);
    let result = validate_flags(&args);
    assert!(result.is_err());
    let err_str = result.unwrap_err().to_string();
    assert!(err_str.contains("unknown flag 'requets'"));
}

#[test]
fn test_expand_api_dto_request_only() {
    let args: Punctuated<Ident, Token![,]> = parse_quote!(request);
    let input: DeriveInput = parse_quote! {
        pub struct TestDto {
            pub id: String,
        }
    };
    let output = expand_api_dto(&args, &input);
    let output_str = output.to_string();

    assert!(output_str.contains("serde :: Deserialize"));
    assert!(!output_str.contains("serde :: Serialize"));
    assert!(output_str.contains("RequestApiDto"));
    assert!(!output_str.contains("ResponseApiDto"));
    assert!(output_str.contains("utoipa :: ToSchema"));
    assert!(output_str.contains("rename_all = \"snake_case\""));
}

#[test]
fn test_expand_api_dto_response_only() {
    let args: Punctuated<Ident, Token![,]> = parse_quote!(response);
    let input: DeriveInput = parse_quote! {
        pub struct TestDto {
            pub id: String,
        }
    };
    let output = expand_api_dto(&args, &input);
    let output_str = output.to_string();

    assert!(output_str.contains("serde :: Serialize"));
    assert!(!output_str.contains("serde :: Deserialize"));
    assert!(output_str.contains("ResponseApiDto"));
    assert!(!output_str.contains("RequestApiDto"));
    assert!(output_str.contains("utoipa :: ToSchema"));
    assert!(output_str.contains("rename_all = \"snake_case\""));
}

#[test]
fn test_expand_api_dto_both_flags() {
    let args: Punctuated<Ident, Token![,]> = parse_quote!(request, response);
    let input: DeriveInput = parse_quote! {
        pub struct TestDto {
            pub id: String,
        }
    };
    let output = expand_api_dto(&args, &input);
    let output_str = output.to_string();

    assert!(output_str.contains("serde :: Serialize"));
    assert!(output_str.contains("serde :: Deserialize"));
    assert!(output_str.contains("RequestApiDto"));
    assert!(output_str.contains("ResponseApiDto"));
    assert!(output_str.contains("utoipa :: ToSchema"));
    assert!(output_str.contains("rename_all = \"snake_case\""));
}

#[test]
fn test_expand_api_dto_no_flags_error() {
    let args: Punctuated<Ident, Token![,]> = parse_quote!();
    let input: DeriveInput = parse_quote! {
        pub struct TestDto {
            pub id: String,
        }
    };
    let output = expand_api_dto(&args, &input);
    let output_str = output.to_string();

    assert!(output_str.contains("compile_error"));
    assert!(output_str.contains("requires at least one of 'request' or 'response'"));
}

#[test]
fn test_expand_api_dto_unknown_flag_error() {
    let args: Punctuated<Ident, Token![,]> = parse_quote!(invalid);
    let input: DeriveInput = parse_quote! {
        pub struct TestDto {
            pub id: String,
        }
    };
    let output = expand_api_dto(&args, &input);
    let output_str = output.to_string();

    assert!(output_str.contains("unknown flag 'invalid'"));
}
