#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once; `allow`, not \
              `expect`, because not every test in the file unwraps or expects"
)]
use super::*;

fn uri(query: &str) -> Uri {
    format!("/bss-orders/v1/orders?{query}").parse().unwrap()
}
fn reason(error: OrdersError) -> Reason {
    match error {
        OrdersError::Refused(r) => r,
        other => panic!("{other:?}"),
    }
}

fn at(text: &str) -> time::OffsetDateTime {
    time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).unwrap()
}
/// A query string with its `$filter` value percent-encoded (spaces, `+`, quotes).
fn filtered(filter: &str, rest: &str) -> Uri {
    let encoded: String = filter
        .bytes()
        .map(|b| match b {
            b' ' => "%20".to_owned(),
            b'+' => "%2B".to_owned(),
            b'\'' => "%27".to_owned(),
            b => (b as char).to_string(),
        })
        .collect();
    uri(&format!("%24filter={encoded}{rest}"))
}

#[test]
fn read_queries_follow_the_flow_order_limit_then_filters_then_cursor() {
    let ok = list_request(&filtered(
        "state eq 'draft' and created_at ge 2026-10-07T10:00:00.123456Z \
         and created_at lt 2026-10-08T00:00:00+02:00 \
         and state_entered_at le 2026-10-07T00:00:00Z \
         and contract_id eq 00000000-0000-0000-0000-0000000000c0",
        "&limit=200",
    ))
    .unwrap();
    assert_eq!(ok.limit.map(PageSize::get), Some(200));
    assert_eq!(ok.filters.state, Some(OrderState::Draft));
    assert_eq!(ok.filters.created_from.unwrap().microsecond(), 123_456);
    // The instant is kept exactly; an offset literal is normalized to UTC.
    assert_eq!(ok.filters.created_to.unwrap(), at("2026-10-07T22:00:00Z"));
    assert_eq!(
        ok.filters.state_entered_before.unwrap(),
        at("2026-10-07T00:00:00Z")
    );
    assert_eq!(ok.filters.contract_id, Some(Uuid::from_u128(0xc0)));
    assert!(ok.cursor.is_none());
    let empty = list_request(&uri("")).unwrap();
    assert_eq!(empty, ListOrders::default());
}

#[test]
fn limit_precedes_filters_and_every_unsupported_filter_shape_is_filter_invalid() {
    for bad in ["0", "201", "x", "-1", "+1", "1.0", "", "1&limit=1"] {
        assert_eq!(
            reason(list_request(&uri(&format!("limit={bad}&bogus=1&cursor=a%20b"))).unwrap_err()),
            Reason::PageSizeExceeded,
            "{bad}"
        );
    }
    // Unknown names, the retired query names and every unsupported filter shape. Each is paired
    // with a malformed cursor, so the filter step must answer before the cursor step does.
    for bad in [
        "bogus=1",
        "page_size=1",
        "state=draft",
        "%24orderby=created_at",
        "%24top=1",
        "%zz=1",
        "%24filter=state%20eq%20%27draft%27&%24filter=state%20eq%20%27draft%27",
    ] {
        assert_eq!(
            reason(list_request(&uri(&format!("{bad}&cursor=a%20b"))).unwrap_err()),
            Reason::FilterInvalid,
            "{bad}"
        );
    }
    for bad in [
        "state eq 'bogus'",
        "state ne 'draft'",
        "state eq 'draft' or state eq 'submitted'",
        "not (state eq 'draft')",
        "state eq 'draft' and state eq 'draft'",
        "created_at gt 2026-10-07T00:00:00Z",
        "created_at ge 'now'",
        "state_entered_at lt 2026-10-07T00:00:00Z",
        "contract_id eq 1",
        "contract_id in (00000000-0000-0000-0000-0000000000c0)",
        "order_id eq 00000000-0000-0000-0000-0000000000c0",
        "contains(state, 'dra')",
        "state eq",
    ] {
        assert_eq!(
            reason(list_request(&filtered(bad, "&cursor=a%20b")).unwrap_err()),
            Reason::FilterInvalid,
            "{bad}"
        );
    }
}

/// The raw `$filter` is at most the toolkit's `MAX_FILTER_LEN` bytes (pricing D-517 precedent,
/// `pricing/tests/sku_reads.rs`): a well-formed `state eq` padded to the limit is read, the same
/// filter one byte longer is `filter-invalid` before it is parsed.
#[test]
fn the_filter_length_cap_admits_the_limit_and_refuses_one_byte_more() {
    let padded = |len: usize| {
        let tail = "eq 'draft'";
        format!(
            "state{}{tail}",
            " ".repeat(len - "state".len() - tail.len())
        )
    };
    let limit = toolkit::api::odata::MAX_FILTER_LEN;
    let at = padded(limit);
    assert_eq!(at.len(), limit);
    let read = list_request(&filtered(&at, "")).unwrap();
    assert_eq!(read.filters.state, Some(OrderState::Draft));
    let over = padded(limit + 1);
    assert_eq!(over.len(), limit + 1);
    assert_eq!(
        reason(list_request(&filtered(&over, "")).unwrap_err()),
        Reason::FilterInvalid
    );
}

#[test]
fn cursors_are_checked_last_and_the_line_list_takes_no_filter() {
    for bad in ["cursor=", "cursor=a%20b", "cursor=a&cursor=b"] {
        assert_eq!(
            reason(list_request(&uri(bad)).unwrap_err()),
            Reason::CursorInvalid,
            "{bad}"
        );
    }
    let token = list_request(&uri("cursor=abc")).unwrap().cursor.unwrap();
    assert_eq!(token.as_str(), "abc");
    // The line list takes no filter at all.
    let lines = line_request(&"/x?limit=5&cursor=t".parse().unwrap()).unwrap();
    assert_eq!(lines.limit.map(PageSize::get), Some(5));
    assert_eq!(lines.cursor.unwrap().as_str(), "t");
    for bad in [
        "/x?state=draft",
        "/x?%24filter=state%20eq%20%27draft%27",
        "/x?page_size=5",
    ] {
        assert_eq!(
            reason(line_request(&bad.parse().unwrap()).unwrap_err()),
            Reason::FilterInvalid,
            "{bad}"
        );
    }
    assert_eq!(
        reason(line_request(&"/x?limit=201".parse().unwrap()).unwrap_err()),
        Reason::PageSizeExceeded
    );
}

#[test]
fn served_responses_carry_the_strong_version_etag() {
    let out = served(
        Some(OrderVersion::try_from(7).unwrap()),
        &serde_json::json!({"a": 1}),
    );
    assert_eq!(out.status(), StatusCode::OK);
    assert_eq!(out.headers()["etag"], "\"7\"");
    let out = served::<serde_json::Value>(None, &serde_json::json!([]));
    assert!(out.headers().get("etag").is_none());
    let refused =
        super::super::error::ApiError::from(OrdersError::Refused(Reason::ReadStoreUnavailable))
            .into_response();
    assert_eq!(refused.status().as_u16(), 503);
    assert_eq!(
        refused.headers()["content-type"],
        "application/problem+json"
    );
}
