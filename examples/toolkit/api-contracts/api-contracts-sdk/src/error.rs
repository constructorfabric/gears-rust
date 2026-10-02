//! Error types for the `PaymentApi` contract.

use toolkit_canonical_errors_macro::resource_error;

/// Resource error constructors for payment operations.
///
/// Generates typed constructors like `PaymentResourceError::not_found(detail)`.
#[resource_error(gts_id!("cf.demo.api_contracts.payment.v1~"))]
pub struct PaymentResourceError;
