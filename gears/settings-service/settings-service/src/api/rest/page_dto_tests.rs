// Created: 2026-10-08 by Virtuozzo International GmbH
//! The listing envelope: the shared shape plus a total, and nothing else.

use serde_json::json;
use toolkit_odata::PageInfo;

use super::{PageDto, PageInfoDto};
use crate::domain::odata::Listing;

fn info(next: Option<&str>) -> PageInfo {
    PageInfo {
        next_cursor: next.map(str::to_owned),
        prev_cursor: None,
        limit: 2,
    }
}

#[test]
fn a_counted_listing_carries_its_total_beside_the_cursors() {
    let listing = Listing {
        items: vec![1, 2],
        page_info: info(Some("c1")),
        total_count: 5,
    };
    let page = PageDto::counted(listing, |n| n * 10);
    let body = serde_json::to_value(&page).expect("serializes");
    assert_eq!(
        body,
        json!({
            "items": [10, 20],
            "page_info": { "next_cursor": "c1", "prev_cursor": null, "limit": 2, "total_count": 5 }
        })
    );
}

#[test]
fn an_uncounted_page_leaves_the_total_out_rather_than_lying_with_a_zero() {
    let page = PageDto::uncounted(vec!["a"], info(None));
    let body = serde_json::to_value(&page).expect("serializes");
    assert_eq!(
        body["page_info"],
        json!({ "next_cursor": null, "prev_cursor": null, "limit": 2 }),
        "no `total_count` key at all: {body}"
    );
}

#[test]
fn a_whole_listing_is_one_page_whose_total_is_its_length() {
    let page = PageDto::whole(vec!["a", "b", "c"]);
    assert_eq!(
        page.page_info,
        PageInfoDto {
            next_cursor: None,
            prev_cursor: None,
            limit: 3,
            total_count: Some(3),
        }
    );
    // An empty one still names a page size the contract allows.
    let empty: PageDto<&str> = PageDto::whole(Vec::new());
    assert_eq!(empty.page_info.limit, 1);
    assert_eq!(empty.page_info.total_count, Some(0));
}

#[test]
fn the_schema_registers_the_item_type_and_the_page_info() {
    use utoipa::ToSchema;
    let mut schemas = Vec::new();
    <PageDto<crate::api::rest::dto::CategoryDto> as ToSchema>::schemas(&mut schemas);
    let names: Vec<&str> = schemas.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"CategoryDto"), "{names:?}");
    assert!(names.contains(&"CountedPageInfoDto"), "{names:?}");
    assert_eq!(
        <PageDto<crate::api::rest::dto::CategoryDto> as ToSchema>::name().as_ref(),
        "CountedPage_CategoryDto"
    );
}
