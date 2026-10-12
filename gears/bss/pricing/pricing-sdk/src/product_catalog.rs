//! The Product & SKU registry's **browse** contract: the SKUs a pick-list offers and the
//! registry-owned tax-category dictionary.
//!
//! `bss-products` provides it (`#[toolkit::provides]` on its gear: a local provider and a REST
//! client over `GET /bss-products/v1/browse`) and its browse door reads through it. No create, no
//! lifecycle, no resolution — those belong to the registry's own authoring surface
//! (`cpt-cf-bss-products-interface-authoring-publish`), and a gear that could write the catalog
//! it prices would be the second author of it.
//!
//! # Why it lives in the SDK
//!
//! The contract belongs where the registry gear can implement it without depending on
//! `bss-pricing`.
//!
//! # Failing is not the same as answering "none"
//!
//! An empty catalog and an unreachable registry are opposite facts for an operator about to type
//! a SKU id from memory, so a method that cannot answer returns a [`CanonicalError`] (per ADR
//! `cpt-cf-errors-adr-sdk-canonical-projection`), never an empty `Vec`: `ServiceUnavailable` when a
//! configured catalog did not answer ([`catalog_unreachable`]), `Internal` when its answer was
//! unusable.

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::ir::binding::{
    HttpBindingIr, HttpFieldBinding, HttpMethod, HttpMethodBindingIr, StreamFraming,
};
use toolkit_contract::ir::contract::{
    ContractIr, FieldIr, FieldRole, Idempotency, InputShape, MethodIr, MethodKind, PrimitiveType,
    TypeRef,
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// One SKU, as much of it as pricing has any business knowing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSku {
    /// The registry's id — what a plan's `sku_id` binds to.
    pub sku_id: Uuid,
    /// The operator-facing code, e.g. `COMP-VCPU-H`.
    pub sku_code: String,
    /// The human name.
    pub name: String,
    /// **The declared metering unit, and the whole reason a price picker wants
    /// this list.** A SKU that declares one is a usage SKU; one that does not is
    /// priced per period. There is no separate flag — the presence of the unit
    /// *is* the fact, which is how the registry PRD models it too.
    pub metering_unit: Option<String>,
    /// `draft` | `published` | `deprecated`, verbatim. Not an enum: the registry
    /// owns this vocabulary and a fifth state must not become a parse failure
    /// in the gear that merely displays it.
    pub status: String,
    /// The registry-owned tier, which a plan's own tier is checked against.
    pub plan_tier: Option<String>,
    /// `product` | `service` | `bundle`, verbatim — the registry owns this
    /// vocabulary as it owns `status`, and `dod-sdk-read-shape` names the
    /// member `type` on the wire. A fourth value must not become a parse
    /// failure here.
    pub sku_type: String,
    /// Whether the SKU may be sold on its own. `false` is a composition- or
    /// metering-only member — priced, never picked as a line of its own.
    pub sellable: bool,
    /// The usage collector's `UsageType` id a usage SKU's declaration carries
    /// (registry P-D-184); absent on a SKU priced per period. Present
    /// exactly when `metering_unit` is, on a well-formed registry row.
    pub usage_type_ref: Option<String>,
    /// Registry-owned withdrawal from new sale. Independent of `status`: the
    /// status word is display vocabulary, this flag is the fact a write path
    /// consults, and neither is derived from the other.
    pub deprecated: bool,
}

/// One page of a prefix search over SKU names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSkuPage {
    /// The matching SKUs in this page, in the registry's own order.
    pub items: Vec<CatalogSku>,
    /// Absent when there is no further page.
    pub next_cursor: Option<String>,
}

/// A tax-category definition owned by Product Catalog, not a tax rate.
///
/// Labels are read from the provider rather than stored alongside each assignment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogTaxCategory {
    /// Stable provider-owned code. Development providers visibly prefix demo codes.
    pub code: String,
    /// Human-readable label for the category picker.
    pub display_name: String,
}

/// The canonical error a client raises when a configured catalog did not answer.
#[must_use]
pub fn catalog_unreachable(detail: impl Into<String>) -> CanonicalError {
    CanonicalError::service_unavailable()
        .with_detail(detail)
        .create()
}

/// The registry's read contract
/// (`cpt-cf-bss-products-interface-read-model`, the browse half).
///
/// `#[toolkit::provides]` validates this trait through
/// [`product_catalog_client_v1_ir`] and
/// [`product_catalog_client_v1_rest_http_binding`]. The contract-kind suffix
/// rule (`Api` / `Backend` / `Embedded` / `Extension`) refuses
/// `#[toolkit::contract]` on this existing name; the IR below is the same
/// shape the macro would emit (`SafeRead` on every method, tenant
/// `SecurityContext` off the wire): `search_skus`'s `limit` is a `u64`, the
/// IR's `U64` (the registry takes 1 to 200 and clamps a larger one; PS-49).
/// Do not add a second trait to satisfy the suffix.
#[async_trait]
pub trait ProductCatalogClientV1: Send + Sync {
    /// The SKUs a write names. Ids the registry does not know come back absent,
    /// not as an error: `SKU_NOT_PUBLISHED` is a rule's finding, not a transport's.
    ///
    /// # Errors
    /// [`CanonicalError`] when no registry is wired, it cannot be reached, or its
    /// answer is unusable.
    async fn get_skus(
        &self,
        ctx: &SecurityContext,
        ids: &[Uuid],
    ) -> Result<Vec<CatalogSku>, CanonicalError>;

    /// The human's selector: a prefix query over the name, paged.
    ///
    /// # Errors
    /// [`CanonicalError`] when no registry is wired, it cannot be reached, or its
    /// answer is unusable.
    async fn search_skus(
        &self,
        ctx: &SecurityContext,
        q: Option<&str>,
        limit: u64,
        cursor: Option<&str>,
    ) -> Result<CatalogSkuPage, CanonicalError>;

    /// Tax-category definitions available to this tenant, in provider order.
    ///
    /// No CRUD or rate data. Implementations must not fall back to fabricated
    /// definitions when the real provider is unavailable. A validator can read
    /// this list once per operation, not once per price.
    ///
    /// # Errors
    /// [`CanonicalError`] when unconfigured, unreachable or unusable. These
    /// cases must not be interpreted as an empty valid dictionary.
    async fn list_tax_categories(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<CatalogTaxCategory>, CanonicalError>;
}

fn secctx_field() -> FieldIr {
    FieldIr {
        name: "ctx".to_owned(),
        ty: TypeRef::Named("SecurityContext".to_owned()),
        optional: false,
        role: FieldRole::SecurityContext,
    }
}

fn query_binding(field: &str) -> HttpFieldBinding {
    HttpFieldBinding::Query {
        field: field.to_owned(),
        param: field.to_owned(),
    }
}

fn unary(
    name: &str,
    fields: Vec<FieldIr>,
    output: TypeRef,
    field_bindings: Vec<HttpFieldBinding>,
) -> (MethodIr, HttpMethodBindingIr) {
    let method = MethodIr {
        name: name.to_owned(),
        kind: MethodKind::Unary,
        input: InputShape { fields },
        output,
        error: Some(TypeRef::Named("CanonicalError".to_owned())),
        idempotency: Idempotency::SafeRead,
        optional: false,
    };
    let binding = HttpMethodBindingIr {
        method_name: name.to_owned(),
        http_method: HttpMethod::Get,
        path_template: "/browse".to_owned(),
        field_bindings,
        retryable: false,
        streaming: false,
        stream_framing: StreamFraming::ServerSentEvents,
        optional: false,
    };
    (method, binding)
}

/// Contract IR for [`ProductCatalogClientV1`].
///
/// `#[toolkit::provides]` calls this as `{contract}_ir()` and runs
/// [`toolkit_contract::ir::validate_contract`].
#[must_use]
pub fn product_catalog_client_v1_ir() -> ContractIr {
    let (get_skus, _) = get_skus_ir();
    let (search_skus, _) = search_skus_ir();
    let (list_tax_categories, _) = list_tax_categories_ir();
    ContractIr {
        name: "ProductCatalogClientV1".to_owned(),
        gear: "bss-products".to_owned(),
        version: "v1".to_owned(),
        methods: vec![get_skus, search_skus, list_tax_categories],
    }
}

/// HTTP binding IR for [`ProductCatalogClientV1`].
///
/// The REST arm of products' provider calls the existing browse door
/// (`GET /bss-products/v1/browse?kind=sku`); this binding is the IR
/// `validate_http_binding` checks, not a second contract surface.
#[must_use]
pub fn product_catalog_client_v1_rest_http_binding() -> HttpBindingIr {
    let (_, get_skus) = get_skus_ir();
    let (_, search_skus) = search_skus_ir();
    let (_, list_tax_categories) = list_tax_categories_ir();
    HttpBindingIr {
        base_path: "/bss-products/v1".to_owned(),
        methods: vec![get_skus, search_skus, list_tax_categories],
    }
}

fn get_skus_ir() -> (MethodIr, HttpMethodBindingIr) {
    unary(
        "get_skus",
        vec![
            secctx_field(),
            FieldIr {
                name: "ids".to_owned(),
                ty: TypeRef::List(Box::new(TypeRef::Primitive(PrimitiveType::Uuid))),
                optional: false,
                role: FieldRole::Wire,
            },
        ],
        TypeRef::List(Box::new(TypeRef::Named("CatalogSku".to_owned()))),
        vec![query_binding("ids")],
    )
}

fn search_skus_ir() -> (MethodIr, HttpMethodBindingIr) {
    unary(
        "search_skus",
        vec![
            secctx_field(),
            FieldIr {
                name: "q".to_owned(),
                ty: TypeRef::Optional(Box::new(TypeRef::Primitive(PrimitiveType::String))),
                optional: true,
                role: FieldRole::Wire,
            },
            // `u64` in the trait, as the IR declares it (whole-branch review PS-49).
            FieldIr {
                name: "limit".to_owned(),
                ty: TypeRef::Primitive(PrimitiveType::U64),
                optional: false,
                role: FieldRole::Wire,
            },
            FieldIr {
                name: "cursor".to_owned(),
                ty: TypeRef::Optional(Box::new(TypeRef::Primitive(PrimitiveType::String))),
                optional: true,
                role: FieldRole::Wire,
            },
        ],
        TypeRef::Named("CatalogSkuPage".to_owned()),
        vec![
            query_binding("q"),
            query_binding("limit"),
            query_binding("cursor"),
        ],
    )
}

fn list_tax_categories_ir() -> (MethodIr, HttpMethodBindingIr) {
    unary(
        "list_tax_categories",
        vec![secctx_field()],
        TypeRef::List(Box::new(TypeRef::Named("CatalogTaxCategory".to_owned()))),
        Vec::new(),
    )
}

#[cfg(test)]
mod contract_ir_tests {
    use toolkit_contract::ir::{validate_contract, validate_http_binding};

    use super::{product_catalog_client_v1_ir, product_catalog_client_v1_rest_http_binding};

    #[test]
    fn the_contract_ir_and_browse_http_binding_validate() {
        let ir = product_catalog_client_v1_ir();
        validate_contract(&ir).expect("contract IR");
        validate_http_binding(&ir, &product_catalog_client_v1_rest_http_binding())
            .expect("HTTP binding IR");
    }
}
