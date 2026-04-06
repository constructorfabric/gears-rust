use super::*;

#[test]
fn test_parse_retry_after_seconds() {
    let mut headers = HeaderMap::new();
    headers.insert(http::header::RETRY_AFTER, "120".parse().unwrap());

    let result = parse_retry_after(&headers);
    assert_eq!(result, Some(Duration::from_secs(120)));
}

#[test]
fn test_parse_retry_after_seconds_with_whitespace() {
    let mut headers = HeaderMap::new();
    headers.insert(http::header::RETRY_AFTER, "  60  ".parse().unwrap());

    let result = parse_retry_after(&headers);
    assert_eq!(result, Some(Duration::from_secs(60)));
}

#[test]
fn test_parse_retry_after_missing() {
    let headers = HeaderMap::new();
    let result = parse_retry_after(&headers);
    assert_eq!(result, None);
}

#[test]
fn test_parse_retry_after_invalid() {
    let mut headers = HeaderMap::new();
    headers.insert(http::header::RETRY_AFTER, "not-a-number".parse().unwrap());

    let result = parse_retry_after(&headers);
    assert_eq!(result, None);
}

#[test]
fn test_parse_retry_after_http_date_in_past() {
    let mut headers = HeaderMap::new();
    // HTTP-date in the past returns None
    headers.insert(
        http::header::RETRY_AFTER,
        "Wed, 21 Oct 2015 07:28:00 GMT".parse().unwrap(),
    );

    let result = parse_retry_after(&headers);
    assert_eq!(result, None);
}

#[test]
fn test_parse_retry_after_http_date_in_future() {
    let mut headers = HeaderMap::new();
    // Create a date 60 seconds in the future
    let future_time = SystemTime::now() + Duration::from_secs(60);
    let http_date = httpdate::fmt_http_date(future_time);
    headers.insert(http::header::RETRY_AFTER, http_date.parse().unwrap());

    let result = parse_retry_after(&headers);
    assert!(result.is_some());
    // Should be approximately 60 seconds (with some tolerance for test execution)
    let duration = result.unwrap();
    assert!(duration.as_secs() >= 58 && duration.as_secs() <= 62);
}

#[test]
fn test_parse_retry_after_negative_seconds() {
    let mut headers = HeaderMap::new();
    headers.insert(http::header::RETRY_AFTER, "-5".parse().unwrap());

    let result = parse_retry_after(&headers);
    assert_eq!(result, None);
}

#[test]
fn test_parse_retry_after_zero() {
    let mut headers = HeaderMap::new();
    headers.insert(http::header::RETRY_AFTER, "0".parse().unwrap());

    let result = parse_retry_after(&headers);
    assert_eq!(result, Some(Duration::from_secs(0)));
}
