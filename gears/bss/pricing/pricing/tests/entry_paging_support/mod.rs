//! Shared bodies of the book's entries pager suites (D-483) on both dialects: a book whose
//! entries cross every boundary the order `(sku_id, charge_kind, model, id)` has — inside a
//! recurring group, where month and year entries of one model are told apart by the id alone,
//! and inside a usage SKU, where the model decides — walked page by page with `next_cursor`.
#![allow(dead_code)]
#![allow(clippy::expect_used, clippy::unwrap_used)]
use crate::book_support::{get, ok, stored_book};
use crate::plan_support::{Fixture, scope};
use bss_pricing::infra::storage::{entity::price_book_entry, repo::price_book_entry_repo};
use serde_json::Value;
use uuid::Uuid;

/// One entry of `book` written straight through the repository, with the id, SKU, charge kind,
/// period, model and reference state a test chooses.
#[allow(
    clippy::too_many_arguments,
    reason = "every column the pager orders or filters by is the test's choice"
)]
pub async fn entry_at(
    f: &Fixture,
    book: Uuid,
    id: Uuid,
    sku: Uuid,
    charge_kind: &str,
    period: Option<&str>,
    model: &str,
    reference_state: &str,
) -> Uuid {
    let now = time::OffsetDateTime::now_utc();
    price_book_entry_repo::insert(
        &f.db.conn().unwrap(),
        &scope(f),
        price_book_entry::Model {
            id,
            tenant_id: f.ctx.subject_tenant_id(),
            book_id: book,
            sku_id: sku,
            charge_kind: charge_kind.into(),
            period: period.map(str::to_owned),
            model: model.into(),
            usage_policy_id: None,
            usage_policy_version: None,
            usage_policy_digest: None,
            usage_sku_version: None,
            dimension_key: None,
            invoice_line_override: None,
            reservation_id: Uuid::new_v4(),
            reference_state: reference_state.into(),
            version: 1,
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap()
    .id
}

/// A random base whose low 16 bits are zero: `base | n` orders as `n` does, and two tests on one
/// database never share an id.
fn base() -> u128 {
    Uuid::new_v4().as_u128() & !0xffff
}

/// The seeded book: its id, its entries in the list's order, and the SKUs and entries a filter
/// names.
pub struct Seeded {
    pub book: Uuid,
    /// The entry ids in `(sku_id, charge_kind, model, id)` order.
    pub ordered: Vec<Uuid>,
    /// The entry ids in the order D-483 replaces, `(sku_id, charge_kind, period or "", id)`.
    pub old_order: Vec<Uuid>,
    pub usage_sku: Uuid,
    pub recurring_sku: Uuid,
    pub one_time_sku: Uuid,
    /// The one entry in the `lost` reference state.
    pub lost: Uuid,
    /// The entries in `confirmation_pending`.
    pub pending: Vec<Uuid>,
}

/// One seeded entry: its id's low bits, SKU, charge kind, period, model and reference state.
type Seed<'a> = (u128, Uuid, &'a str, Option<&'a str>, &'a str, &'a str);
/// One seeded entry by its order keys: SKU, charge kind, model, period and id.
type Keyed<'a> = (Uuid, &'a str, &'a str, Option<&'a str>, Uuid);

/// A book of nine entries over three SKUs (usage, then recurring, then one-time in SKU order),
/// whose ids run against the order: within the recurring SKU the year `flat` entry has the lowest
/// id and the month `per_unit` one the highest, and the usage SKU's `per_unit` entry sorts after
/// its `graduated` one though its id is lower.
pub async fn seeded(f: &Fixture) -> Seeded {
    let book = stored_book(f, "paging", time::OffsetDateTime::now_utc()).await;
    let (skus, ids) = (base(), base());
    let usage_sku = Uuid::from_u128(skus | 0x10);
    let recurring_sku = Uuid::from_u128(skus | 0x20);
    let one_time_sku = Uuid::from_u128(skus | 0x30);
    let id = |n: u128| Uuid::from_u128(ids | n);
    let rows: [Seed; 9] = [
        (0x14, usage_sku, "usage", None, "volume", "confirmed"),
        (0x11, usage_sku, "usage", None, "per_unit", "lost"),
        (0x12, usage_sku, "usage", None, "graduated", "confirmed"),
        (
            0x05,
            recurring_sku,
            "recurring",
            Some("month"),
            "per_unit",
            "confirmed",
        ),
        (
            0x01,
            recurring_sku,
            "recurring",
            Some("year"),
            "flat",
            "confirmed",
        ),
        (
            0x03,
            recurring_sku,
            "recurring",
            Some("month"),
            "flat",
            "confirmation_pending",
        ),
        (
            0x02,
            recurring_sku,
            "recurring",
            Some("year"),
            "per_unit",
            "confirmed",
        ),
        (
            0x21,
            one_time_sku,
            "one_time",
            None,
            "flat",
            "confirmation_pending",
        ),
        (
            0x22,
            one_time_sku,
            "one_time",
            None,
            "per_unit",
            "confirmed",
        ),
    ];
    for (n, sku, kind, period, model, state) in rows {
        entry_at(f, book, id(n), sku, kind, period, model, state).await;
    }
    let mut keyed: Vec<Keyed> = rows
        .iter()
        .map(|(n, sku, kind, period, model, _)| (*sku, *kind, *model, *period, id(*n)))
        .collect();
    keyed.sort_by(|a, b| (a.0, a.1, a.2, a.4).cmp(&(b.0, b.1, b.2, b.4)));
    let ordered = keyed.iter().map(|k| k.4).collect();
    keyed.sort_by(|a, b| {
        (a.0, a.1, a.3.unwrap_or(""), a.4).cmp(&(b.0, b.1, b.3.unwrap_or(""), b.4))
    });
    let old_order = keyed.iter().map(|k| k.4).collect();
    Seeded {
        book,
        ordered,
        old_order,
        usage_sku,
        recurring_sku,
        one_time_sku,
        lost: id(0x11),
        pending: vec![id(0x03), id(0x21)],
    }
}

/// `path` with `params` appended, each value percent-encoded where a filter needs it.
pub fn with(path: &str, params: &[(&str, &str)]) -> String {
    let query: Vec<String> = params
        .iter()
        .map(|(k, v)| {
            format!(
                "{k}={}",
                v.replace('%', "%25")
                    .replace(' ', "%20")
                    .replace('\'', "%27")
                    .replace(',', "%2C")
                    .replace('(', "%28")
                    .replace(')', "%29")
            )
        })
        .collect();
    if query.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{}", query.join("&"))
    }
}

/// The ids of a page's items.
pub fn ids_of(page: &Value) -> Vec<Uuid> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().parse().unwrap())
        .collect()
}

/// Every page of `GET /price-books/{book}/entries` under `params` at `limit`, followed by
/// `next_cursor` to its last page: the pages' ids, each page no longer than `limit`.
pub async fn walk(
    f: &Fixture,
    book: Uuid,
    params: &[(&str, &str)],
    limit: usize,
) -> Vec<Vec<Uuid>> {
    let path = format!("/price-books/{book}/entries");
    let limit_text = limit.to_string();
    let mut pages = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut query: Vec<(&str, &str)> = params.to_vec();
        query.push(("limit", &limit_text));
        if let Some(cursor) = &cursor {
            query.push(("cursor", cursor));
        }
        let page = ok(f, &with(&path, &query)).await;
        assert_eq!(page["page_info"]["limit"], limit, "{page}");
        let ids = ids_of(&page);
        assert!(ids.len() <= limit, "{limit}: {page}");
        pages.push(ids);
        match page["page_info"]["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => return pages,
        }
        assert!(pages.len() < 100, "the walk ends");
    }
}

/// D-483: the list is ordered `(sku_id, charge_kind, model, id)`, so within a recurring group
/// month and year entries of one model follow the id, and a usage SKU's entries follow their
/// model. Walked at every page size from 1 to past the whole book, the pages join into that one
/// order, with no entry twice and none skipped, and the last page has no `next_cursor`; a second
/// page's `prev_cursor` reads the first page again.
pub async fn the_pages_cross_every_boundary(f: &Fixture) {
    let s = seeded(f).await;
    let path = format!("/price-books/{}/entries", s.book);
    let whole = ok(f, &path).await;
    assert_eq!(ids_of(&whole), s.ordered, "the new order: {whole:#}");
    assert_ne!(
        s.ordered, s.old_order,
        "the seed tells the two orders apart"
    );
    assert!(whole["page_info"]["next_cursor"].is_null(), "{whole}");
    for limit in 1..=s.ordered.len() + 1 {
        let pages = walk(f, s.book, &[], limit).await;
        let joined: Vec<Uuid> = pages.iter().flatten().copied().collect();
        assert_eq!(joined, s.ordered, "limit {limit}: {pages:#?}");
        assert_eq!(
            pages.len(),
            s.ordered.len().div_ceil(limit),
            "limit {limit}: {pages:#?}"
        );
    }
    // A boundary inside the recurring group (flat month and flat year differ by id alone), then
    // back with `prev_cursor`.
    let first = ok(f, &with(&path, &[("limit", "4")])).await;
    let next = first["page_info"]["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    let second = ok(f, &with(&path, &[("limit", "4"), ("cursor", &next)])).await;
    assert_eq!(ids_of(&second), s.ordered[4..8].to_vec(), "{second:#}");
    let prev = second["page_info"]["prev_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    let back = ok(f, &with(&path, &[("limit", "4"), ("cursor", &prev)])).await;
    assert_eq!(ids_of(&back), ids_of(&first), "{back:#}");
}

/// D-483: `$filter` over `sku_id` (`eq`, `in`), `charge_kind`, `model` and `reference_state`,
/// alone and together, keeps the order and pages under it; a value outside a closed set, a field
/// the list does not filter by, and `id` are 400.
pub async fn the_filter_narrows_every_page(f: &Fixture) {
    let s = seeded(f).await;
    let path = format!("/price-books/{}/entries", s.book);
    let keep = |wanted: &dyn Fn(usize) -> bool| -> Vec<Uuid> {
        s.ordered
            .iter()
            .enumerate()
            .filter(|(i, _)| wanted(*i))
            .map(|(_, id)| *id)
            .collect()
    };
    // In the list's order: the usage SKU's three, the recurring SKU's four, the one-time two.
    let usage = keep(&|i| i < 3);
    let recurring = keep(&|i| (3..7).contains(&i));
    let flat = vec![s.ordered[3], s.ordered[4], s.ordered[7]];
    for (filter, expected) in [
        (format!("sku_id eq {}", s.usage_sku), usage.clone()),
        (
            format!("sku_id in ({}, {})", s.recurring_sku, s.usage_sku),
            [usage.clone(), recurring.clone()].concat(),
        ),
        ("charge_kind eq 'recurring'".to_owned(), recurring.clone()),
        (
            "charge_kind in ('usage', 'one_time')".to_owned(),
            [usage.clone(), keep(&|i| i >= 7)].concat(),
        ),
        ("model eq 'flat'".to_owned(), flat.clone()),
        ("reference_state eq 'lost'".to_owned(), vec![s.lost]),
        (
            "reference_state eq 'confirmation_pending'".to_owned(),
            vec![s.pending[0], s.pending[1]],
        ),
        (
            format!("sku_id eq {} and model ne 'per_unit'", s.recurring_sku),
            vec![s.ordered[3], s.ordered[4]],
        ),
    ] {
        let whole = ok(f, &with(&path, &[("$filter", &filter)])).await;
        assert_eq!(ids_of(&whole), expected, "{filter}: {whole:#}");
        let pages = walk(f, s.book, &[("$filter", &filter)], 1).await;
        let joined: Vec<Uuid> = pages.iter().flatten().copied().collect();
        assert_eq!(joined, expected, "{filter} paged by one");
    }
    for filter in [
        "charge_kind eq 'weekly'",
        "model in ('flat', 'tiered')",
        "reference_state eq 'unreserved'",
        "period eq 'month'",
        "book_id eq 00000000-0000-0000-0000-000000000000",
        "id eq 00000000-0000-0000-0000-000000000000",
        "sku_id eq 'not-a-uuid'",
    ] {
        let (status, body, _) = get(f, &with(&path, &[("$filter", filter)])).await;
        assert_eq!(status, 400, "{filter}: {body}");
    }
}
