// Created: 2026-03-13 by Constructor Tech
// Updated: 2026-03-17 by Constructor Tech
#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_ast;

use lint_utils::is_in_contract_module_ast;
use rustc_ast::{Item, ItemKind, TyKind, VisibilityKind};
use rustc_lint::{EarlyLintPass, LintContext};

dylint_linting::declare_early_lint! {
    /// ### What it does
    ///
    /// Detects `pub type X = Y` aliases where Y is a primitive-like type (Uuid, String,
    /// integer types). Such aliases provide zero compile-time type safety and should
    /// be newtypes instead.
    ///
    /// ### Why is this bad?
    ///
    /// A bare type alias is fully transparent: `TenantId` and `UserId` both resolve
    /// to `Uuid`, so the compiler accepts one where the other is expected. A newtype
    /// (`pub struct TenantId(Uuid)`) makes such confusion a hard compile error.
    /// Type aliases are useful for generics or shortening complex types, not for
    /// wrapping a single primitive.
    ///
    /// ### Known Exclusions
    ///
    /// Generic type aliases (e.g., `pub type BoxedId<T> = ...`) are not flagged.
    /// Aliases of complex types (e.g., `pub type JsonValue = serde_json::Value`) are
    /// not flagged — only primitive-like backing types are reported.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// // Bad - transparent alias; no type safety
    /// pub type TenantId = Uuid;
    /// pub type GtsId = String;
    /// pub type Port = u16;
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// // Good - newtypes provide compile-time separation
    /// pub struct TenantId(pub Uuid);
    /// pub struct GtsId(String);
    /// pub struct Port(u16);
    /// ```
    pub DE1303_NO_PRIMITIVE_TYPE_ALIAS,
    Deny,
    "pub type X = primitive is a transparent alias; use a newtype for type safety (DE1303)"
}

impl EarlyLintPass for De1303NoPrimitiveTypeAlias {
    fn check_item(&mut self, cx: &rustc_lint::EarlyContext<'_>, item: &Item) {
        // Only enforce in contract modules — SDK/contract boundaries are where
        // transparent primitive aliases cause API type-safety problems.
        if !is_in_contract_module_ast(cx, item) {
            return;
        }

        let ItemKind::TyAlias(ty_alias) = &item.kind else {
            return;
        };

        // Only flag public aliases — private helpers are internal details
        if !matches!(item.vis.kind, VisibilityKind::Public) {
            return;
        }

        let name = ty_alias.ident.name.as_str();

        // Skip generic aliases like `pub type WrappedId<T> = ...`
        if !ty_alias.generics.params.is_empty() {
            return;
        }

        // RHS must be a bare path whose last segment is a primitive-like backing type.
        // Covers UUID types, String, and all built-in primitive types. Qualified paths
        // like `uuid::Uuid` work because we only inspect the last path segment.
        const PRIMITIVE_BACKING_TYPES: &[&str] = &[
            // UUID / identifier types
            "Uuid", "Ulid",
            // String
            "String",
            // Unsigned integers
            "u8", "u16", "u32", "u64", "u128", "usize",
            // Signed integers
            "i8", "i16", "i32", "i64", "i128", "isize",
            // Floating point
            "f32", "f64",
            // Other primitives
            "bool", "char",
        ];

        let Some(ty) = &ty_alias.ty else {
            return;
        };
        let TyKind::Path(None, path) = &ty.kind else {
            return;
        };
        let Some(last_seg) = path.segments.last() else {
            return;
        };
        let backing = last_seg.ident.name.as_str();
        if !PRIMITIVE_BACKING_TYPES.contains(&backing) {
            return;
        }

        cx.span_lint(DE1303_NO_PRIMITIVE_TYPE_ALIAS, item.span, |diag| {
            diag.primary_message(format!(
                "`pub type {name} = {backing}` is a transparent alias with no type safety (DE1303)"
            ));
            diag.help(format!(
                "wrap {backing} in a newtype: `pub struct {name}(pub {backing});` or `pub struct {name}({backing});`"
            ));
            diag.note("transparent aliases provide no compile-time separation; use a newtype for distinct semantic types");
        });
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
        lint_utils::test_comment_annotations_match_stderr(&ui_dir, "DE1303", "transparent alias");
    }
}
