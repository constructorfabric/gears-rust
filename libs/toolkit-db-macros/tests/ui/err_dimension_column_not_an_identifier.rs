// A dimension column name that cannot become a column variant is reported, not
// a macro panic.
//
// The `pep_prop` fixture next to this one covers the custom properties. The
// four dimensions reach `syn::Ident::new` the same way, through
// `generate_col_impl` and `generate_scope_properties`, and used to abort
// expansion with a bare `proc macro panicked` and no span.

use toolkit_db_macros::Scopable;

#[derive(Scopable)]
#[secure(
    tenant_col = "2fa_id",
    resource_col = "id",
    no_owner,
    no_type
)]
struct Model;
