//! The REST read responses serialize the API DTOs (`api::rest::dto`) converted from the SDK read
//! results (TOOLKIT-CORE-002). This pins that move to the wire it replaced: for a corpus of SDK
//! `OrderView`, `OrderPage` and `LinePage` values, the DTO's bytes are exactly the SDK value's
//! bytes (`serde_json::to_vec`), and a value the SDK serializer refuses is refused by the
//! conversion too, never served with an invented spelling.
//!
//! The corpus crosses every optional member (absent, present), every enum variant and the edge
//! values of each scalar: the first and last four-digit years, nanosecond and whole-second
//! instants, the extreme quantities, versions, revisions, page sizes and term counts, cursor
//! punctuation, and text that JSON must escape.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a broken corpus must fail loudly; `allow`, not `expect`, because not every \
              test in the file unwraps or expects"
)]
use bss_orders::api::rest::dto::{
    OrdersLinePage, OrdersOrderPage, OrdersOrderView, ResponseUnencodable,
};
use bss_orders_sdk::authoring::{
    AuthoredTerm, BillingCycle, CalendarDate, Category, Currency, DraftLine, OrderHeader,
    OrderView, Quantity, SalesPath, SelectedItem, VersionSummary,
};
use bss_orders_sdk::catalog::OrderState;
use bss_orders_sdk::models::{DraftRevision, OrderVersion};
use bss_orders_sdk::reads::{Cursor, LinePage, OrderPage, OrderSummary, PageInfo, PageSize};
use serde::Serialize;
use time::{Date, Month, OffsetDateTime};
use uuid::Uuid;

/// The DTO answers exactly the SDK bytes, or both refuse.
fn same_wire<S, D>(sdk: &S)
where
    S: Serialize + Clone + std::fmt::Debug,
    D: Serialize + TryFrom<S, Error = ResponseUnencodable>,
{
    let expected = serde_json::to_vec(sdk);
    let converted = D::try_from(sdk.clone());
    match (expected, converted) {
        (Ok(expected), Ok(dto)) => assert_eq!(
            String::from_utf8(serde_json::to_vec(&dto).unwrap()).unwrap(),
            String::from_utf8(expected).unwrap(),
            "{sdk:?}"
        ),
        (Err(_), Err(_)) => {}
        (expected, converted) => panic!(
            "SDK serialization ok={} but DTO conversion ok={} for {sdk:?}",
            expected.is_ok(),
            converted.is_ok()
        ),
    }
}

fn u(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn quantity(text: &str) -> Quantity {
    serde_json::from_value(serde_json::Value::String(text.to_owned())).unwrap()
}

fn date(year: i32, month: Month, day: u8) -> CalendarDate {
    CalendarDate(Date::from_calendar_date(year, month, day).unwrap())
}

fn instants() -> Vec<OffsetDateTime> {
    vec![
        OffsetDateTime::from_unix_timestamp_nanos(1_791_288_000_123_456_789).unwrap(),
        OffsetDateTime::from_unix_timestamp(1_791_288_000).unwrap(),
        OffsetDateTime::from_unix_timestamp_nanos(1_791_288_000_100_000_000).unwrap(),
        // The first and last instants of the four-digit years.
        Date::from_calendar_date(0, Month::January, 1)
            .unwrap()
            .midnight()
            .assume_utc(),
        Date::from_calendar_date(9999, Month::December, 31)
            .unwrap()
            .with_hms_nano(23, 59, 59, 999_999_999)
            .unwrap()
            .assume_utc(),
    ]
}

fn headers() -> Vec<OrderHeader> {
    let states = [
        OrderState::Draft,
        OrderState::Submitted,
        OrderState::PendingApproval,
        OrderState::Approved,
        OrderState::InFulfillment,
        OrderState::OnHold,
        OrderState::Completed,
        OrderState::Rejected,
        OrderState::Cancelled,
        OrderState::FulfillmentFailed,
        OrderState::Expired,
    ];
    let mut out = Vec::new();
    for (i, at) in instants().into_iter().enumerate() {
        for (j, state) in states.iter().enumerate() {
            out.push(OrderHeader {
                order_id: u(1),
                order_number: [
                    "ORD-1",
                    "",
                    "na\u{ef}ve \"quoted\" \\ back\nline \u{1F600} \u{0007}",
                ][(i + j) % 3]
                    .to_owned(),
                state: *state,
                category: [Category::NewSale, Category::Change][j % 2],
                resource_tenant_id: u(10),
                seller_tenant_id: u(20),
                payer_tenant_id: u(u128::MAX),
                contract_id: [None, Some(u(40))][i % 2],
                sales_path: [SalesPath::SelfService, SalesPath::PartnerPlaced][(i + j) % 2],
                created_at: at,
                state_entered_at: instants()[(i + 1) % 5],
            });
        }
    }
    out
}

fn items() -> Vec<Vec<SelectedItem>> {
    vec![
        vec![],
        vec![SelectedItem {
            item_id: u(7),
            quantity: None,
            selected_dim_value: None,
        }],
        [
            "1",
            "0.0000000000000000000000000001",
            "9999999999999999999999999999",
            "12.5",
        ]
        .iter()
        .enumerate()
        .map(|(k, q)| SelectedItem {
            item_id: u(100 + k as u128),
            quantity: Some(quantity(q)),
            selected_dim_value: [None, Some(String::new()), Some("large/\"x\"\t".to_owned())]
                [k % 3]
                .clone(),
        })
        .collect(),
    ]
}

fn terms() -> Vec<Option<AuthoredTerm>> {
    vec![
        None,
        Some(AuthoredTerm::Rolling),
        Some(AuthoredTerm::Periods { count: 1 }),
        Some(AuthoredTerm::Periods { count: u32::MAX }),
        Some(AuthoredTerm::Calendar {
            years: 0,
            months: 0,
            days: 0,
            microseconds: 1,
        }),
        Some(AuthoredTerm::Calendar {
            years: u32::MAX,
            months: u32::MAX,
            days: u32::MAX,
            microseconds: u64::MAX,
        }),
    ]
}

fn lines() -> Vec<DraftLine> {
    let dates = [
        None,
        Some(date(0, Month::January, 1)),
        Some(date(9999, Month::December, 31)),
        Some(date(2026, Month::February, 28)),
    ];
    let mut out = Vec::new();
    for (i, term) in terms().into_iter().enumerate() {
        for (j, selected) in items().into_iter().enumerate() {
            out.push(DraftLine {
                line_id: u(1000 + (i * 10 + j) as u128),
                plan_id: u(2),
                plan_revision_id: u(3),
                selected_items: selected,
                currency: Currency::try_from(["USD", "EUR", "ZZZ"][j % 3].to_owned()).unwrap(),
                contract_effective_date: dates[i % 4],
                service_activation_date: dates[(i + 1) % 4],
                acceptance_due_date: dates[(i + j) % 4],
                term_duration: term,
                billing_cycle: [None, Some(BillingCycle::Month), Some(BillingCycle::Year)][j % 3],
            });
        }
    }
    out
}

fn versions() -> Vec<VersionSummary> {
    let v = |n: i64| OrderVersion::try_from(n).unwrap();
    instants()
        .into_iter()
        .enumerate()
        .map(|(i, at)| VersionSummary {
            version: [v(1), v(i64::from(i32::MAX))][i % 2],
            supersedes_version: [None, Some(v(1)), Some(v(i64::from(i32::MAX - 1)))][i % 3],
            created_at: at,
        })
        .collect()
}

fn revisions() -> [Option<DraftRevision>; 3] {
    [
        None,
        Some(DraftRevision::try_from(0).unwrap()),
        Some(DraftRevision::try_from(i64::MAX).unwrap()),
    ]
}

fn page_infos() -> Vec<PageInfo> {
    let cursor = |text: &str| Cursor::try_from(text.to_owned()).unwrap();
    vec![
        PageInfo::forward(None, PageSize::DEFAULT),
        PageInfo::forward(
            Some(cursor("v1.abc-_~!\"#$%&'()*+,./:;<=>?@[\\]^`{|}")),
            PageSize::try_from(1).unwrap(),
        ),
        PageInfo {
            next_cursor: Some(cursor("x")),
            prev_cursor: Some(cursor("y")),
            limit: PageSize::try_from(200).unwrap(),
        },
    ]
}

#[test]
fn order_views_serialize_exactly_as_the_sdk_model() {
    let lines = lines();
    let mut count = 0;
    for (i, order) in headers().into_iter().enumerate() {
        for (j, version) in versions().into_iter().enumerate() {
            let view = OrderView {
                order: order.clone(),
                version,
                lines: match (i + j) % 3 {
                    0 => Vec::new(),
                    1 => vec![lines[(i + j) % lines.len()].clone()],
                    _ => lines.clone(),
                },
                draft_revision: revisions()[(i + j) % 3],
            };
            same_wire::<_, OrdersOrderView>(&view);
            count += 1;
        }
    }
    assert!(count >= 100, "corpus too small: {count}");
}

#[test]
fn order_pages_serialize_exactly_as_the_sdk_model() {
    let headers = headers();
    for (i, page_info) in page_infos().into_iter().enumerate() {
        for size in [0, 1, headers.len()] {
            let page = OrderPage {
                items: headers
                    .iter()
                    .take(size)
                    .enumerate()
                    .map(|(k, order)| OrderSummary {
                        order: order.clone(),
                        current_version: OrderVersion::try_from(1 + i64::try_from(k).unwrap())
                            .unwrap(),
                        draft_revision: revisions()[(i + k) % 3],
                    })
                    .collect(),
                page_info: page_info.clone(),
            };
            same_wire::<_, OrdersOrderPage>(&page);
        }
    }
}

#[test]
fn line_pages_serialize_exactly_as_the_sdk_model() {
    let lines = lines();
    for (i, page_info) in page_infos().into_iter().enumerate() {
        for size in [0, 1, lines.len()] {
            for revision in revisions() {
                let page = LinePage {
                    items: lines.iter().take(size).cloned().collect(),
                    page_info: page_info.clone(),
                    current_version: OrderVersion::try_from(1 + i64::try_from(i).unwrap()).unwrap(),
                    draft_revision: revision,
                };
                same_wire::<_, OrdersLinePage>(&page);
            }
        }
    }
}

/// A date or instant before year 0 (`time` without `large-dates` cannot hold one after 9999)
/// has no SDK spelling: the SDK serializer refuses it, and so does the conversion (the handler
/// answers the internal error).
#[test]
fn values_without_an_sdk_spelling_are_refused_by_both() {
    let mut view = OrderView {
        order: headers()[0].clone(),
        version: versions()[0].clone(),
        lines: vec![lines()[0].clone()],
        draft_revision: None,
    };
    for year in [-1, -9999] {
        view.lines[0].acceptance_due_date = Some(date(year, Month::December, 31));
        assert!(serde_json::to_vec(&view).is_err());
        assert_eq!(
            OrdersOrderView::try_from(view.clone()).err().unwrap(),
            ResponseUnencodable("acceptance_due_date")
        );
        same_wire::<_, OrdersOrderView>(&view);
    }
    view.lines[0].acceptance_due_date = None;
    view.order.created_at = Date::from_calendar_date(-1, Month::December, 31)
        .unwrap()
        .midnight()
        .assume_utc();
    assert!(serde_json::to_vec(&view).is_err());
    assert_eq!(
        OrdersOrderView::try_from(view.clone()).err().unwrap(),
        ResponseUnencodable("created_at")
    );
    let mut page = LinePage {
        items: vec![lines()[0].clone()],
        page_info: page_infos()[0].clone(),
        current_version: OrderVersion::try_from(1).unwrap(),
        draft_revision: None,
    };
    page.items[0].service_activation_date = Some(date(-1, Month::January, 1));
    same_wire::<_, OrdersLinePage>(&page);
    assert!(OrdersLinePage::try_from(page).is_err());
}
