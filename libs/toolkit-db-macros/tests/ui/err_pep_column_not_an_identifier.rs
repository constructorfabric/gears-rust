// A pep_prop column name that cannot become a column variant is reported, not
// a macro panic.
//
// `syn::Ident::new` panics on an invalid identifier, which used to abort
// expansion with a bare `proc macro panicked` and no span.

use toolkit_db_macros::Scopable;

#[derive(Scopable)]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "id",
    no_owner,
    no_type,
    pep_prop(second_factor = "2fa_col")
)]
struct Model;
