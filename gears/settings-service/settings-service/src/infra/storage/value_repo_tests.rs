//! The value repository over an in-memory database: what a row-level
//! operation answers when the row it names is not there.

use sea_orm::{ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter};
use serde_json::json;
use toolkit_db::secure::SecureEntityExt;
use toolkit_odata::ODataQuery;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::{ValueRepo, flagged_declarations_at};
use crate::domain::category::visibility::DomainVisibility;
use crate::domain::error::DomainError;
use crate::domain::resolution::scope_class;
use crate::domain::value::{ReviewRequest, ValueRepository};
use crate::infra::storage::entity::setting_value::{self, Entity as ValueEntity};
use crate::test_support::ResolutionHarness;

#[tokio::test]
async fn flagging_a_row_that_is_not_there_is_not_found_not_success() {
    let h = ResolutionHarness::new().await;
    let declaration = h.declare("one", scope_class::LOCAL, json!(1)).await;
    h.set(declaration, h.tree.a, json!(2)).await;
    let conn = h.db.conn().expect("connection");
    let scope = AccessScope::allow_all();

    // A row that vanished under the caller — or was never in its scope —
    // matched nothing; the caller must not be told the flag was applied.
    let missing = ValueRepo
        .flag(&conn, &scope, Uuid::new_v4(), Some("stale".to_owned()))
        .await;
    assert!(
        matches!(missing, Err(DomainError::NotFound { resource: "value" })),
        "{missing:?}"
    );

    // The row that is there takes the flag, and says why.
    let row = ValueRepo
        .find_all(&conn, &scope, declaration)
        .await
        .expect("rows")
        .remove(0);
    ValueRepo
        .flag(
            &conn,
            &scope,
            row.id,
            Some("no longer validates".to_owned()),
        )
        .await
        .expect("flagged");
    let row = ValueRepo
        .find_all(&conn, &scope, declaration)
        .await
        .expect("rows")
        .remove(0);
    assert!(row.needs_review);
    assert_eq!(
        row.needs_review_detail.as_deref(),
        Some("no longer validates")
    );
}

/// A page of the review listing over `tenants`, the caller's grant and
/// domains as given.
async fn review_page_of(
    h: &ResolutionHarness,
    scope: &AccessScope,
    visibility: &DomainVisibility,
    tenants: &[Uuid],
    limit: Option<u64>,
) -> toolkit_odata::Page<crate::domain::value::StoredValue> {
    let conn = h.db.conn().expect("connection");
    let query = ODataQuery {
        limit,
        ..ODataQuery::default()
    };
    ValueRepo
        .list_review(
            &conn,
            &ReviewRequest {
                scope,
                visibility,
                hidden_for: &[],
                tenant_ids: tenants,
                declarations: None,
                query: &query,
            },
        )
        .await
        .expect("page")
}

#[tokio::test]
async fn the_review_listing_pages_by_its_bound_and_says_there_is_more() {
    // The listing spans a page of declarations across a subtree; unbounded, a
    // broad migration makes it a million-row read. A page holds `limit` rows
    // and a cursor says the rest is there.
    let h = ResolutionHarness::new().await;
    let declaration = h.declare("flagged", scope_class::LOCAL, json!(1)).await;
    let tenants = [h.tree.root, h.tree.a, h.tree.b, h.tree.c];
    for tenant in tenants {
        h.set_flagged(declaration, tenant, json!(2)).await;
    }
    let page = review_page_of(
        &h,
        &AccessScope::allow_all(),
        &DomainVisibility::Unrestricted,
        &tenants,
        Some(2),
    )
    .await;
    assert_eq!(page.items.len(), 2, "the bound of two");
    assert!(page.page_info.next_cursor.is_some(), "and the rest follows");
}

#[tokio::test]
async fn a_narrowed_grant_leaves_the_rows_of_a_declaration_it_excludes_out_of_the_review_listing() {
    let h = ResolutionHarness::new().await;
    let kept = h.declare("kept", scope_class::LOCAL, json!(1)).await;
    let barred = h.declare("barred", scope_class::LOCAL, json!(1)).await;
    h.set_flagged(kept, h.tree.a, json!(2)).await;
    h.set_flagged(barred, h.tree.a, json!(2)).await;
    let tenants = [h.tree.root, h.tree.a];

    // A grant on one declaration only: the other's rows are not this caller's.
    let page = review_page_of(
        &h,
        &AccessScope::for_resource(kept),
        &DomainVisibility::Unrestricted,
        &tenants,
        None,
    )
    .await;
    let listed: Vec<Uuid> = page.items.iter().map(|r| r.declaration_id).collect();
    assert_eq!(listed, vec![kept]);
}

#[tokio::test]
async fn a_restricted_domain_view_leaves_the_rows_of_another_domain_out_of_the_review_listing() {
    let h = ResolutionHarness::new().await;
    let open = h.declare("open", scope_class::LOCAL, json!(1)).await;
    let walled = h.declare("walled", scope_class::LOCAL, json!(1)).await;
    h.bind_domain(walled, "commercial").await;
    h.set_flagged(open, h.tree.a, json!(2)).await;
    h.set_flagged(walled, h.tree.a, json!(2)).await;
    let tenants = [h.tree.root, h.tree.a];

    // An administrator of another domain sees the undomained setting and not
    // the one bound to `commercial`.
    let page = review_page_of(
        &h,
        &AccessScope::allow_all(),
        &DomainVisibility::Restricted(vec!["infra".to_owned()]),
        &tenants,
        None,
    )
    .await;
    let listed: Vec<Uuid> = page.items.iter().map(|r| r.declaration_id).collect();
    assert_eq!(listed, vec![open]);

    // The domain's own administrator sees both.
    let page = review_page_of(
        &h,
        &AccessScope::allow_all(),
        &DomainVisibility::Restricted(vec!["commercial".to_owned()]),
        &tenants,
        None,
    )
    .await;
    assert_eq!(page.items.len(), 2);
}

#[tokio::test]
async fn a_subject_bound_flagged_row_is_neither_listed_for_review_nor_a_flag_at_its_tenant() {
    // The writer never sets a subject, so the only way to get such a row is
    // the identity model's own track. The review listing and the narrowing
    // of the browse and the search are over the subject-less track only, and
    // a subject-bound row that stopped validating must not surface on them.
    let h = ResolutionHarness::new().await;
    let shared = h.declare("shared", scope_class::LOCAL, json!(1)).await;
    let personal = h.declare("personal", scope_class::LOCAL, json!(1)).await;
    h.set_flagged(shared, h.tree.a, json!(2)).await;
    let conn = h.db.conn().expect("connection");
    let at = time::OffsetDateTime::now_utc();
    toolkit_db::secure::secure_insert::<ValueEntity>(
        setting_value::ActiveModel {
            id: Set(Uuid::new_v4()),
            declaration_id: Set(personal),
            tenant_id: Set(h.tree.a),
            subject_type: Set(Some("gts.cf.core.security.subject_service.v1~".to_owned())),
            subject_id: Set(Some(Uuid::new_v4().to_string())),
            value: Set(Some(json!(2))),
            secret_ref: Set(None),
            data_classification: Set("public".to_owned()),
            needs_review: Set(true),
            needs_review_detail: Set(Some("no longer validates".to_owned())),
            last_change_at: Set(at),
            created_at: Set(at),
            updated_at: Set(at),
            set_by: Set("identity-model".to_owned()),
        },
        &AccessScope::allow_all(),
        &conn,
    )
    .await
    .expect("a subject-bound row");
    let tenants = [h.tree.root, h.tree.a];

    let page = review_page_of(
        &h,
        &AccessScope::allow_all(),
        &DomainVisibility::Unrestricted,
        &tenants,
        None,
    )
    .await;
    let listed: Vec<Uuid> = page.items.iter().map(|r| r.declaration_id).collect();
    assert_eq!(
        listed,
        vec![shared],
        "the subject-bound row is not a review item"
    );

    // The predicate the browse and the search narrow by reads the same track.
    let flagged: Vec<Uuid> = ValueEntity::find()
        .filter(setting_value::Column::DeclarationId.in_subquery(flagged_declarations_at(h.tree.a)))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .expect("rows")
        .into_iter()
        .map(|row| row.declaration_id)
        .collect();
    assert_eq!(flagged, vec![shared], "nor a flag at its tenant");
}

/// A subject-bound row of `declaration` at `tenant`, which no subject-less
/// read may see.
async fn insert_subject_row(
    h: &ResolutionHarness,
    declaration: Uuid,
    tenant: Uuid,
    value: serde_json::Value,
) {
    let conn = h.db.conn().expect("connection");
    let at = time::OffsetDateTime::now_utc();
    toolkit_db::secure::secure_insert::<ValueEntity>(
        setting_value::ActiveModel {
            id: Set(Uuid::new_v4()),
            declaration_id: Set(declaration),
            tenant_id: Set(tenant),
            subject_type: Set(Some("gts.cf.core.security.subject_service.v1~".to_owned())),
            subject_id: Set(Some(Uuid::new_v4().to_string())),
            value: Set(Some(value)),
            secret_ref: Set(None),
            data_classification: Set("public".to_owned()),
            needs_review: Set(false),
            needs_review_detail: Set(None),
            last_change_at: Set(at),
            created_at: Set(at),
            updated_at: Set(at),
            set_by: Set("identity-model".to_owned()),
        },
        &AccessScope::allow_all(),
        &conn,
    )
    .await
    .expect("a subject-bound row");
}

#[tokio::test]
async fn the_count_over_tenants_is_the_subjectless_rows_of_that_declaration_there_alone() {
    let h = ResolutionHarness::new().await;
    let one = h.declare("one", scope_class::LOCAL, json!(1)).await;
    let other = h.declare("other", scope_class::LOCAL, json!(1)).await;
    h.set(one, h.tree.root, json!(2)).await;
    h.set(one, h.tree.a, json!(2)).await;
    h.set(one, h.tree.b, json!(3)).await;
    h.set(other, h.tree.root, json!(2)).await;
    insert_subject_row(&h, one, h.tree.root, json!(9)).await;
    let conn = h.db.conn().expect("connection");
    let scope = AccessScope::allow_all();

    let count = |tenants: Vec<Uuid>| {
        let (conn, scope) = (&conn, &scope);
        async move {
            ValueRepo
                .count_in_tenants(conn, scope, one, &tenants)
                .await
                .expect("counted")
        }
    };
    // Other tenants (`b`), another declaration (`other`) and the subject row
    // at the root are all outside the count.
    assert_eq!(count(vec![h.tree.root, h.tree.a]).await, 2);
    assert_eq!(count(vec![h.tree.root, h.tree.a, h.tree.b]).await, 3);
    assert_eq!(count(vec![h.tree.c]).await, 0);
    assert_eq!(count(Vec::new()).await, 0);
    let found = ValueRepo
        .find_in_tenants(&conn, &scope, one, &[h.tree.root, h.tree.a, h.tree.b])
        .await
        .expect("rows");
    assert_eq!(found.len(), 3, "the count is the length of the rows");
}

#[tokio::test]
async fn the_distinct_read_groups_by_value_and_classification_with_multiplicity() {
    let h = ResolutionHarness::new().await;
    let one = h.declare("one", scope_class::LOCAL, json!(1)).await;
    let other = h.declare("other", scope_class::LOCAL, json!(1)).await;
    h.set(one, h.tree.root, json!("x")).await;
    h.set(one, h.tree.a, json!("x")).await;
    h.set(one, h.tree.b, json!("x")).await;
    h.set(one, h.tree.c, json!("y")).await;
    h.set(other, h.tree.root, json!("z")).await;
    insert_subject_row(&h, one, h.tree.s, json!("w")).await;
    // Two secret rows: no inline value, held by reference.
    let conn = h.db.conn().expect("connection");
    let at = time::OffsetDateTime::now_utc();
    let secret_tenants = [Uuid::new_v4(), Uuid::new_v4()];
    for tenant in secret_tenants {
        toolkit_db::secure::secure_insert::<ValueEntity>(
            setting_value::ActiveModel {
                id: Set(Uuid::new_v4()),
                declaration_id: Set(one),
                tenant_id: Set(tenant),
                subject_type: Set(None),
                subject_id: Set(None),
                value: Set(None),
                secret_ref: Set(Some("cred://ref".to_owned())),
                data_classification: Set("secret".to_owned()),
                needs_review: Set(false),
                needs_review_detail: Set(None),
                last_change_at: Set(at),
                created_at: Set(at),
                updated_at: Set(at),
                set_by: Set("test".to_owned()),
            },
            &AccessScope::allow_all(),
            &conn,
        )
        .await
        .expect("a secret row");
    }
    let tenants = [
        h.tree.root,
        h.tree.a,
        h.tree.b,
        h.tree.c,
        h.tree.s,
        secret_tenants[0],
        secret_tenants[1],
    ];

    let mut groups = ValueRepo
        .distinct_values_in_tenants(&conn, &AccessScope::allow_all(), one, &tenants)
        .await
        .expect("grouped");
    groups.sort_by_key(|g| (g.value.as_ref().map(ToString::to_string), g.count));
    let shape: Vec<(Option<serde_json::Value>, String, u64)> = groups
        .into_iter()
        .map(|g| (g.value, g.data_classification, g.count))
        .collect();
    let public = |value: Option<serde_json::Value>, count| (value, "public".to_owned(), count);
    let mut expected = vec![
        (None, "secret".to_owned(), 2),
        public(Some(json!("x")), 3),
        public(Some(json!("y")), 1),
    ];
    expected.sort_by_key(|(v, _, n)| (v.as_ref().map(ToString::to_string), *n));
    assert_eq!(shape, expected);
    assert!(
        ValueRepo
            .distinct_values_in_tenants(&conn, &AccessScope::allow_all(), one, &[])
            .await
            .expect("grouped")
            .is_empty()
    );
}

#[tokio::test]
async fn the_listing_projection_returns_the_columns_of_the_rows_the_full_read_returns() {
    let h = ResolutionHarness::new().await;
    let one = h.declare("one", scope_class::LOCAL, json!(1)).await;
    h.set(one, h.tree.root, json!("x")).await;
    h.set(one, h.tree.a, json!({"k": 1})).await;
    insert_subject_row(&h, one, h.tree.b, json!("w")).await;
    let conn = h.db.conn().expect("connection");
    let scope = AccessScope::allow_all();
    let tenants = [h.tree.root, h.tree.a, h.tree.b];

    let mut listed = ValueRepo
        .list_in_tenants(&conn, &scope, one, &tenants)
        .await
        .expect("listed");
    let mut full = ValueRepo
        .find_in_tenants(&conn, &scope, one, &tenants)
        .await
        .expect("rows");
    listed.sort_by_key(|r| r.tenant_id);
    full.sort_by_key(|r| r.tenant_id);
    let projected: Vec<_> = full
        .into_iter()
        .map(|r| (r.tenant_id, r.value, r.data_classification))
        .collect();
    let listed: Vec<_> = listed
        .into_iter()
        .map(|r| (r.tenant_id, r.value, r.data_classification))
        .collect();
    assert_eq!(listed, projected);
    assert_eq!(listed.len(), 2, "the subject row is not listed");
}
