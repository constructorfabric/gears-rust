// An empty dimension column name is reported for the same reason: the empty
// string is not an identifier either, and `Ident::new("")` panics.

use toolkit_db_macros::Scopable;

#[derive(Scopable)]
#[secure(tenant_col = "", resource_col = "id", no_owner, no_type)]
struct Model;
