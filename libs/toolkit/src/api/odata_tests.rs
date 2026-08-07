#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use crate::api::odata::*;
    use axum::extract::FromRequestParts;
    use axum::http::Request;

    #[test]
    fn test_parse_orderby_simple() {
        let result = parse_orderby("created_at desc").unwrap();
        assert_eq!(result.0.len(), 1);
        assert_eq!(result.0[0].field, "created_at");
        assert_eq!(result.0[0].dir, SortDir::Desc);
    }

    #[test]
    fn test_parse_orderby_multiple_fields() {
        let result = parse_orderby("created_at desc, id asc, name").unwrap();
        assert_eq!(result.0.len(), 3);

        assert_eq!(result.0[0].field, "created_at");
        assert_eq!(result.0[0].dir, SortDir::Desc);

        assert_eq!(result.0[1].field, "id");
        assert_eq!(result.0[1].dir, SortDir::Asc);

        assert_eq!(result.0[2].field, "name");
        assert_eq!(result.0[2].dir, SortDir::Asc); // default
    }

    #[test]
    fn test_parse_orderby_whitespace_tolerance() {
        let result = parse_orderby("  created_at   desc  ,   id   asc  ").unwrap();
        assert_eq!(result.0.len(), 2);
        assert_eq!(result.0[0].field, "created_at");
        assert_eq!(result.0[0].dir, SortDir::Desc);
        assert_eq!(result.0[1].field, "id");
        assert_eq!(result.0[1].dir, SortDir::Asc);
    }

    #[test]
    fn test_parse_orderby_empty() {
        let result = parse_orderby("").unwrap();
        assert!(result.is_empty());

        let result = parse_orderby("   ").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_orderby_too_long() {
        let long_orderby = "a".repeat(MAX_ORDERBY_LEN + 1);
        let result = parse_orderby(&long_orderby);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            toolkit_odata::Error::InvalidOrderByField(_)
        ));
    }

    #[test]
    fn test_parse_orderby_too_many_fields() {
        let many_fields: Vec<String> = (0..=MAX_ORDER_FIELDS)
            .map(|i| format!("field{i}"))
            .collect();
        let orderby = many_fields.join(", ");

        let result = parse_orderby(&orderby);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            toolkit_odata::Error::InvalidOrderByField(_)
        ));
    }

    #[test]
    fn test_parse_orderby_invalid_clause() {
        let result = parse_orderby("field invalid_direction");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            toolkit_odata::Error::InvalidOrderByField(_)
        ));
    }

    #[test]
    fn test_parse_orderby_empty_field() {
        let result = parse_orderby(", asc");
        // The new implementation is more lenient and skips empty segments
        // so this now succeeds with one field "asc"
        assert!(result.is_ok());
        let order = result.unwrap();
        assert_eq!(order.0.len(), 1);
        assert_eq!(order.0[0].field, "asc");
    }

    #[tokio::test]
    async fn test_extract_odata_query_full() {
        let uri = "/?%24filter=email%20eq%20%27test%40example.com%27&%24orderby=created_at%20desc&limit=25&cursor=eyJ2IjoxLCJrIjpbInRlc3QiXSwicyI6Ii1jcmVhdGVkX2F0Iiwib28oImFzYyJ9";

        let request = Request::builder().uri(uri).body(()).unwrap();

        let (mut parts, _body) = request.into_parts();

        let result = extract_odata_query(&mut parts, &()).await;
        assert!(result.is_err());
        let canonical = result.unwrap_err();
        // Canonical `InvalidArgument` maps to 400; replaces the legacy 422.
        assert_eq!(canonical.status_code(), 400);
        assert!(canonical.gts_type().contains("invalid_argument"));

        // Should have cursor (even if decode fails, it should try)
        // Note: The cursor in the test is not a valid base64url, but that's OK for this test
    }

    #[tokio::test]
    async fn test_extract_odata_query_filter_only() {
        let uri = "/?%24filter=email%20eq%20%27test%40example.com%27";

        let request = Request::builder().uri(uri).body(()).unwrap();

        let (mut parts, _body) = request.into_parts();

        let query = extract_odata_query(&mut parts, &()).await.unwrap();

        assert!(query.filter.is_some());
        assert!(query.order.is_empty());
        assert_eq!(query.limit, None);
        assert!(query.cursor.is_none());
    }

    #[tokio::test]
    async fn test_extract_odata_query_orderby_only() {
        let uri = "/?%24orderby=created_at%20desc%2C%20id%20asc";

        let request = Request::builder().uri(uri).body(()).unwrap();

        let (mut parts, _body) = request.into_parts();

        let query = extract_odata_query(&mut parts, &()).await.unwrap();

        assert!(query.filter.is_none());
        assert_eq!(query.order.0.len(), 2);
        assert_eq!(query.limit, None);
        assert!(query.cursor.is_none());
    }

    #[tokio::test]
    async fn test_extract_odata_query_empty() {
        let uri = "/";

        let request = Request::builder().uri(uri).body(()).unwrap();

        let (mut parts, _body) = request.into_parts();

        let query = extract_odata_query(&mut parts, &()).await.unwrap();

        assert!(query.filter.is_none());
        assert!(query.order.is_empty());
        assert_eq!(query.limit, None);
        assert!(query.cursor.is_none());
    }

    #[tokio::test]
    async fn test_extract_odata_query_limit_zero_error() {
        let uri = "/?limit=0";

        let request = Request::builder().uri(uri).body(()).unwrap();

        let (mut parts, _body) = request.into_parts();

        let result = extract_odata_query(&mut parts, &()).await;
        assert!(result.is_err());
        let _problem_response = result.unwrap_err();
    }

    #[tokio::test]
    async fn test_extract_odata_query_filter_too_long() {
        let long_filter = "email eq '".to_owned() + &"a".repeat(MAX_FILTER_LEN) + "'";
        let uri = format!("/?%24filter={}", urlencoding::encode(&long_filter));

        let request = Request::builder().uri(uri).body(()).unwrap();

        let (mut parts, _body) = request.into_parts();

        let result = extract_odata_query(&mut parts, &()).await;
        assert!(result.is_err());
        let _problem_response = result.unwrap_err();
    }

    #[tokio::test]
    async fn test_extract_odata_query_invalid_filter() {
        let uri = "/?%24filter=invalid%20syntax%20here";

        let request = Request::builder().uri(uri).body(()).unwrap();

        let (mut parts, _body) = request.into_parts();

        let result = extract_odata_query(&mut parts, &()).await;
        assert!(result.is_err());
        let _problem_response = result.unwrap_err();
    }

    #[tokio::test]
    async fn test_extract_odata_query_invalid_orderby() {
        let uri = "/?%24orderby=field%20invalid_direction";

        let request = Request::builder().uri(uri).body(()).unwrap();

        let (mut parts, _body) = request.into_parts();

        let result = extract_odata_query(&mut parts, &()).await;
        assert!(result.is_err());
        let _problem_response = result.unwrap_err();
    }

    #[tokio::test]
    async fn test_extract_odata_query_invalid_cursor() {
        let uri = "/?cursor=invalid_cursor";

        let request = Request::builder().uri(uri).body(()).unwrap();

        let (mut parts, _body) = request.into_parts();

        let result = extract_odata_query(&mut parts, &()).await;
        assert!(result.is_err());
        let _problem_response = result.unwrap_err();
    }

    #[tokio::test]
    async fn test_odata_extractor() {
        let uri = "/?%24filter=email%20eq%20%27test%40example.com%27&limit=10";

        let request = Request::builder().uri(uri).body(()).unwrap();

        let (mut parts, _body) = request.into_parts();

        let odata = OData::from_request_parts(&mut parts, &()).await.unwrap();

        assert!(odata.filter.is_some());
        assert_eq!(odata.limit, Some(10));
    }

    // ─── `$top` ⇄ `limit` alias ──────────────────────────────────────
    //
    // `$top` is the canonical OData page-size spelling (OASIS OData 4.01
    // Part 2: URL Conventions, §5.1.6). It is a serde alias of `limit`,
    // so both spellings fold onto the same `ODataQuery.limit` slot.

    #[tokio::test]
    async fn test_extract_odata_query_top_binds_limit() {
        let uri = "/?%24top=25";

        let request = Request::builder().uri(uri).body(()).unwrap();

        let (mut parts, _body) = request.into_parts();

        let query = extract_odata_query(&mut parts, &()).await.unwrap();

        assert_eq!(
            query.limit,
            Some(25),
            "`$top` must bind the same slot as `limit`"
        );
    }

    #[tokio::test]
    async fn test_extract_odata_query_top_zero_error() {
        // Same floor as `limit=0`: zero is not a usable page size.
        let uri = "/?%24top=0";

        let request = Request::builder().uri(uri).body(()).unwrap();

        let (mut parts, _body) = request.into_parts();

        let canonical = extract_odata_query(&mut parts, &())
            .await
            .expect_err("`$top=0` must be rejected");
        assert_eq!(canonical.status_code(), 400);

        // The violation names `$top` — a caller who sent `$top` must not be
        // told about a parameter they did not use.
        let problem = toolkit_canonical_errors::Problem::from(canonical);
        let violations = problem
            .context
            .get("field_violations")
            .and_then(|v| v.as_array())
            .expect("invalid_argument must carry field_violations[]");
        assert_eq!(
            violations[0].get("field").and_then(|f| f.as_str()),
            Some("$top"),
            "violation was {:?}",
            violations[0]
        );
    }

    #[tokio::test]
    async fn test_extract_odata_query_top_and_limit_conflict() {
        // Two spellings of one slot in one request is ambiguous: reject it
        // rather than silently picking a winner.
        let uri = "/?%24top=25&limit=10";

        let request = Request::builder().uri(uri).body(()).unwrap();

        let (mut parts, _body) = request.into_parts();

        let canonical = extract_odata_query(&mut parts, &())
            .await
            .expect_err("`$top` together with `limit` must be rejected");
        assert_eq!(canonical.status_code(), 400);

        // Surfaces through the axum-level deserialization path, so the
        // violation is keyed to `query` (gear OpenAPI documents cite this
        // reason for the two-spellings-in-one-request case).
        let problem = toolkit_canonical_errors::Problem::from(canonical);
        let violations = problem
            .context
            .get("field_violations")
            .and_then(|v| v.as_array())
            .expect("invalid_argument must carry field_violations[]");
        assert_eq!(
            violations[0].get("reason").and_then(|r| r.as_str()),
            Some("INVALID_QUERY_PARAMS"),
            "violation was {:?}",
            violations[0]
        );
    }

    #[tokio::test]
    async fn test_odata_extractor_top_alias() {
        let uri = "/?%24filter=email%20eq%20%27test%40example.com%27&%24top=10";

        let request = Request::builder().uri(uri).body(()).unwrap();

        let (mut parts, _body) = request.into_parts();

        let odata = OData::from_request_parts(&mut parts, &()).await.unwrap();

        assert!(odata.filter.is_some());
        assert_eq!(odata.limit, Some(10));
    }

    #[test]
    fn test_odata_deref() {
        use toolkit_odata::ast::*;

        let expr = Expr::Identifier("test".to_owned());
        let query = ODataQuery::default().with_filter(expr);
        let odata = OData(query);

        // Test Deref
        assert!(odata.has_filter());

        // Test AsRef
        let query_ref: &ODataQuery = odata.as_ref();
        assert!(query_ref.has_filter());

        // Test Into
        let query_back: ODataQuery = odata.into();
        assert!(query_back.has_filter());
    }
}
