//! A declaring crate for the crate-local GTS collector tests: it declares its
//! own collector, depends on `toolkit-gts` under another name (`gts_kit`), and
//! mixes `local` declarations with a crate-root (global-only) one, some behind an
//! always-true and an always-false `cfg`.
#![allow(
    clippy::non_minimal_cfg,
    reason = "the constant cfgs are the point: declarations gated in and out"
)]

gts_kit::declare_gts_inventory!();

use gts_kit::{GtsInstanceId, gts_id};

#[gts_kit::local::gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("test.cf.local_fixture.widget.v1~"),
    description = "Fixture base type",
    properties = "id,name",
    base = true
)]
pub struct WidgetV1 {
    pub id: GtsInstanceId,
    pub name: String,
}

/// A generic Type Schema: collected through its `()` instantiation.
#[gts_kit::local::gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("test.cf.local_fixture.boxed.v1~"),
    description = "Fixture generic base type",
    properties = "id,content",
    base = true
)]
pub struct BoxedV1<P> {
    pub id: GtsInstanceId,
    pub content: P,
}

/// Every id [`gts_declarations`] returns, sorted.
pub const DECLARED_IDS: [&str; 6] = [
    gts_id!("test.cf.local_fixture.boxed.v1~"),
    gts_id!("test.cf.local_fixture.cfg_on_type.v1~"),
    gts_id!("test.cf.local_fixture.widget.v1~"),
    gts_id!("test.cf.local_fixture.widget.v1~test.cf.local_fixture.cfg_on.v1"),
    gts_id!("test.cf.local_fixture.widget.v1~test.cf.local_fixture.raw.v1"),
    gts_id!("test.cf.local_fixture.widget.v1~test.cf.local_fixture.typed.v1"),
];

#[cfg(all())]
#[gts_kit::local::gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("test.cf.local_fixture.cfg_on_type.v1~"),
    description = "Fixture type compiled in",
    properties = "id",
    base = true
)]
pub struct CfgOnTypeV1 {
    pub id: GtsInstanceId,
}

#[cfg(any())]
#[gts_kit::local::gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("test.cf.local_fixture.cfg_off_type.v1~"),
    description = "Fixture type compiled out",
    properties = "id",
    base = true
)]
pub struct CfgOffTypeV1 {
    pub id: GtsInstanceId,
}

gts_kit::local::gts_instance! {
    WidgetV1 {
        id: gts_id!("test.cf.local_fixture.widget.v1~test.cf.local_fixture.typed.v1"),
        name: "typed".to_owned(),
    }
}

gts_kit::local::gts_instance_raw!({
    "id": gts_id!("test.cf.local_fixture.widget.v1~test.cf.local_fixture.raw.v1"),
    "name": "raw",
});

#[cfg(all())]
gts_kit::local::gts_instance_raw!({
    "id": gts_id!("test.cf.local_fixture.widget.v1~test.cf.local_fixture.cfg_on.v1"),
    "name": "on",
});

#[cfg(any())]
gts_kit::local::gts_instance_raw!({
    "id": gts_id!("test.cf.local_fixture.widget.v1~test.cf.local_fixture.cfg_off.v1"),
    "name": "off",
});

// A crate-root macro: process-wide inventory only, never this crate's.
gts_kit::gts_instance_raw!({
    "id": gts_id!("test.cf.local_fixture.widget.v1~test.cf.local_fixture.global_only.v1"),
    "name": "global",
});
