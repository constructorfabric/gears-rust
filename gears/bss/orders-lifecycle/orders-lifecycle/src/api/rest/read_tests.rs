#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;

fn uri(query: &str) -> Uri {
    format!("/bss-orders-lifecycle/v1/orders?{query}")
        .parse()
        .unwrap()
}
fn reason(error: OrdersError) -> Reason {
    match error {
        OrdersError::Refused(r) => r,
        other => panic!("{other:?}"),
    }
}

#[test]
fn read_queries_follow_the_flow_order_page_size_then_filters_then_cursor() {
    let ok = list_request(&uri(
        "page_size=200&state=draft&created_from=2026-10-07T10:00:00.123456Z\
         &created_to=2026-10-08T00:00:00%2B02:00&state_entered_before=2026-10-07T00:00:00Z\
         &contract_id=00000000-0000-0000-0000-0000000000c0",
    ))
    .unwrap();
    assert_eq!(ok.page_size.map(PageSize::get), Some(200));
    assert_eq!(ok.filters.state, Some(OrderState::Draft));
    assert_eq!(ok.filters.created_from.unwrap().microsecond(), 123_456);
    assert_eq!(
        ok.filters.created_to.unwrap().offset(),
        time::UtcOffset::from_hms(2, 0, 0).unwrap()
    );
    assert_eq!(ok.filters.contract_id, Some(Uuid::from_u128(0xc0)));
    assert!(ok.cursor.is_none());
    let empty = list_request(&uri("")).unwrap();
    assert_eq!(empty, ListOrders::default());
    for bad in ["0", "201", "x", "-1", "+1", "1.0", "", "1&page_size=1"] {
        assert_eq!(
            reason(list_request(&uri(&format!("page_size={bad}&bogus=1&cursor=x"))).unwrap_err()),
            Reason::PageSizeExceeded,
            "{bad}"
        );
    }
    for bad in [
        "bogus=1",
        "state=bogus",
        "state=draft&state=draft",
        "created_from=now",
        "contract_id=1",
        "page_size=1&sort=x",
        "%zz=1",
    ] {
        assert_eq!(
            reason(list_request(&uri(&format!("{bad}&cursor=x"))).unwrap_err()),
            Reason::FilterInvalid,
            "{bad}"
        );
    }
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
    let lines = line_request(&"/x?page_size=5&cursor=t".parse().unwrap()).unwrap();
    assert_eq!(lines.page_size.map(PageSize::get), Some(5));
    assert_eq!(lines.cursor.unwrap().as_str(), "t");
    assert_eq!(
        reason(line_request(&"/x?state=draft".parse().unwrap()).unwrap_err()),
        Reason::FilterInvalid
    );
    assert_eq!(
        reason(line_request(&"/x?page_size=201".parse().unwrap()).unwrap_err()),
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
    let refused = super::outcome::<serde_json::Value>(Err(OrdersError::Refused(
        Reason::ReadStoreUnavailable,
    )));
    assert_eq!(refused.status().as_u16(), 503);
    assert_eq!(
        refused.headers()["content-type"],
        "application/problem+json"
    );
}
