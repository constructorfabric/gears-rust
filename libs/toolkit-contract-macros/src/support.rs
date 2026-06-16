//! Shared helpers used by every macro in this crate.

use proc_macro2::TokenStream;
use quote::quote;
use syn::punctuated::Punctuated;
use syn::{Ident, Path, PathArguments, PathSegment, Token};

/// Take all segments of `path` except the last (the trait name itself):
/// `foo::bar::Baz` → `foo::bar`. A single-segment path yields an empty path so
/// a subsequent [`append_segment`] resolves against the call-site scope.
///
/// # Errors
/// Returns an error if `path` has no segments.
pub fn parent_module(path: &Path) -> syn::Result<Path> {
    if path.segments.is_empty() {
        return Err(syn::Error::new_spanned(path, "contract path is empty"));
    }
    let mut segments: Punctuated<PathSegment, Token![::]> = Punctuated::new();
    let n = path.segments.len();
    for (i, seg) in path.segments.iter().enumerate() {
        if i + 1 < n {
            segments.push(seg.clone());
        }
    }
    Ok(Path {
        leading_colon: path.leading_colon,
        segments,
    })
}

/// `parent` + `ident` → `parent::ident`. An empty parent yields a bare-ident
/// path resolved against the call-site scope.
#[must_use]
pub fn append_segment(parent: &Path, ident: &Ident) -> Path {
    let mut out = parent.clone();
    out.segments.push(PathSegment {
        ident: ident.clone(),
        arguments: PathArguments::None,
    });
    out
}

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
