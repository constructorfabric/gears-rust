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
    assert_eq!(items[0]["actor_name"], Value::Null, "{}", items[0]);
    assert!(am.calls().is_empty(), "a masked actor is never looked up");
}

#[tokio::test]
async fn a_history_page_with_am_absent_still_answers_200_with_null_names() {
    // The directory reports itself unavailable, as a hub without Account
    // Management does, and panics if it is asked anyway.
    let h = RestHarness::with_directory(Arc::new(UnavailableDirectory)).await;
    let uri = one_written_value(&h).await;
    let (status, body) = h.get(&uri, h.inner.tree.root).await;
    assert_eq!(status, 200, "{body}");
    let items = body["items"].as_array().expect("items");
    assert!(!items.is_empty(), "{body}");
    for item in items {
        assert_eq!(item["actor_name"], Value::Null, "{item}");
    }
}

#[tokio::test]
async fn the_actor_name_key_is_always_on_the_wire() {
    let h = RestHarness::new().await;
    let uri = one_written_value(&h).await;
    let items = h.items(&uri, h.inner.tree.root).await;
    for item in &items {
        let object = item.as_object().expect("a record is an object");
        assert!(object.contains_key("actor_name"), "{item}");
    }
    assert!(!items.is_empty());
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
    }
    // The root administrator acted from the root, so AM is asked there, not in
    // the child the record is about.
    assert_eq!(am.calls(), [(root, vec![admin_of(root)])]);
}
