//! P-D-262: every actor id a products read shows carries a `*_name` sibling, resolved through
//! Account Management in one lookup per answer. A failing directory leaves the names null and the
//! read 200; a write answer names nobody; the system actor reads "System" and is never looked up.
//!
//! A child of the governance suite, so it drives the real doors through its `Fixture`.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use bss_rest::actor_names::{ActorDirectory, ActorNames, IdpUser, ListUsersQuery, queried_ids};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::{Page, PageInfo};

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

/// The fixture's state over its database, naming actors through `people`.
fn named_state(f: &Fixture, people: Arc<People>) -> Arc<crate::api::rest::ApiState> {
    Arc::new(crate::api::rest::ApiState {
        db: f.state.db.clone(),
        sink: f.state.sink.clone(),
        usage_type_catalog: f.state.usage_type_catalog.clone(),
        usage_type_catalog_source: f.state.usage_type_catalog_source,
        idempotency_retention_hours: f.state.idempotency_retention_hours,
        fence_ttl_minutes: f.state.fence_ttl_minutes,
        reference_principals: f.state.reference_principals.clone(),
        hub: f.state.hub.clone(),
        actor_names: ActorNames::with_directory(people, &crate::api::rest::SYSTEM_ACTORS),
    })
}

/// The fixture's doors and the derived types' over the fixture's database, naming actors through
/// `people`.
fn named_app(f: &Fixture, people: Arc<People>) -> Router {
    let state = named_state(f, people);
    let openapi = toolkit::api::OpenApiRegistryImpl::new();
    routes(state.clone(), &openapi)
        .merge(crate::api::rest::derived_usage_types::router(
            state, &openapi,
        ))
        .layer(axum::Extension(flat_in_enforcer(f.tenant)))
}

/// Everything the reads show: the fixture's SKU by Ann, submitted by Ann and approved by Rob; the
/// fixture's derived type by Dee; and a derived type by the system actor.
struct World {
    f: Fixture,
    people: Arc<People>,
    app: Router,
    unit: String,
}

const DEE: Uuid = Uuid::from_u128(7);

async fn world() -> World {
    let f = Fixture::new(1).await;
    let people = Arc::new(People::default());
    people.know(f.author.subject_id(), "Ann Author");
    people.know(f.reviewer.subject_id(), "Rob Reviewer");
    people.know(DEE, "Dee Deriver");
    let (status, u) = f.post("/submit", json!({})).await;
    assert_eq!(status, 200, "{u}");
    // A write answer names nobody (P-D-262).
    assert_eq!(u["sku"].get("created_by_name"), Some(&Value::Null), "{u}");
    assert_eq!(
        u["unit"].get("submitted_by_name"),
        Some(&Value::Null),
        "{u}"
    );
    let (status, b) = f.vote(&u, "approve", 1).await;
    assert_eq!(status, 200, "{b}");
    assert_eq!(b["outcome"], "applied", "{b}");
    system_derived_type(&f).await;
    let app = named_app(&f, people.clone());
    World {
        f,
        people,
        app,
        unit: u["unit"]["id"].as_str().unwrap().to_owned(),
    }
}

/// A derived type `system-made`, created by the system actor (the nil id, P-D-189).
async fn system_derived_type(f: &Fixture) {
    use crate::domain::derived::NewDerivedType;
    use crate::infra::storage::repo::derived_usage_type_repo as store;
    let (db, scope) = repo_connection(&f.dsn, f.tenant).await;
    let conn = db.conn().unwrap();
    let created = store::create_type(
        &conn,
        &scope,
        f.tenant,
        NewDerivedType {
            code: "system-made".into(),
            name: "System made".into(),
        },
        crate::infra::storage::repo::SYSTEM_ACTOR,
        time::OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    let meter = store::find_type(&conn, &scope, f.tenant, "meter")
        .await
        .unwrap()
        .unwrap();
    let mut version = store::list_versions(&conn, &scope, f.tenant, meter.id)
        .await
        .unwrap()
        .remove(0);
    store::insert_version(
        &conn,
        &scope,
        f.tenant,
        crate::domain::derived::NewDerivedVersion {
            type_id: created.id,
            version: 1,
            declaration_json: std::mem::take(&mut version.declaration_json),
            digest: version.digest.clone(),
            created_by: crate::infra::storage::repo::SYSTEM_ACTOR,
            created_at: time::OffsetDateTime::now_utc(),
        },
    )
    .await
    .unwrap();
}

impl World {
    /// One read: 200, and exactly one directory lookup.
    async fn read(&self, path: &str) -> Value {
        let before = self.people.calls();
        let (status, b) = call(
            &self.app,
            &self.f.author,
            Method::GET,
            path,
            json!({}),
            None,
        )
        .await;
        assert_eq!(status, 200, "{path}: {b}");
        assert_eq!(
            self.people.calls() - before,
            1,
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

/// The listed derived type with `code`.
fn derived<'a>(items: &'a Value, code: &str) -> &'a Value {
    items
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["code"] == code)
        .unwrap_or_else(|| panic!("{code} is not listed: {items}"))
}

/// Every read the world's actors show, with the names expected for Ann, Rob, Dee and the system.
async fn every_read(
    w: &World,
    ann: Option<&str>,
    rob: Option<&str>,
    dee: Option<&str>,
    system: Option<&str>,
) {
    let (sku, unit) = (w.f.id, &w.unit);
    let list = w.read("/skus").await;
    named(&list["items"][0], "created_by", ann);
    let card = w.read(&format!("/skus/{sku}")).await;
    named(&card["sku"], "created_by", ann);

    let history = w.read(&format!("/skus/{sku}/history")).await;
    let rows = history["items"].as_array().unwrap();
    assert!(rows.len() >= 3, "{history}");
    for row in rows {
        let actor: Uuid = row["actor"].as_str().unwrap().parse().unwrap();
        let expected = if actor == w.f.author.subject_id() {
            ann
        } else if actor == w.f.reviewer.subject_id() {
            rob
        } else if actor.is_nil() {
            system
        } else {
            panic!("an actor the world did not make: {row}")
        };
        named(row, "actor", expected);
    }

    let one = w.read(&format!("/approval-units/{unit}")).await;
    named(&one, "submitted_by", ann);
    named(&one["decisions"][0], "actor", rob);
    named(&one["impact_live"], "created_by", ann);
    let units = w.read("/approval-units").await;
    named(&units["items"][0], "submitted_by", ann);
    named(&units["items"][0]["decisions"][0], "actor", rob);

    let types = w.read("/derived-usage-types").await;
    let meter = derived(&types["items"], "meter");
    named(meter, "created_by", dee);
    named(&meter["latest"], "created_by", dee);
    let made = derived(&types["items"], "system-made");
    named(made, "created_by", system);
    named(&made["latest"], "created_by", system);
    let meter = w.read("/derived-usage-types/meter").await;
    named(&meter, "created_by", dee);
    named(&meter["versions"][0], "created_by", dee);
    let version = w.read("/derived-usage-types/meter/versions/1").await;
    named(&version, "created_by", dee);
}

#[tokio::test]
async fn every_read_names_its_actors_in_one_lookup() {
    let w = world().await;
    every_read(
        &w,
        Some("Ann Author"),
        Some("Rob Reviewer"),
        Some("Dee Deriver"),
        Some("System"),
    )
    .await;
    // The system actor is never asked of the directory.
    assert!(
        w.people
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|ids| !ids.contains(&Uuid::nil()))
    );
}

#[tokio::test]
async fn a_failing_directory_leaves_every_name_null_and_the_read_200() {
    let w = world().await;
    w.people.down.store(true, Ordering::SeqCst);
    every_read(&w, None, None, None, Some("System")).await;
}

#[tokio::test]
async fn a_renamed_actor_reads_the_new_name_on_the_next_read() {
    let w = world().await;
    let before = w.read(&format!("/skus/{}", w.f.id)).await;
    named(&before["sku"], "created_by", Some("Ann Author"));
    w.people.know(w.f.author.subject_id(), "Ann Married");
    let after = w.read(&format!("/skus/{}", w.f.id)).await;
    named(&after["sku"], "created_by", Some("Ann Married"));
}

#[tokio::test]
async fn without_account_management_the_names_are_null() {
    // The fixture's own state names through a hub that holds no AM client.
    let f = Fixture::new(1).await;
    let (status, b) = call(
        &f.app,
        &f.author,
        Method::GET,
        &format!("/skus/{}", f.id),
        json!({}),
        None,
    )
    .await;
    assert_eq!(status, 200, "{b}");
    named(&b["sku"], "created_by", None);
}

#[tokio::test]
async fn a_page_of_skus_by_several_authors_makes_one_directory_call() {
    use crate::domain::sku::NewSku;
    use bss_products_sdk::models::SkuType;
    let f = Fixture::new(1).await;
    let people = Arc::new(People::default());
    let authors: Vec<SecurityContext> = (0..3).map(|_| authed_ctx(f.tenant)).collect();
    for (n, who) in authors.iter().enumerate() {
        people.know(who.subject_id(), &format!("Author {n}"));
    }
    let (db, scope) = repo_connection(&f.dsn, f.tenant).await;
    let conn = db.conn().unwrap();
    for n in 0..30 {
        repo::insert_sku(
            &conn,
            &scope,
            f.tenant,
            NewSku {
                code: format!("PAGE-{n:02}"),
                name: format!("Page {n}"),
                r#type: SkuType::Recurring,
                category_id: None,
                description: String::new(),
                sellable: true,
                gl_code: None,
                tax_category: None,
                invoice_line_template: None,
                billing_timing: None,
                usage_type_ref: None,
                unit: None,
            },
            authors[n % authors.len()].subject_id(),
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    }
    let app = named_app(&f, people.clone());
    let (status, page) = call(
        &app,
        &f.author,
        Method::GET,
        "/skus?limit=50",
        json!({}),
        None,
    )
    .await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["items"].as_array().unwrap().len(), 31, "{page}");
    assert_eq!(people.calls(), 1, "one lookup for the whole page");
    let mut asked = people.calls.lock().unwrap()[0].clone();
    asked.sort_unstable();
    let mut expected: Vec<Uuid> = authors
        .iter()
        .chain([&f.author])
        .map(SecurityContext::subject_id)
        .collect();
    expected.sort_unstable();
    assert_eq!(asked, expected, "the distinct creators, once each");
    for row in page["items"].as_array().unwrap() {
        let by: Uuid = row["created_by"].as_str().unwrap().parse().unwrap();
        let expected = authors
            .iter()
            .position(|a| a.subject_id() == by)
            .map(|n| format!("Author {n}"));
        named(row, "created_by", expected.as_deref());
    }
}

/// The approvals inbox over this gear's source alone, naming actors through `people`, as the
/// facade names them.
fn named_inbox(f: &Fixture, people: Arc<People>) -> Router {
    use bss_approvals_sdk::ApprovalSourceV1;
    let source = Arc::new(
        crate::api::rest::approval_units::inbox_source::ProductsApprovalSource::new(
            named_state(f, people.clone()),
            flat_in_enforcer(f.tenant),
        ),
    );
    let hub = Arc::new(toolkit::ClientHub::new());
    hub.register_scoped::<dyn ApprovalSourceV1>(
        toolkit::client_hub::ClientScope::new("products"),
        source,
    );
    let state = bss_approvals::api::ApiState::new(vec!["products".into()], hub).with_actor_names(
        ActorNames::with_directory(people, &bss_approvals::api::SYSTEM_ACTORS),
    );
    bss_approvals::api::rest::router(Arc::new(state), &toolkit::api::OpenApiRegistryImpl::new())
}

/// AP-D-11, P-D-262: an inbox card read makes ONE lookup. This gear's source answers the card
/// unnamed and the inbox names its submitter, its voter and the live SKU's creator once.
#[tokio::test]
async fn an_inbox_card_read_makes_one_lookup() {
    let w = world().await;
    let inbox = named_inbox(&w.f, w.people.clone());
    let before = w.people.calls();
    let response = inbox
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/bss-approvals/v1/approval-units/{}", w.unit))
                .extension(w.f.author.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let card: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        w.people.calls() - before,
        1,
        "one lookup per inbox card: {card}"
    );
    named(&card, "submitted_by", Some("Ann Author"));
    named(&card["decisions"][0], "actor", Some("Rob Reviewer"));
    named(&card["subject_live"], "created_by", Some("Ann Author"));
}

/// AP-D-11, P-D-262: this gear's source declares the actors its reads name "System", pricing's
/// system actor among them, so the inbox names them as this gear does.
#[tokio::test]
async fn the_source_declares_the_system_actors_this_gear_names() {
    use bss_approvals_sdk::ApprovalSourceV1;
    let f = Fixture::new(1).await;
    let source = crate::api::rest::approval_units::inbox_source::ProductsApprovalSource::new(
        f.state.clone(),
        flat_in_enforcer(f.tenant),
    );
    assert_eq!(source.system_actors(), crate::api::rest::SYSTEM_ACTORS);
    assert!(
        source
            .system_actors()
            .contains(&bss_products_sdk::PRICING_SYSTEM_ACTOR)
    );
    assert!(source.system_actors().contains(&Uuid::nil()));
}

const ARI: Uuid = Uuid::from_u128(9);

/// P-D-263, P-D-262: an archived SKU and an archived category name the actor who archived them, on
/// the SKU card, the SKU list, the category list and the category card, each in one lookup.
#[tokio::test]
async fn an_archived_sku_and_category_name_their_archiver() {
    use crate::domain::category::NewCategory;
    use crate::domain::sku::NewSku;
    use bss_products_sdk::models::{Lifecycle, SkuType};
    let w = world().await;
    w.people.know(ARI, "Ari Archiver");
    let (db, scope) = repo_connection(&w.f.dsn, w.f.tenant).await;
    let conn = db.conn().unwrap();
    let now = time::OffsetDateTime::now_utc();
    let gone = repo::insert_sku(
        &conn,
        &scope,
        w.f.tenant,
        NewSku {
            code: "GONE".into(),
            name: "Gone".into(),
            r#type: SkuType::Recurring,
            category_id: None,
            description: String::new(),
            sellable: true,
            gl_code: None,
            tax_category: None,
            invoice_line_template: None,
            billing_timing: None,
            usage_type_ref: None,
            unit: None,
        },
        w.f.author.subject_id(),
        now,
    )
    .await
    .unwrap();
    repo::set_lifecycle(
        &conn,
        &scope,
        w.f.tenant,
        gone.id,
        &[Lifecycle::Draft],
        Lifecycle::Retired,
        now,
    )
    .await
    .unwrap();
    let gone = repo::find_sku(&conn, &scope, w.f.tenant, gone.id)
        .await
        .unwrap()
        .unwrap();
    let repo::HeadWrite::Written(_) = repo::set_sku_archived(
        &conn,
        &scope,
        w.f.tenant,
        gone.id,
        gone.revision,
        Some(ARI),
        now,
    )
    .await
    .unwrap() else {
        panic!("the SKU archive matched")
    };
    let shelved = repo::insert_category(
        &conn,
        &scope,
        w.f.tenant,
        NewCategory {
            code: "shelved".into(),
            name: "Shelved".into(),
            is_default: false,
            sort_order: 0,
        },
        now,
    )
    .await
    .unwrap();
    let Some(repo::HeadWrite::Written(shelved)) =
        repo::retire_category_if_unused(&conn, &scope, w.f.tenant, shelved.id, now)
            .await
            .unwrap()
    else {
        panic!("the category retires")
    };
    let repo::HeadWrite::Written(_) = repo::set_category_archived(
        &conn,
        &scope,
        w.f.tenant,
        shelved.id,
        shelved.version,
        Some(ARI),
        now,
    )
    .await
    .unwrap() else {
        panic!("the category archive matched")
    };

    let card = w.read(&format!("/skus/{}", gone.id)).await;
    named(&card["sku"], "archived_by", Some("Ari Archiver"));
    named(&card["sku"], "created_by", Some("Ann Author"));
    let list = w.read("/skus?$filter=archived%20eq%20true").await;
    assert_eq!(list["items"][0]["code"], "GONE", "{list}");
    named(&list["items"][0], "archived_by", Some("Ari Archiver"));
    let category = w.read(&format!("/categories/{}", shelved.id)).await;
    named(&category, "archived_by", Some("Ari Archiver"));
    let categories = w.read("/categories?$filter=archived%20eq%20true").await;
    assert_eq!(categories["items"][0]["code"], "shelved", "{categories}");
    named(&categories["items"][0], "archived_by", Some("Ari Archiver"));
}
