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

    // ─── unsupported system query options ────────────────────────────
    //
    // OASIS OData 4.01 Part 1: Protocol, §6.1 "Query Option
    // Extensibility": a service "MUST fail any request that contains
    // unsupported OData query options defined in the version of this
    // specification supported by the service", and SHOULD fail options it
    // does not understand. Accepting-and-dropping `$skip` left a caller
    // re-reading page one while believing the offset advanced.

    /// Lift `field_violations[]` off a canonical error as
    /// `(field, reason, description)` triples in wire order.
    fn violations_of(canonical: CanonicalError) -> Vec<(String, String, String)> {
        let problem = toolkit_canonical_errors::Problem::from(canonical);
        problem
            .context
            .get("field_violations")
            .and_then(|v| v.as_array())
            .expect("invalid_argument must carry field_violations[]")
            .iter()
            .map(|v| {
                let get = |key: &str| {
                    v.get(key)
                        .and_then(|s| s.as_str())
                        .unwrap_or_default()
                        .to_owned()
                };
                (get("field"), get("reason"), get("description"))
            })
            .collect()
    }

    /// Run the extractor over a bare query string.
    async fn extract(query: &str) -> Result<ODataQuery, CanonicalError> {
        let request = Request::builder()
            .uri(format!("/?{query}"))
            .body(())
            .unwrap();
        let (mut parts, _body) = request.into_parts();
        extract_odata_query(&mut parts, &()).await
    }

    #[tokio::test]
    async fn test_extract_odata_query_skip_rejected() {
        // Offset paging is not the model: the platform pages by cursor, and
        // honoring `$top` while dropping `$skip` would serve a page of the
        // requested size at a fixed offset forever.
        let canonical = extract("%24skip=20")
            .await
            .expect_err("`$skip` must be rejected, not dropped");
        assert_eq!(canonical.status_code(), 400);

        let violations = violations_of(canonical);
        assert_eq!(violations.len(), 1, "violations were {violations:?}");
        let (field, reason, description) = &violations[0];
        assert_eq!(field, "$skip");
        assert_eq!(reason, "UNSUPPORTED_QUERY_PARAM");
        assert!(
            description.contains("$skiptoken"),
            "detail must name the cursor replacement: {description}"
        );
    }

    #[tokio::test]
    async fn test_extract_odata_query_count_rejected() {
        // `PageInfo` carries no total, so `$count` has no truthful answer.
        let canonical = extract("%24count=true")
            .await
            .expect_err("`$count` must be rejected, not dropped");
        assert_eq!(canonical.status_code(), 400);

        let violations = violations_of(canonical);
        assert_eq!(violations.len(), 1, "violations were {violations:?}");
        let (field, reason, description) = &violations[0];
        assert_eq!(field, "$count");
        assert_eq!(reason, "UNSUPPORTED_QUERY_PARAM");
        assert!(
            description.contains("total"),
            "detail must say totals are not part of the page contract: {description}"
        );
    }

    #[tokio::test]
    async fn test_extract_odata_query_misspelled_option_rejected() {
        // The §6.1 SHOULD case: an option the service does not understand.
        // A dropped `$filtre` returns the whole unfiltered collection with
        // a 200.
        let canonical = extract("%24filtre=email%20eq%20%27a%40b.c%27")
            .await
            .expect_err("`$filtre` must be rejected, not dropped");
        assert_eq!(canonical.status_code(), 400);

        let violations = violations_of(canonical);
        assert_eq!(violations.len(), 1, "violations were {violations:?}");
        let (field, _, description) = &violations[0];
        assert_eq!(field, "$filtre");
        // `$filtre` is not an OData-defined option, so it must not be
        // described as one: it is unknown, not unimplemented.
        assert!(
            description.contains("unknown"),
            "a typo must read as unknown, not as unsupported: {description}"
        );
    }

    #[tokio::test]
    async fn test_extract_odata_query_reports_every_unsupported_option() {
        // One round trip should name every offending option, in wire order,
        // rather than making the caller fix them one request at a time.
        let canonical = extract("%24skip=20&%24count=true&%24expand=tenant")
            .await
            .expect_err("unsupported options must be rejected");

        let fields: Vec<String> = violations_of(canonical)
            .into_iter()
            .map(|(field, _, _)| field)
            .collect();
        assert_eq!(fields, ["$skip", "$count", "$expand"]);
    }

    #[tokio::test]
    async fn test_extract_odata_query_repeated_unsupported_option_reported_once() {
        // One violation per offending key, not per occurrence: a repeated
        // option is one mistake to fix.
        let canonical = extract("%24skip=20&%24skip=40")
            .await
            .expect_err("`$skip` must be rejected");

        let fields: Vec<String> = violations_of(canonical)
            .into_iter()
            .map(|(field, _, _)| field)
            .collect();
        assert_eq!(fields, ["$skip"]);
    }

    #[tokio::test]
    async fn test_extract_odata_query_miscased_option_rejected() {
        // `ODataParams` binds `$top` case-sensitively, so `$Top` would
        // otherwise be dropped. Reject it and name the spelling that binds.
        // (OData 4.01 Part 2 §5.1 asks a 4.01 service to accept
        // case-insensitive option names; this platform does not, and says
        // so instead of ignoring the request.)
        let canonical = extract("%24Top=10")
            .await
            .expect_err("`$Top` must be rejected, not dropped");
        assert_eq!(canonical.status_code(), 400);

        let violations = violations_of(canonical);
        assert_eq!(violations.len(), 1, "violations were {violations:?}");
        let (field, _, description) = &violations[0];
        assert_eq!(field, "$Top");
        assert!(
            description.contains("$top"),
            "detail must name the spelling that binds: {description}"
        );
    }

    #[tokio::test]
    async fn test_extract_odata_query_skiptoken_binds_cursor() {
        // `$skiptoken` is the canonical OData spelling of what this
        // platform calls `cursor` — alias, same slot, same as `$top`.
        let token = CursorV1 {
            k: vec!["2026-08-10T00:00:00Z".to_owned()],
            o: SortDir::Asc,
            s: "created_at".to_owned(),
            f: None,
            d: "fwd".to_owned(),
        }
        .encode()
        .expect("cursor must encode");

        let query = extract(&format!("%24skiptoken={token}"))
            .await
            .expect("`$skiptoken` must be accepted");
        assert!(
            query.cursor.is_some(),
            "`$skiptoken` must bind the same slot as `cursor`"
        );
    }

    #[tokio::test]
    async fn test_extract_odata_query_non_odata_params_pass_through() {
        // Part 1 §6.1 reserves the `$` prefix for OData system query
        // options, so unprefixed keys belong to the handler's own params
        // struct — chat-engine's `q` / `context`, file-storage's `offset`.
        // The extractor must not police that namespace.
        let query = extract("q=needle&context=2&limit=5")
            .await
            .expect("non-OData params belong to the handler, not this extractor");
        assert_eq!(query.limit, Some(5));
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
