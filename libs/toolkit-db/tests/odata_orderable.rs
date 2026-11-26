//! `FieldToColumn::is_orderable` defaults to the field's own `FilterField::orderable`, the flag
//! `with_odata_orderby` publishes from, so a mapper that does not override it refuses in
//! `$orderby` exactly the fields the contract leaves out.

use toolkit_db::odata::FieldToColumn;
use toolkit_odata::filter::{FieldKind, FilterField};

mod ent {
    use sea_orm::entity::prelude::*;

    #[derive(Debug, Clone, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "odata_orderable")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub region: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Field {
    Id,
    Region,
}

impl FilterField for Field {
    const FIELDS: &'static [Self] = &[Self::Id, Self::Region];

    fn name(&self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Region => "region",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::Id => FieldKind::I64,
            Self::Region => FieldKind::String,
        }
    }

    fn orderable(&self) -> bool {
        matches!(self, Self::Id)
    }
}

struct Mapper;

impl FieldToColumn<Field> for Mapper {
    type Column = ent::Column;

    fn map_field(field: Field) -> ent::Column {
        match field {
            Field::Id => ent::Column::Id,
            Field::Region => ent::Column::Region,
        }
    }
}

#[test]
fn the_default_mapper_check_follows_the_field_flag() {
    assert!(Mapper::is_orderable(Field::Id));
    assert!(!Mapper::is_orderable(Field::Region));
}
