//! Compile-fail test: `secure_insert_from_select` forces the checks of
//! `ScopeProperties` for the entity it writes.
//!
//! An entity that names nothing in `SCOPE_PROPERTIES` and decides nothing in
//! `UNSCOPED_DIMENSIONS` is one nobody declared, not one without scope
//! columns. The insert-from-select gate reads the table to decide whether to
//! skip per-row validation, so it must not answer for an entity whose table
//! was never checked. `DIMENSIONS_ARE_DECLARED` fails the build here.
//!
//! The check is evaluated at monomorphization, so this fixture is only
//! meaningful while `tests/ui/mono/pass` makes trybuild run `cargo build`.

use std::future::Future as _;

use sea_orm::entity::prelude::*;
use toolkit_db::secure::{ScopableEntity, SecureConn, secure_insert_from_select};
use toolkit_security::AccessScope;

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

    const UNSCOPED_DIMENSIONS: &'static [&'static str] = &[];

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
