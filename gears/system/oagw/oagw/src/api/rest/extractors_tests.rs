use uuid::Uuid;

use super::parse_gts_id;
use crate::domain::gts_helpers as gts;

#[test]
fn parse_gts_id_accepts_an_id_of_the_expected_schema() {
    let id = Uuid::from_u128(42);

    let parsed = parse_gts_id(
        &gts::format_upstream_gts(id),
        gts::UPSTREAM_SCHEMA,
        "/oagw/v1/upstreams",
    )
    .expect("an upstream id parses as an upstream id");
    assert_eq!(parsed, id);
}

#[test]
fn parse_gts_id_rejects_an_id_of_another_schema() {
    let instance = "/oagw/v1/upstreams/some-id";

    let problem = parse_gts_id(
        &gts::format_route_gts(Uuid::from_u128(42)),
        gts::UPSTREAM_SCHEMA,
        instance,
    )
    .expect_err("a route id is not an upstream id");
    assert_eq!(problem.status, Some(400));
    assert_eq!(problem.instance.as_deref(), Some(instance));
}
