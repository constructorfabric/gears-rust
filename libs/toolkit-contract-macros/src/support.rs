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

/// Strip a trailing major-version marker (`V` + digits) from a trait name, so
/// the contract-type suffix rule classifies the *type*, not the version.
///
/// The suffix rule (DESIGN D6) encodes the contract type — `Api` / `Embedded` /
/// `Backend` / `Extension`. A major version is orthogonal metadata: per ADR-0007
/// a breaking change ships as a parallel trait carrying a trailing version, so
/// `PaymentApiV2` must classify exactly like `PaymentApi`.
///
/// Only a real `V<digits>` tail is removed, and never the whole name, so the
/// rule stays strict: `PaymentServiceV2` → `PaymentService`, which still has no
/// contract-type suffix and is still rejected by the caller.
///
/// ```text
/// PaymentApiV2  → PaymentApi     PaymentApi → PaymentApi (unchanged)
/// PaymentApiV10 → PaymentApi     V2         → V2         (nothing to classify)
/// Api2          → Api2           PaymentApiV → PaymentApiV
/// ```
#[must_use]
pub fn strip_version_suffix(name: &str) -> &str {
    let without_digits = name.trim_end_matches(|c: char| c.is_ascii_digit());
    // Require: digits were actually present, they are preceded by `V`, and
    // something remains in front of that `V` to classify.
    if without_digits.len() < name.len()
        && without_digits.len() > 1
        && without_digits.ends_with('V')
    {
        // `V` is ASCII, so trimming one byte is a valid char boundary.
        &without_digits[..without_digits.len() - 1]
    } else {
        name
    }
}

/// The trailing major-version marker of a trait name, lowercased so it can be
/// compared directly against the `version = "vN"` attribute spelling:
/// `PaymentApiV2` → `Some("v2")`, `PaymentApi` → `None`.
///
/// Used to keep the *third* place a version is spelled (the trait name) in
/// agreement with the declared `version` — the other two (`version` attribute
/// and the projection's `base_path`) are checked by the `require_full_coverage`
/// assertion.
#[must_use]
pub fn version_marker(name: &str) -> Option<String> {
    let stripped = strip_version_suffix(name);
    if stripped.len() == name.len() {
        None
    } else {
        Some(name[stripped.len()..].to_ascii_lowercase())
    }
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::strip_version_suffix;

    #[test]
    fn strips_trailing_major_version() {
        assert_eq!(strip_version_suffix("PaymentApiV2"), "PaymentApi");
        assert_eq!(strip_version_suffix("PaymentApiV10"), "PaymentApi");
        assert_eq!(
            strip_version_suffix("NotificationBackendV2"),
            "NotificationBackend"
        );
    }

    #[test]
    fn leaves_unversioned_names_untouched() {
        assert_eq!(strip_version_suffix("PaymentApi"), "PaymentApi");
        assert_eq!(
            strip_version_suffix("EventProducerEmbedded"),
            "EventProducerEmbedded"
        );
    }

    #[test]
    fn does_not_strip_partial_or_whole_name_matches() {
        // No `V` before the digits — not a version marker.
        assert_eq!(strip_version_suffix("Api2"), "Api2");
        // No digits — a bare trailing `V` is not a version marker.
        assert_eq!(strip_version_suffix("PaymentApiV"), "PaymentApiV");
        // Nothing would remain to classify.
        assert_eq!(strip_version_suffix("V2"), "V2");
        assert_eq!(strip_version_suffix(""), "");
    }

    #[test]
    fn stripping_keeps_the_suffix_rule_strict() {
        // The point of this test is the *classification* outcome, not the string
        // rewrite: a versioned name whose base still lacks a contract-type
        // suffix must stay unclassifiable, and a local-only contract must not be
        // widened into a remote-capable one by stripping.
        use crate::model::ContractKind;
        assert_eq!(ContractKind::from_suffix("PaymentServiceV2"), None);
        assert_eq!(
            ContractKind::from_suffix("FooEmbeddedV2"),
            Some(ContractKind::Embedded)
        );
        assert_eq!(
            ContractKind::from_suffix("FooExtensionV2"),
            Some(ContractKind::Extension)
        );
    }

    #[test]
    fn classifies_versioned_names_by_contract_type() {
        // Guards the kind itself — a trybuild fixture only proves the code
        // compiles and cannot tell `Api` from `Backend`.
        use crate::model::ContractKind;
        assert_eq!(
            ContractKind::from_suffix("PaymentApiV2"),
            Some(ContractKind::Api)
        );
        assert_eq!(
            ContractKind::from_suffix("FooBackendV10"),
            Some(ContractKind::Backend)
        );
        assert_eq!(
            ContractKind::from_suffix("PaymentApi"),
            Some(ContractKind::Api)
        );
    }

    #[test]
    fn extracts_lowercased_version_marker() {
        use super::version_marker;
        assert_eq!(version_marker("PaymentApiV2").as_deref(), Some("v2"));
        assert_eq!(version_marker("FooBackendV10").as_deref(), Some("v10"));
        // No marker: an unversioned name is unconstrained by the version check.
        assert_eq!(version_marker("PaymentApi"), None);
        assert_eq!(version_marker("V2"), None);
    }
}
