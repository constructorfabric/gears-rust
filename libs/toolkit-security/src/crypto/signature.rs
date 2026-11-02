//! RSA and ECDSA signatures through the selected native provider.

pub use super::ecdsa::{
    EcdsaAlgorithm, EcdsaP256KeyPair, EcdsaP256VerifyingKey, verify_ecdsa, verify_ecdsa_p256,
};
pub use super::rsa::{RsaAlgorithm, verify_rsa_components};
