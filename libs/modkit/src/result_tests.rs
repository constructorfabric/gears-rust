use super::*;

#[test]
fn test_api_result_ok() {
    let result: ApiResult<i32> = Ok(42);
    assert!(result.is_ok());
}

#[test]
fn test_api_result_err() {
    use http::StatusCode;

    let result: ApiResult<i32> = Err(Problem::new(
        StatusCode::BAD_REQUEST,
        "Bad Request",
        "Invalid input",
    ));
    assert!(result.is_err());
}
