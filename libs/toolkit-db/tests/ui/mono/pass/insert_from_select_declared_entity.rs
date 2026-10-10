//! Pass test, and the control for `tests/ui/mono/fail`: the same call compiles
//! for an entity that declared every well-known dimension unscoped.
//!
//! Without it the fail fixture could be failing for any reason. Its presence
//! is also what makes trybuild run `cargo build` rather than `cargo check`,
//! which a check evaluated at monomorphization needs.

use std::future::Future as _;

use sea_orm::entity::prelude::*;
use toolkit_db::secure::{ScopableEntity, SecureConn, secure_insert_from_select};
use toolkit_security::{AccessScope, pep_properties};

mod link {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "mono_link")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub left_id: i64,
        #[sea_orm(primary_key, auto_increment = false)]
        pub right_id: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

impl ScopableEntity for link::Entity {
    const SCOPE_PROPERTIES: &'static [(&'static str, Self::Column)] = &[];

    const UNSCOPED_DIMENSIONS: &'static [&'static str] = &[
        pep_properties::OWNER_TENANT_ID,
        pep_properties::RESOURCE_ID,
        pep_properties::OWNER_ID,
    ];

    fn type_col() -> Option<link::Column> {
        None
    }
}

/// Never called: taking it as a function pointer is what makes the compiler
/// instantiate it, and polling the future is what instantiates the body of
/// `secure_insert_from_select::<link::Entity, _>` -- the place the checks fire.
fn write(runner: &SecureConn, scope: &AccessScope) {
    let source = sea_orm::sea_query::Query::select().to_owned();
    let mut insert = std::pin::pin!(secure_insert_from_select::<link::Entity, _>(
        [link::Column::LeftId, link::Column::RightId],
        source,
        scope,
        runner,
    ));
    let _ = insert
        .as_mut()
        .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()));
}

fn main() {
    std::hint::black_box(write as fn(&SecureConn, &AccessScope));
}
