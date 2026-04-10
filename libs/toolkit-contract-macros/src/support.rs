//! Shared helpers used by every macro in this crate.

use proc_macro2::TokenStream;
use quote::quote;

const CONTRACT_PKG: &str = "cf-gears-toolkit-contract";
const CONTRACT_LIB: &str = "toolkit_contract";

/// Resolve the path that user code uses to refer to the `toolkit_contract`
/// crate. Falls back to `::toolkit::contract_support` when the contract crate
/// is reachable only through the umbrella `toolkit` re-export.
pub fn contract_support_path() -> TokenStream {
    for package_name in [CONTRACT_PKG, "toolkit-contract"] {
        if let Ok(found) = proc_macro_crate::crate_name(package_name) {
            return match found {
                proc_macro_crate::FoundCrate::Itself => quote!(::toolkit_contract),
                proc_macro_crate::FoundCrate::Name(name) => {
                    let pkg_normalized = CONTRACT_PKG.replace('-', "_");
                    let effective = if name == pkg_normalized {
                        CONTRACT_LIB
                    } else {
                        &name
                    };
                    let ident = syn::Ident::new(effective, proc_macro2::Span::call_site());
                    quote!(::#ident)
                }
            };
        }
    }

    quote!(::toolkit::contract_support)
}
