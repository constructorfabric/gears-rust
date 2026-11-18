//! The history read with actor names: the field on the wire, and what a
//! request does and does not ask Account Management for.

use std::sync::{Arc, Mutex};

use account_management_sdk::{IdpUser, IdpUserFilterField, ListUsersQuery};
use async_trait::async_trait;
use serde_json::{Value, json};
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::filter::{FilterNode, ODataValue};
use toolkit_odata::{Page, PageInfo};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::infra::actor_names::ActorDirectory;
use crate::test_support::{RestHarness, TEXT, UnavailableDirectory};

/// A directory that gives every asked user one name, and records each query as
/// `(tenant, ids)`. With no name it lists nobody.
#[derive(Default)]
struct FakeAm {
    name: Option<String>,
    calls: Mutex<Vec<(Uuid, Vec<Uuid>)>>,
}

impl FakeAm {
    fn naming_everyone(name: &str) -> Arc<Self> {
        Arc::new(Self {
            name: Some(name.to_owned()),
            ..Self::default()
        })
    }

    fn calls(&self) -> Vec<(Uuid, Vec<Uuid>)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl ActorDirectory for FakeAm {
    async fn list_users(
        &self,
        _ctx: &SecurityContext,
        tenant_id: Uuid,
        query: ListUsersQuery,
    ) -> Result<Page<IdpUser>, CanonicalError> {
        let Some(FilterNode::InList {
            field: IdpUserFilterField::Id,
            values,
        }) = &query.filter
        else {
            panic!("expected an id-set lookup");
        };
        let ids: Vec<Uuid> = values
            .iter()
            .filter_map(|v| match v {
                ODataValue::Uuid(id) => Some(*id),
                _ => None,
            })
            .collect();
        self.calls.lock().unwrap().push((tenant_id, ids.clone()));
        let items = self
            .name
            .iter()
            .flat_map(|name| {
                ids.iter()
                    .map(move |id| IdpUser::new(*id, "login").with_display_name(name))
            })
            .collect();
        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: 200,
            },
        ))
    }
}

/// The id the harness gives the administrator of `tenant`.
fn admin_of(tenant: Uuid) -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_OID, tenant.as_bytes())
}

/// Write one value at the root and answer the history URI of its setting.
async fn one_written_value(h: &RestHarness) -> String {
    h.inner
        .declare_typed("contact", "cascading", json!("nobody"), TEXT, "public")
        .await;
    let key = h.inner.key("contact").to_string();
    let encoded: String = key.replace('~', "%7E");
    let written = h
        .send(
            "PUT",
            &format!("/settings-service/v1/settings/{encoded}/value"),
            Some(json!({ "value": "somebody" })),
            Some("absent"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(written.status, 200, "{}", written.body);
    format!("/settings-service/v1/settings/{encoded}/history")
}

#[tokio::test]
async fn history_names_its_actors_in_one_lookup() {
    let am = FakeAm::naming_everyone("Ada Admin");
    let h = RestHarness::with_directory(am.clone()).await;
    let admin = admin_of(h.inner.tree.root);
    let uri = one_written_value(&h).await;
    let items = h.items(&uri, h.inner.tree.root).await;
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["actor"], json!(admin.to_string()));
    assert_eq!(items[0]["actor_masked"], json!(false));
    assert_eq!(items[0]["actor_name"], json!("Ada Admin"), "{}", items[0]);
    assert_eq!(items[0]["actor_name_status"], json!("resolved"));
    assert_eq!(am.calls(), [(h.inner.tree.root, vec![admin])]);
}

#[tokio::test]
async fn a_masked_actor_reads_no_name() {
    let am = FakeAm::naming_everyone("Ada Admin");
    let h = RestHarness::without_pii_entitlement_with_directory(am.clone()).await;
    let uri = one_written_value(&h).await;
    let items = h.items(&uri, h.inner.tree.root).await;
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["actor_masked"], json!(true), "{}", items[0]);
    assert_eq!(items[0]["actor_name_status"], json!("masked"));
    assert!(items[0].get("actor_name").is_none(), "{}", items[0]);
    assert!(am.calls().is_empty(), "a masked actor is never looked up");
}

#[tokio::test]
async fn a_history_page_with_am_absent_still_answers_200_with_unavailable_names() {
    // The directory reports itself unavailable, as a hub without Account
    // Management does, and panics if it is asked anyway.
    let h = RestHarness::with_directory(Arc::new(UnavailableDirectory)).await;
    let uri = one_written_value(&h).await;
    let (status, body) = h.get(&uri, h.inner.tree.root).await;
    assert_eq!(status, 200, "{body}");
    let items = body["items"].as_array().expect("items");
    assert!(!items.is_empty(), "{body}");
    for item in items {
        assert_eq!(item["actor_name_status"], json!("unavailable"), "{item}");
        assert!(item.get("actor_name").is_none(), "{item}");
    }
}

#[tokio::test]
async fn a_record_without_a_name_has_a_status_and_no_name_key() {
    // The default directory lists nobody: Account Management answered, and
    // does not know the actor.
    let h = RestHarness::new().await;
    let uri = one_written_value(&h).await;
    let items = h.items(&uri, h.inner.tree.root).await;
    assert!(!items.is_empty());
    for item in &items {
        let object = item.as_object().expect("a record is an object");
        assert_eq!(object["actor_name_status"], json!("unknown"), "{item}");
        assert!(!object.contains_key("actor_name"), "{item}");
        assert!(!object.values().any(Value::is_null), "no `null`: {item}");
    }
}

/// Copy the one audit record of a probed harness under a new id, with
/// `overrides` as `column = expression` replacing the copied columns.
async fn copy_of_the_record(h: &RestHarness, overrides: &[(&str, &str)]) -> Uuid {
    let columns = h
        .probe_query("SELECT name FROM pragma_table_info('audit_records')")
        .await
        .expect("the table is readable");
    let names: Vec<String> = columns
        .iter()
        .map(|c| c.try_get::<String>("", "name").expect("a name"))
        .filter(|n| n != "id")
        .collect();
    let values: Vec<&str> = names
        .iter()
        .map(|n| {
            overrides
                .iter()
                .find(|(column, _)| column == n)
                .map_or(n.as_str(), |(_, expression)| *expression)
        })
        .collect();
    let copy = Uuid::new_v4();
    h.probe_exec(&format!(
        "INSERT INTO audit_records (id, {}) SELECT X'{}', {} FROM audit_records \
         WHERE actor_tenant_id IS NOT NULL AND actor_classification = 'pii' LIMIT 1",
        names.join(", "),
        copy.simple(),
        values.join(", "),
    ))
    .await;
    copy
}

#[tokio::test]
async fn a_module_and_a_record_without_an_actor_tenant_say_why_they_have_no_name() {
    let am = FakeAm::naming_everyone("Ada Admin");
    let h = RestHarness::probed_with_directory(am.clone()).await;
    let root = h.inner.tree.root;
    let uri = one_written_value(&h).await;
    // A record written before the actor tenant was recorded, and a module's.
    let legacy = copy_of_the_record(&h, &[("actor_tenant_id", "NULL")]).await;
    let module = copy_of_the_record(
        &h,
        &[
            ("actor_tenant_id", "NULL"),
            ("actor", "'settings-demo'"),
            ("actor_classification", "'public'"),
        ],
    )
    .await;

    // The platform reader is shown every actor, so neither is masked.
    let items = h.items(&uri, root).await;
    assert_eq!(items.len(), 3, "{items:?}");
    let by_id = |id: Uuid| -> &Value {
        items
            .iter()
            .find(|i| i["id"] == json!(id.to_string()))
            .unwrap_or_else(|| panic!("the copy is listed: {items:?}"))
    };
    let legacy = by_id(legacy);
    assert_eq!(legacy["actor_masked"], json!(false), "{legacy}");
    assert_eq!(legacy["actor_name_status"], json!("not_recorded"));
    assert!(legacy.get("actor_name").is_none(), "{legacy}");
    let module = by_id(module);
    assert_eq!(module["actor"], json!("settings-demo"));
    assert_eq!(module["actor_name_status"], json!("module"), "{module}");
    assert!(module.get("actor_name").is_none(), "{module}");
    // The original is named; neither copy was looked up.
    let named: Vec<&Value> = items
        .iter()
        .filter(|i| i["actor_name_status"] == json!("resolved"))
        .collect();
    assert_eq!(named.len(), 1, "{items:?}");
    assert_eq!(named[0]["actor_name"], json!("Ada Admin"));
    assert_eq!(am.calls(), [(root, vec![admin_of(root)])]);
}

#[tokio::test]
async fn a_withheld_actor_reads_masked_and_is_never_named() {
    // A tenant reader below the actor: the boundary withholds the platform
    // administrator who wrote in the tenant's scope, whatever the entitlement.
    let am = FakeAm::naming_everyone("Ada Admin");
    let h = RestHarness::with_directory(am.clone()).await;
    let (root, child) = (h.inner.tree.root, h.inner.tree.a);
    h.inner
        .declare_typed("contact", "cascading", json!("nobody"), TEXT, "public")
        .await;
    let encoded: String = h.inner.key("contact").to_string().replace('~', "%7E");
    let written = h
        .send(
            "PUT",
            &format!("/settings-service/v1/settings/{encoded}/value?tenant={child}"),
            Some(json!({ "value": "somebody" })),
            Some("absent"),
            root,
        )
        .await;
    assert_eq!(written.status, 200, "{}", written.body);
    let items = h
        .items(
            &format!("/settings-service/v1/settings/{encoded}/history?tenant={child}"),
            child,
        )
        .await;
    assert!(!items.is_empty(), "{items:?}");
    for item in &items {
        assert_eq!(item["actor_withheld"], json!(true), "{item}");
        assert_eq!(item["actor_masked"], json!(true), "{item}");
        assert_eq!(item["actor_name_status"], json!("masked"), "{item}");
        assert!(item.get("actor_name").is_none(), "{item}");
    }
    assert!(am.calls().is_empty(), "a withheld actor is never looked up");
}

#[tokio::test]
async fn a_definition_record_has_no_tenant_key() {
    let h = RestHarness::new().await;
    let root = h.inner.tree.root;
    let created = h
        .send(
            "POST",
            "/settings-service/v1/declarations",
            Some(json!({
                "value_type_id": crate::test_support::BOOL,
                "vendor": "acme",
                "name": "proxy",
                "category_id": h.inner.category_id(),
                "default_value": true,
                "scope_class": "cascading",
            })),
            None,
            root,
        )
        .await;
    assert_eq!(created.status, 201, "{}", created.body);
    let key = created.body["key"]
        .as_str()
        .expect("a key")
        .replace('~', "%7E");
    let items = h
        .items(
            &format!("/settings-service/v1/settings/{key}/history"),
            root,
        )
        .await;
    assert_eq!(items.len(), 1, "{items:?}");
    let object = items[0].as_object().expect("an object");
    assert!(!object.contains_key("tenant_id"), "{}", items[0]);
    assert!(object.contains_key("actor_name_status"), "{}", items[0]);
}

#[tokio::test]
async fn a_root_reading_a_child_scope_asks_the_tenant_the_actor_acted_from() {
    let am = FakeAm::naming_everyone("Ada Admin");
    let h = RestHarness::with_directory(am.clone()).await;
    let root = h.inner.tree.root;
    let child = h.inner.tree.a;
    h.inner
        .declare_typed("contact", "cascading", json!("nobody"), TEXT, "public")
        .await;
    let key = h.inner.key("contact").to_string();
    let encoded: String = key.replace('~', "%7E");
    let written = h
        .send(
            "PUT",
            &format!("/settings-service/v1/settings/{encoded}/value?tenant={child}"),
            Some(json!({ "value": "somebody" })),
            Some("absent"),
            root,
        )
        .await;
    assert_eq!(written.status, 200, "{}", written.body);
    let items = h
        .items(
            &format!("/settings-service/v1/settings/{encoded}/history?tenant={child}"),
            root,
        )
        .await;
    assert!(!items.is_empty(), "{items:?}");
    for item in &items {
        assert_eq!(item["actor_name"], json!("Ada Admin"), "{item}");
        assert_eq!(item["actor_name_status"], json!("resolved"), "{item}");
    }
    // The root administrator acted from the root, so AM is asked there, not in
    // the child the record is about.
    assert_eq!(am.calls(), [(root, vec![admin_of(root)])]);
}
