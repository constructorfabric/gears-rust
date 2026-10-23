//! Guards on how the category routes are declared.
//!
//! `OperationBuilder`'s typestate forces every route to *choose* an
//! authentication posture before it can register -- `authenticated`,
//! `anonymous` and `public` all satisfy `AuthSet`. It does not force the choice
//! to be authentication. A settings route served anonymously would expose the
//! platform's configuration surface, and nothing in the type system would say
//! so, which is why the choice is asserted here.

/// The route declarations, read at compile time rather than from disk.
const SOURCE: &str = include_str!("routes.rs");

/// Count lines whose code -- not prose -- is exactly `needle`.
///
/// Prose is excluded deliberately: the module doc mentions `.authenticated()`
/// when explaining why routes are built through `OperationBuilder`, and a
/// substring count would read that sentence as a sixth route.
fn code_lines_matching(needle: &str) -> usize {
    SOURCE
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("//"))
        .filter(|line| line.contains(needle))
        .count()
}

#[test]
fn every_route_requires_an_authenticated_principal() {
    let routes = code_lines_matching("OperationBuilder::");
    let authenticated = code_lines_matching(".authenticated()");
    assert!(routes > 0, "the fixture must find the route declarations");
    assert_eq!(
        authenticated, routes,
        "every route must call `.authenticated()`; {routes} routes declared, \
         {authenticated} authenticated"
    );
}

#[test]
fn no_route_opts_out_of_authentication() {
    // The two escape hatches, named explicitly so adding one is a deliberate
    // act that fails this test rather than a quiet edit nobody reviews.
    for opt_out in [".anonymous()", ".public()"] {
        assert!(
            code_lines_matching(opt_out) == 0,
            "`{opt_out}` would serve a settings route without a principal"
        );
    }
}

#[test]
fn the_list_categories_description_names_sort_order_for_filtering_and_ordering() {
    // The description is the only place a client learns which fields the
    // listing accepts; `sort_order` must be named under both options, or the
    // weight a listing is ordered by cannot be discovered.
    let start = SOURCE
        .find("settings_service.list_categories")
        .expect("the list route is declared");
    let end = start
        + SOURCE[start..]
            .find(".tag(TAG)")
            .expect("the description precedes the tag");
    // Join the string literal's continuation lines into one sentence run.
    let description = SOURCE[start..end]
        .replace("\\\n", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    let (filter, orderby) = description
        .split_once("`$orderby` over")
        .expect("the description covers `$orderby`");
    assert!(
        filter.contains("`$filter` over") && filter.contains("`sort_order`"),
        "`$filter` must list `sort_order`: {filter}"
    );
    let orderby = orderby
        .split("with cursor pagination")
        .next()
        .unwrap_or(orderby);
    assert!(
        orderby.contains("`sort_order`"),
        "`$orderby` must list `sort_order`: {orderby}"
    );
}

/// The setting-value route declarations, read at compile time as well.
const SETTING_SOURCE: &str = include_str!("setting_routes.rs");

/// One operation's builder chain, from its `OperationBuilder::` call to its
/// `.register(`, with string literals' continuation lines joined into one run.
fn operation(operation_id: &str) -> String {
    let at = SETTING_SOURCE
        .find(&format!("\"{operation_id}\""))
        .unwrap_or_else(|| panic!("`{operation_id}` is declared"));
    let start = SETTING_SOURCE[..at]
        .rfind("OperationBuilder::")
        .expect("the operation has a builder");
    let end = at
        + SETTING_SOURCE[at..]
            .find(".register(")
            .expect("the builder is registered");
    SETTING_SOURCE[start..end]
        .replace("\\\n", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn the_review_listing_is_registered_beside_the_browse_and_describes_its_cursor_and_its_filter() {
    let review = operation("settings_service.list_review");
    assert!(
        review.contains("OperationBuilder::get(\"/settings-service/v1/settings/review\")"),
        "a read on the review path: {review}"
    );
    assert!(review.contains(".authenticated()"), "{review}");
    assert!(
        review.contains("row-level cursor"),
        "the description says the cursor walks rows, not settings: {review}"
    );
    assert!(
        review.contains("`needs_review`"),
        "the description says the filter refuses `needs_review`: {review}"
    );
}

#[test]
fn the_browse_description_no_longer_says_the_filter_lists_the_flagged_rows() {
    // `needs_review eq true` narrows the browse now; the rows are the review
    // listing's, and a description that kept promising them would send a
    // client to the wrong operation.
    let browse = operation("settings_service.browse_settings");
    assert!(
        !browse.contains("flagged override rows"),
        "the browse does not list rows any more: {browse}"
    );
    assert!(browse.contains("needs_review eq true"), "{browse}");
}
