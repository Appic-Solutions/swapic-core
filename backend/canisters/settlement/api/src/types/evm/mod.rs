use crate::types::events::Hash32;
use candid::CandidType;
use serde::Deserialize;

/// A secp256k1 signature with the recovery bit an EIP-1559 envelope carries. `s` is always
/// in the lower half of the curve's range, which is the only form EIP-2 chains accept.
#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct EcdsaSignature {
    pub r: Hash32,
    pub s: Hash32,
    pub y_parity: bool,
}

/// Why a key or a signature is not one this canister can use. A refused management call is
/// reported rather than trapped: the interface specification says a rejected signing
/// request may leave the signature in the system anyway, so a caller that retries has to
/// know it is retrying.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum EcdsaError {
    PublicKey { message: String },
    Signature { message: String },
    UnreadablePublicKey,
    UnreadableSignature,
    NoParityRecovers,
}
