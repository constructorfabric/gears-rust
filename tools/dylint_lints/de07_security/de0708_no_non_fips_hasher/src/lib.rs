#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_ast;

use clippy_utils::diagnostics::span_lint_and_then;
use lint_utils::{is_in_hasher_allow_list, use_tree_to_strings};
use rustc_ast::{Item, ItemKind};
use rustc_lint::{EarlyLintPass, LintContext};

dylint_linting::declare_early_lint! {
    /// ### What it does
    ///
    /// Prohibits imports of non-FIPS-validated hash crates (`sha2`, `sha1`, `md5`)
    /// outside an explicit allow-list of source files.
    ///
    /// ### Why is this bad?
    ///
    /// These crates use pure-Rust RustCrypto implementations that are not
    /// FIPS-validated. While they are Phase B entries in `deny-fips.toml`
    /// (present in the dependency graph via transitives), new *direct* usage
    /// should not be introduced without review.
    ///
    /// ### Known Exclusions
    ///
    /// None — all direct call sites have been replaced. The allow-list in
    /// `lint_utils::is_in_hasher_allow_list` is empty but can be extended
    /// if a legitimate usage is introduced.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// // Bad — direct sha2 import in application code
    /// use sha2::{Digest, Sha256};
    /// ```
    ///
    /// Use instead: request a review and add the file to the DE0708 allow-list
    /// in `lint_utils::is_in_hasher_allow_list` if the usage is non-cryptographic,
    /// or route the operation through the validated crypto provider.
    pub DE0708_NO_NON_FIPS_HASHER,
    Deny,
    "non-FIPS-validated hasher import (sha2/sha1/md5) outside allow-list (DE0708)"
}

/// Crate names to detect (as they appear in `use` statements — hyphens become underscores).
const BANNED_CRATES: &[&str] = &["sha2", "sha1", "md5"];

/// Check if a resolved use-path matches one of the banned hasher crates.
fn is_banned_path(path: &str) -> bool {
    BANNED_CRATES
        .iter()
        .any(|crate_name| path == *crate_name || path.starts_with(&format!("{}::", crate_name)))
}

/// Find the first banned path in a use tree (handles grouped imports).
fn find_banned_path(tree: &rustc_ast::UseTree) -> Option<String> {
    use_tree_to_strings(tree)
        .into_iter()
        .find(|path| is_banned_path(path))
}

impl EarlyLintPass for De0708NoNonFipsHasher {
    fn check_item(&mut self, cx: &rustc_lint::EarlyContext<'_>, item: &Item) {
        // Skip files in the allow-list
        if is_in_hasher_allow_list(cx.sess().source_map(), item.span) {
            return;
        }

        let ItemKind::Use(use_tree) = &item.kind else {
            return;
        };

        if let Some(path_str) = find_banned_path(use_tree) {
            span_lint_and_then(
                cx,
                DE0708_NO_NON_FIPS_HASHER,
                item.span,
                format!("non-FIPS-validated hasher import detected: `{path_str}` (DE0708)"),
                |diag| {
                    diag.help(
                        "these crates use pure-Rust RustCrypto; add to the DE0708 allow-list if usage is non-cryptographic",
                    );
                    diag.note("see docs/security/SECURITY.md §9 for the FIPS dependency policy");
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ui_examples() {
        dylint_testing::ui_test_examples(env!("CARGO_PKG_NAME"));
    }

    #[test]
    fn test_comment_annotations_match_stderr() {
        let ui_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
        lint_utils::test_comment_annotations_match_stderr(&ui_dir, "DE0708", "non-FIPS hasher");
    }
}
