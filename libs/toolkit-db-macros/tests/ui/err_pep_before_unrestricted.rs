// pep_prop cannot be used with unrestricted, whichever order they are written in.
//
// The parse-time guard reads `config.unrestricted`, so it only fires for the
// `unrestricted`-first spelling that err_unrestricted_with_pep.rs covers. Written
// this way round, the property used to be dropped in silence and the entity came
// out fully unscoped; `validate_config` now refuses it.

use toolkit_db_macros::Scopable;

#[derive(Scopable)]
#[secure(pep_prop(custom = "custom_col"), unrestricted)]
struct Model;
