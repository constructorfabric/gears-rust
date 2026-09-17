// Created: 2026-09-17 by Constructor Tech
//! Wire shapes of the restriction surface: the tag a mutation must present,
//! and the difference between a stored row and an effective answer.

use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{render_readout, render_restriction};
use crate::domain::access::{
    ABSENT_RESTRICTION_TAG, AccessReadout, EffectiveAccess, Restriction, TenantAccess,
};
use crate::domain::declaration::Declaration;

const KEY: &str = "gts.cf.core.settings.setting_type.v1~acme.settings.network.proxy.v1~";

fn at(seconds: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(seconds).expect("in range")
}

fn declaration() -> Declaration {
    Declaration {
        id: Uuid::nil(),
        key: KEY.to_owned(),
        leaf_slug: "proxy".to_owned(),
        value_type_id: "gts.cf.toolkit.settings.type_bool_flag.v1~".to_owned(),
        category_id: Uuid::nil(),
        scope_class: "cascading".to_owned(),
        mode: "standard".to_owned(),
        status: "active".to_owned(),
        domain_affinity: None,
        licence_feature: None,
        owner_module: None,
        description: None,
        default_value: json!(true),
        has_secret_trait: false,
        data_classification: "public".to_owned(),
        requires_step_up: false,
        anonymous_exposable: false,
        source: "admin_authored".to_owned(),
        last_change_at: at(0),
        updated_at: at(0),
    }
}

fn restriction(access: TenantAccess, tenant: Uuid) -> Restriction {
    Restriction {
        id: Uuid::from_u128(42),
        declaration_id: Uuid::nil(),
        tenant_id: tenant,
        access,
        set_by: "root-admin".to_owned(),
        created_at: at(100),
        updated_at: at(200),
    }
}

#[test]
fn a_row_renders_its_stored_spelling_its_author_and_its_tag() {
    let tenant = Uuid::from_u128(3);
    let dto = render_restriction(&restriction(TenantAccess::ReadOnly, tenant));

    assert_eq!(dto.tenant_id, tenant);
    assert_eq!(dto.access, "read_only");
    // The administrator who decided, never the tenant that was restricted.
    assert_eq!(dto.set_by, "root-admin");
    assert_eq!(dto.updated_at, "1970-01-01T00:03:20Z");
    assert_eq!(dto.etag, at(200).unix_timestamp_nanos().to_string());

    assert_eq!(
        render_restriction(&restriction(TenantAccess::Hidden, tenant)).access,
        "hidden"
    );
}

#[test]
fn the_tag_moves_with_the_row_so_a_changed_row_fails_the_comparison() {
    let tenant = Uuid::from_u128(3);
    let mut later = restriction(TenantAccess::ReadOnly, tenant);
    later.updated_at = at(300);

    assert_ne!(
        render_restriction(&restriction(TenantAccess::ReadOnly, tenant)).etag,
        render_restriction(&later).etag
    );
}

#[test]
fn a_pair_with_no_row_still_carries_a_tag_a_first_write_can_present() {
    // Creating a row is a conditional write like any other: without a tag for
    // the absent state, two administrators could both create one and the
    // second would silently win.
    let readout = AccessReadout {
        declaration: declaration(),
        tenant_id: Uuid::from_u128(3),
        stored: None,
        effective: EffectiveAccess::OVERRIDABLE,
        etag: crate::domain::access::restriction_tag(None),
    };

    let dto = render_readout(&readout);
    assert!(dto.stored.is_none());
    assert_eq!(dto.etag, ABSENT_RESTRICTION_TAG);
    assert_eq!(dto.effective.access, "overridable");
    assert_eq!(
        dto.effective.supplied_by, None,
        "nothing supplies the default"
    );
    // `supplied_by` is skipped rather than sent as null, so a client testing
    // for the field's presence reads the same answer as one testing its value.
    let wire = serde_json::to_value(&dto).expect("serializes");
    assert!(wire["effective"].get("supplied_by").is_none(), "{wire}");
    assert!(wire.get("stored").is_none(), "{wire}");
}

#[test]
fn an_inherited_restriction_names_the_ancestor_that_supplies_it() {
    // The pair itself holds no row: the answer comes from a strict ancestor,
    // and the client has to be able to say where, or an administrator cannot
    // tell why a tenant they may edit refuses the edit.
    let ancestor = Uuid::from_u128(1);
    let readout = AccessReadout {
        declaration: declaration(),
        tenant_id: Uuid::from_u128(3),
        stored: None,
        effective: EffectiveAccess {
            access: TenantAccess::ReadOnly,
            supplied_by: Some(ancestor),
        },
        etag: crate::domain::access::restriction_tag(None),
    };

    let dto = render_readout(&readout);
    assert_eq!(dto.key, KEY);
    assert!(
        dto.stored.is_none(),
        "the answer is not this pair's own row"
    );
    assert_eq!(dto.effective.access, "read_only");
    assert_eq!(dto.effective.supplied_by, Some(ancestor));
    assert_eq!(dto.etag, ABSENT_RESTRICTION_TAG);
}

#[test]
fn a_stored_row_and_the_readout_agree_on_the_tag() {
    // The header a mutation echoes and the tag inside the stored row are one
    // value; two spellings would make a conditional write fail at random.
    let tenant = Uuid::from_u128(3);
    let row = restriction(TenantAccess::Hidden, tenant);
    let readout = AccessReadout {
        declaration: declaration(),
        tenant_id: tenant,
        stored: Some(row.clone()),
        effective: EffectiveAccess {
            access: TenantAccess::Hidden,
            supplied_by: Some(tenant),
        },
        etag: crate::domain::access::restriction_tag(Some(&row)),
    };

    let dto = render_readout(&readout);
    let stored = dto.stored.expect("the row");
    assert_eq!(stored.etag, dto.etag);
    assert_eq!(stored.access, "hidden");
    assert_eq!(dto.effective.supplied_by, Some(tenant));
}
