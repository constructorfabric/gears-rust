//! Actor names on the reads (D-519): every actor id a read shows carries a `*_name` sibling,
//! resolved through Account Management in one lookup per response. A failing directory leaves
//! the names null and the read 200; a write answer carries no name.
#![expect(
    clippy::unwrap_used,
    reason = "a test's fixtures and reads fail the test where they fail"
)]
mod plan_support;
use bss_pricing::infra::storage::{entity::plan_item, repo::plan_item_repo};
use bss_products_sdk::{PRICING_SYSTEM_ACTOR, models::SkuType};
use bss_rest::actor_names::{ActorDirectory, IdpUser, ListUsersQuery, queried_ids};
use plan_support::{Catalog, Fixture, book, id_of, item, plan, policy_entry as entry, scope};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::{Page, PageInfo};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// The people the directory knows, every lookup it answered, and whether it is down.
#[derive(Default)]
struct People {
    names: Mutex<BTreeMap<Uuid, String>>,
    calls: Mutex<Vec<Vec<Uuid>>>,
    down: AtomicBool,
}

#[async_trait::async_trait]
impl ActorDirectory for People {
    async fn list_users(
        &self,
        _ctx: &SecurityContext,
        query: ListUsersQuery,
    ) -> Result<Page<IdpUser>, CanonicalError> {
        let ids = queried_ids(&query);
        self.calls.lock().unwrap().push(ids.clone());
        if self.down.load(Ordering::SeqCst) {
            return Err(CanonicalError::service_unavailable().create());
        }
        let names = self.names.lock().unwrap();
        let users = ids
            .iter()
            .filter_map(|id| {
                names
                    .get(id)
                    .map(|name| IdpUser::new(*id, "login").with_display_name(name))
            })
            .collect();
        Ok(Page::new(
            users,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: 200,
            },
        ))
    }
}

impl People {
    fn know(&self, id: Uuid, name: &str) {
        self.names.lock().unwrap().insert(id, name.to_owned());
    }
    fn calls(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

/// Everything the reads show, written by the author `f.ctx` (Ann) and voted on by Rob.
struct World {
    f: Fixture,
    people: Arc<People>,
    rob: SecurityContext,
    book: Uuid,
    plan: Uuid,
    revision: Uuid,
    item: Uuid,
    entry: Uuid,
    sku: Uuid,
    unit: String,
}

async fn world() -> World {
    let catalog = Arc::new(Catalog::default());
    let people = Arc::new(People::default());
    let fixture = Fixture::with_directory(catalog.clone(), people.clone()).await;
    let rob = fixture.user();
    people.know(fixture.ctx.subject_id(), "Ann Author");
    people.know(rob.subject_id(), "Rob Reviewer");
    settings_by_ann(&fixture).await;
    let eur = book(&fixture, "names").await;
    let (made, revision) = plan(&fixture, "names", eur).await;
    let sku = catalog.sku(SkuType::Usage);
    let priced = entry(&fixture, eur, sku, "usage", None).await;
    let ann_item = item(&fixture, revision, sku, Some(priced), "paid").await;
    system_item(&fixture, &catalog, eur, &ann_item).await;
    let unit = approved_price(&fixture, &rob, priced).await;
    World {
        f: fixture,
        people,
        rob,
        book: eur,
        plan: id_of(&made["id"]),
        revision,
        item: ann_item.id,
        entry: priced,
        sku,
        unit,
    }
}

/// The settings, written by Ann.
async fn settings_by_ann(fixture: &Fixture) {
    let (status, read, tag) = fixture
        .call("GET", "/settings", json!({}), None, None)
        .await;
    assert_eq!(status, 200, "{read}");
    let (status, written, _) = fixture
        .call(
            "PUT",
            "/settings",
            json!({"default_timing":"arrears","default_rounding":"half_up","invoice_line_templates":{},"currencies":[]}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(status, 200, "{written}");
}

/// A second item of `like`'s revision, on its own entry, made by pricing's system actor.
async fn system_item(fixture: &Fixture, catalog: &Catalog, book: Uuid, like: &plan_item::Model) {
    let other = catalog.sku(SkuType::Usage);
    let mut system = like.clone();
    system.id = Uuid::now_v7();
    system.sku_id = other;
    system.price_book_entry_id = Some(entry(fixture, book, other, "usage", None).await);
    system.created_by = PRICING_SYSTEM_ACTOR;
    plan_item_repo::insert_as_given(&fixture.db.conn().unwrap(), &scope(fixture), system)
        .await
        .unwrap();
}

/// A price on `priced` drafted and submitted by Ann and approved by Rob, then a second draft by
/// Ann: the unit's id.
async fn approved_price(fixture: &Fixture, rob: &SecurityContext, priced: Uuid) -> String {
    let (_, _, tag) = fixture
        .call("GET", "/approval-policy", json!({}), None, None)
        .await;
    let (status, policy, _) = fixture
        .call(
            "PUT",
            "/approval-policy",
            json!({"kind":"prices","quorum":1}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(status, 200, "{policy}");
    let today = time::OffsetDateTime::now_utc().date();
    let drafted = draft(fixture, priced, today, "price").await;
    // A write answer carries no name: no write answer names anyone (D-519).
    named(&drafted["items"][0], "created_by", None);
    let price = drafted["items"][0]["id"].as_str().unwrap().to_owned();
    let (status, receipt, _) = fixture
        .call(
            "POST",
            &format!("/prices/{price}/submit"),
            json!({}),
            None,
            Some("submit"),
        )
        .await;
    assert_eq!(status, 201, "{receipt}");
    let unit = receipt["unit"]["id"].as_str().unwrap().to_owned();
    let (status, vote, _) = fixture
        .call_as(
            rob,
            "POST",
            &format!("/approval-units/{unit}/approve"),
            json!({"generation":1}),
            None,
            Some("approve"),
        )
        .await;
    assert_eq!(status, 200, "{vote}");
    assert_eq!(vote["outcome"], "applied", "{vote}");
    draft(fixture, priced, today + time::Duration::days(30), "price-2").await;
    unit
}

/// A draft price on `priced` from `from`, by the fixture's user.
async fn draft(fixture: &Fixture, priced: Uuid, from: time::Date, key: &str) -> Value {
    let (status, drafted, _) = fixture
        .call(
            "POST",
            &format!("/price-book-entries/{priced}/prices"),
            json!({"price":{"rate":"0.10"},"eligibility":"all","effective_from":from.to_string()}),
            None,
            Some(key),
        )
        .await;
    assert_eq!(status, 201, "{drafted}");
    drafted
}

impl World {
    /// One read: 200, and exactly one directory lookup when `looked_up`.
    async fn read(&self, path: &str, looked_up: bool) -> Value {
        let before = self.people.calls();
        let (s, b, _) = self.f.call("GET", path, json!({}), None, None).await;
        assert_eq!(s, 200, "{path}: {b}");
        assert_eq!(
            self.people.calls() - before,
            usize::from(looked_up),
            "{path}: one lookup per read"
        );
        b
    }
}

/// The `*_name` sibling of `field` on `value`: present, and the given text or null.
#[track_caller]
fn named(value: &Value, field: &str, expected: Option<&str>) {
    let key = format!("{field}_name");
    let got = value
        .get(&key)
        .unwrap_or_else(|| panic!("{key} is missing: {value}"));
    assert_eq!(
        got,
        &expected.map_or(Value::Null, |name| json!(name)),
        "{key}: {value}"
    );
}

/// The row of `items` whose `id` is `entry`.
fn of_entry(items: &Value, entry: Uuid) -> &Value {
    items
        .as_array()
        .unwrap()
        .iter()
        .find(|listed| listed["id"] == json!(entry))
        .unwrap_or_else(|| panic!("entry {entry} is not listed: {items}"))
}

/// Every read the world's actors show, with the name each `*_name` is expected to carry: Ann's,
/// Rob's and the system actor's labels, or null for all of them.
async fn every_read(w: &World, ann: Option<&str>, rob: Option<&str>, system: Option<&str>) {
    let settings = w.read("/settings", true).await;
    named(&settings, "updated_by", ann);
    plan_reads(w, ann, system).await;
    let card = w.read(&format!("/approval-units/{}", w.unit), true).await;
    named(&card, "submitted_by", ann);
    named(&card["decisions"][0], "actor", rob);
    let units = w.read("/approval-units", true).await;
    named(&units["items"][0], "submitted_by", ann);
    named(&units["items"][0]["decisions"][0], "actor", rob);
    price_reads(w, ann).await;
}

/// The plan, revision and item reads.
async fn plan_reads(w: &World, ann: Option<&str>, system: Option<&str>) {
    let list = w.read("/plans", true).await;
    let row = &list["items"][0];
    named(row, "created_by", ann);
    named(&row["revisions"][0], "created_by", ann);
    named(&row["current"], "created_by", ann);
    let one = w.read(&format!("/plans/{}", w.plan), true).await;
    named(&one, "created_by", ann);
    named(&one["revisions"][0], "created_by", ann);
    named(&one["current"], "created_by", ann);
    let rev = w
        .read(&format!("/plan-revisions/{}", w.revision), true)
        .await;
    named(&rev, "created_by", ann);
    let items = rev["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{rev}");
    for listed in items {
        let by_system = listed["created_by"] == json!(PRICING_SYSTEM_ACTOR);
        named(listed, "created_by", if by_system { system } else { ann });
    }
    let one_item = w.read(&format!("/plan-items/{}", w.item), true).await;
    named(&one_item, "created_by", ann);
}

/// The reads that show prices.
async fn price_reads(w: &World, ann: Option<&str>) {
    let (entry, book) = (w.entry, w.book);
    let prices = w
        .read(&format!("/price-book-entries/{entry}/prices"), true)
        .await;
    assert_eq!(prices["items"].as_array().unwrap().len(), 2, "{prices}");
    for price in prices["items"].as_array().unwrap() {
        named(price, "created_by", ann);
    }
    let read = w.read(&format!("/price-book-entries/{entry}"), true).await;
    named(&read["current_price"], "created_by", ann);
    named(&read["next_price"], "created_by", ann);
    let entries = w.read(&format!("/price-books/{book}/entries"), true).await;
    named(
        &of_entry(&entries["items"], entry)["current_price"],
        "created_by",
        ann,
    );
    let by_sku = w
        .read(&format!("/price-book-entries?sku_id={}", w.sku), true)
        .await;
    named(&by_sku["items"][0]["current_price"], "created_by", ann);
    let export = w.read(&format!("/price-books/{book}/export"), true).await;
    let exported = export["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|listed| listed["entry"]["id"] == json!(entry))
        .unwrap();
    assert_eq!(exported["prices"].as_array().unwrap().len(), 2, "{export}");
    for price in exported["prices"].as_array().unwrap() {
        named(price, "created_by", ann);
    }
    let changes = w
        .read(&format!("/price-books/{book}/publish-changes"), true)
        .await;
    named(&changes["prices"][0]["price"], "created_by", ann);
    named(&changes["prices"][0]["before"], "created_by", ann);
}

#[tokio::test]
async fn every_read_names_its_actors_in_one_lookup() {
    let w = world().await;
    every_read(&w, Some("Ann Author"), Some("Rob Reviewer"), Some("System")).await;
    // The system actor is never asked of the directory.
    assert!(
        w.people
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|ids| !ids.contains(&PRICING_SYSTEM_ACTOR))
    );
}

#[tokio::test]
async fn a_failing_directory_leaves_every_name_null_and_the_read_200() {
    let w = world().await;
    w.people.down.store(true, Ordering::SeqCst);
    every_read(&w, None, None, Some("System")).await;
}

#[tokio::test]
async fn a_renamed_actor_reads_the_new_name_on_the_next_read() {
    let w = world().await;
    let before = w.read(&format!("/plans/{}", w.plan), true).await;
    named(&before, "created_by", Some("Ann Author"));
    w.people.know(w.f.ctx.subject_id(), "Ann Married");
    let after = w.read(&format!("/plans/{}", w.plan), true).await;
    named(&after, "created_by", Some("Ann Married"));
}

/// A conditional read of the plan list: the status, the body and the weak tag.
async fn revalidate(w: &World, path: &str, tag: Option<&str>) -> (u16, Value, String) {
    use axum::{
        body::Body,
        http::{Request, header},
    };
    use tower::ServiceExt;
    let mut request = Request::builder()
        .method("GET")
        .uri(format!("/bss-pricing/v1{path}"))
        .extension(w.f.ctx.clone());
    if let Some(tag) = tag {
        request = request.header(header::IF_NONE_MATCH, tag);
    }
    let response =
        w.f.app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
    let status = response.status().as_u16();
    let etag = response
        .headers()
        .get(header::ETAG)
        .map_or("", |v| v.to_str().unwrap())
        .to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body, etag)
}

/// D-518, D-519 (review RF-P item 9): the plan list's weak tag covers the names. A repeated read
/// under its tag is 304; after a rename the same tag reads 200 with the new name and a new tag,
/// never a stale 304 that keeps the old name.
#[tokio::test]
async fn a_rename_changes_the_plan_lists_tag() {
    let w = world().await;
    let (s, first, tag) = revalidate(&w, "/plans", None).await;
    assert_eq!(s, 200, "{first}");
    named(&first["items"][0], "created_by", Some("Ann Author"));
    assert!(tag.starts_with("W/"), "{tag}");
    let (s, _, same) = revalidate(&w, "/plans", Some(&tag)).await;
    assert_eq!(s, 304, "nothing changed");
    assert_eq!(same, tag);
    w.people.know(w.f.ctx.subject_id(), "Ann Married");
    let (s, renamed, new_tag) = revalidate(&w, "/plans", Some(&tag)).await;
    assert_eq!(s, 200, "a rename is a new body: {renamed}");
    assert_ne!(new_tag, tag);
    named(&renamed["items"][0], "created_by", Some("Ann Married"));
}

/// D-519, D-522 (review RF-P item 9): an archived book names its archiver on the archived list and
/// on its read by id; the archive's own answer names nobody, and a failing directory leaves the
/// name null on a 200.
#[tokio::test]
async fn an_archived_book_names_its_archiver() {
    let w = world().await;
    let shelved = book(&w.f, "shelved").await;
    let (_, _, tag) =
        w.f.call(
            "GET",
            &format!("/price-books/{shelved}"),
            json!({}),
            None,
            None,
        )
        .await;
    let (s, archived, _) =
        w.f.call_as(
            &w.rob,
            "POST",
            &format!("/price-books/{shelved}/archive"),
            json!({}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{archived}");
    assert_eq!(
        archived["archived_by"],
        json!(w.rob.subject_id()),
        "{archived}"
    );
    named(&archived, "archived_by", None);
    let list = "/price-books?$filter=archived%20eq%20true";
    let by_id = format!("/price-books/{shelved}");
    for (down, expected) in [(false, Some("Rob Reviewer")), (true, None)] {
        w.people.down.store(down, Ordering::SeqCst);
        let page = w.read(list, true).await;
        assert_eq!(page["items"].as_array().unwrap().len(), 1, "{page}");
        named(&page["items"][0], "archived_by", expected);
        let one = w.read(&by_id, true).await;
        named(&one, "archived_by", expected);
    }
}

#[tokio::test]
async fn a_page_of_fifty_plans_makes_one_directory_call() {
    let catalog = Arc::new(Catalog::default());
    let people = Arc::new(People::default());
    let f = Fixture::with_directory(catalog, people.clone()).await;
    let authors: Vec<SecurityContext> = (0..3).map(|_| f.user()).collect();
    for (n, who) in authors.iter().enumerate() {
        people.know(who.subject_id(), &format!("Author {n}"));
    }
    let eur = book(&f, "fifty").await;
    for n in 0..50 {
        let who = &authors[n % authors.len()];
        let (s, b, _) = f
            .call_as(
                who,
                "POST",
                "/plans",
                json!({"code":format!("P{n:02}"),"name":format!("Plan {n}"),"book_id":eur}),
                None,
                Some(&format!("plan-{n}")),
            )
            .await;
        assert_eq!(s, 201, "{b}");
        named(&b, "created_by", None);
    }
    let before = people.calls();
    let (s, b, _) = f
        .call("GET", "/plans?limit=50", json!({}), None, None)
        .await;
    assert_eq!(s, 200, "{b}");
    let items = b["items"].as_array().unwrap();
    assert_eq!(items.len(), 50);
    assert_eq!(people.calls() - before, 1, "one lookup for the whole page");
    let mut asked = people.calls.lock().unwrap().last().unwrap().clone();
    asked.sort_unstable();
    let mut expected: Vec<Uuid> = authors.iter().map(SecurityContext::subject_id).collect();
    expected.sort_unstable();
    assert_eq!(asked, expected, "the distinct authors, once each");
    for row in items {
        let by: Uuid = row["created_by"].as_str().unwrap().parse().unwrap();
        let n = authors.iter().position(|a| a.subject_id() == by).unwrap();
        named(row, "created_by", Some(&format!("Author {n}")));
    }
}

#[tokio::test]
async fn an_unregistered_account_management_reads_null_names() {
    // The default fixture's hub holds no AM client: every name is unavailable, the read 200.
    let (f, _) = plan_support::setup().await;
    let eur = book(&f, "plain").await;
    let (p, _) = plan(&f, "plain", eur).await;
    let (s, b, _) = f
        .call(
            "GET",
            &format!("/plans/{}", id_of(&p["id"])),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    named(&b, "created_by", None);
}

#[tokio::test]
async fn a_settings_read_stays_a_put_body_and_its_name_is_never_written() {
    let w = world().await;
    let (s, mut body, tag) = w.f.call("GET", "/settings", json!({}), None, None).await;
    assert_eq!(s, 200, "{body}");
    named(&body, "updated_by", Some("Ann Author"));
    // D-438: a read is a PUT body without these three; the name is accepted and ignored.
    for field in ["version", "updated_at", "updated_by"] {
        body.as_object_mut().unwrap().remove(field);
    }
    body["updated_by_name"] = json!("Mallory");
    let (s, b, _) =
        w.f.call_as(&w.rob, "PUT", "/settings", body, Some(&tag), None)
            .await;
    assert_eq!(s, 200, "{b}");
    named(&b, "updated_by", None);
    let after = w.read("/settings", true).await;
    assert_eq!(after["updated_by"], json!(w.rob.subject_id()));
    named(&after, "updated_by", Some("Rob Reviewer"));
}
