// The same configuration as `err_unrestricted_with_tenant.rs`, written the
// other way round.
//
// The rule has one owner, so the diagnostic no longer depends on which
// attribute came first: both files report `tenant_col`, at `tenant_col`.

use toolkit_db_macros::Scopable;

#[derive(Scopable)]
#[secure(tenant_col = "tenant_id", unrestricted)]
struct Model;
