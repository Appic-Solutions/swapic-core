//! Threshold ECDSA: the canister's own EVM address, and its signature over a transaction's
//! signing hash.
//!
//! The management canister answers r and s only, so the recovery bit an EIP-1559 envelope
//! carries is found here by trying both and keeping the one that recovers to the address
//! the canister already knows is its own. That is the pattern proven live in
//! `research/gasless_test.py`, with the guesswork taken out: the trial is decided against
//! our own key rather than against whatever a node accepts.
//!
//! Keys derive per canister id, so the address is fixed for the life of the canister: the
//! production canister id is fixed on day one, and a canister that loses its key loses the
//! Swapic contracts' canister role, the gas and the pool's USDC behind that address.

#[cfg(test)]
mod tests;

use crate::storage::{config, ecdsa_address};
use candid::Principal;
use ic_cdk::api::call::call_with_payment128;
use ic_cdk::api::management_canister::ecdsa::{
    ecdsa_public_key, EcdsaCurve, EcdsaKeyId, EcdsaPublicKeyArgument, SignWithEcdsaArgument,
    SignWithEcdsaResponse,
};
use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
use thiserror::Error;
use types::{EcdsaSignature, EvmAddress, TxHash};

/// Cycles attached to one threshold signature.
///
/// The measured mainnet price of a `key_1` signature was 26,160,133,703
/// cycles, and it scales with the signing subnet's node count, so this sits well above the
/// price rather than beside it: an over-attached call is refunded the difference
/// (measured), and an under-attached one fails outright. That is why the signing call below
/// is made by hand: `ic_cdk 0.17`'s `sign_with_ecdsa` attaches exactly 26,153,846,153,
/// which is BELOW today's measured price and would fail every production signature.
const CYCLES_PER_SIGNATURE: u128 = 80_000_000_000;

/// Why a key or a signature is not one this canister can use.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum EcdsaError {
    #[error("the management canister refused the public key: {message}")]
    PublicKey { message: String },
    #[error("the management canister refused the signature: {message}")]
    Signature { message: String },
    #[error("the management canister answered bytes that are not a public key")]
    UnreadablePublicKey,
    #[error("the management canister answered bytes that are not a signature")]
    UnreadableSignature,
    #[error("neither recovery bit recovers the signature to this canister's own address")]
    NoParityRecovers,
}

/// The key this canister signs with, named by the config: `dfx_test_key` locally,
/// `key_1` in production.
fn key_id() -> EcdsaKeyId {
    EcdsaKeyId {
        curve: EcdsaCurve::Secp256k1,
        name: config::get().ecdsa_key_name,
    }
}

/// The EVM address of a recovered or derived key.
fn address_of(key: &VerifyingKey) -> EvmAddress {
    let point = key.to_encoded_point(false);
    let uncompressed: &[u8; 65] = point
        .as_bytes()
        .try_into()
        .expect("BUG: an uncompressed SEC1 point is 65 bytes");
    EvmAddress::from_public_key(uncompressed)
}

/// A SEC1 public key as the management canister answers it.
fn public_key_of(bytes: &[u8]) -> Result<VerifyingKey, EcdsaError> {
    VerifyingKey::from_sec1_bytes(bytes).map_err(|_| EcdsaError::UnreadablePublicKey)
}

/// The key `parity` recovers `raw` to over `hash`.
fn recover_key(raw: &[u8; 64], hash: TxHash, parity: bool) -> Result<VerifyingKey, EcdsaError> {
    let signature = Signature::from_slice(raw).map_err(|_| EcdsaError::UnreadableSignature)?;
    VerifyingKey::recover_from_prehash(hash.as_ref(), &signature, RecoveryId::new(parity, false))
        .map_err(|_| EcdsaError::NoParityRecovers)
}

/// The signature an EIP-1559 envelope carries: s reflected into the lower half of the
/// range, which is the only form EIP-2 chains accept, and the recovery bit that then
/// recovers to `mine`. The reflection can flip the right bit, so the trial runs after it
/// and nothing has to track the flip.
fn signature_for(
    raw: &[u8; 64],
    hash: TxHash,
    mine: EvmAddress,
) -> Result<EcdsaSignature, EcdsaError> {
    let signature = Signature::from_slice(raw).map_err(|_| EcdsaError::UnreadableSignature)?;
    let signature = signature.normalize_s().unwrap_or(signature);
    let normalized: [u8; 64] = signature.to_bytes().into();
    let (r, s) = normalized.split_at(32);
    for parity in [false, true] {
        if recover_key(&normalized, hash, parity).is_ok_and(|key| address_of(&key) == mine) {
            return Ok(EcdsaSignature::new(
                r.try_into().expect("BUG: a serialized r is 32 bytes"),
                s.try_into().expect("BUG: a serialized s is 32 bytes"),
                parity,
            ));
        }
    }
    Err(EcdsaError::NoParityRecovers)
}

/// This canister's EVM address, derived once from its threshold key and then read from
/// stable memory. Deriving twice is harmless: the same key answers the same address, so
/// two callers that race both write the same value.
pub async fn canister_address() -> Result<EvmAddress, EcdsaError> {
    if let Some(address) = ecdsa_address::get() {
        return Ok(address);
    }
    let (key,) = ecdsa_public_key(EcdsaPublicKeyArgument {
        canister_id: None,
        derivation_path: vec![],
        key_id: key_id(),
    })
    .await
    .map_err(|(code, message)| EcdsaError::PublicKey {
        message: format!("{code:?}: {message}"),
    })?;
    let address = address_of(&public_key_of(&key.public_key)?);
    ecdsa_address::set(address);
    Ok(address)
}

/// The canister's signature over `hash`, ready to go into an envelope.
///
/// A rejected signing call is an error and not a trap, because the interface specification
/// says a `SYS_UNKNOWN` or `CANISTER_ERROR` rejection may leave the signature in the system
/// anyway: a caller that retries has to know it is retrying, which a trap would hide.
pub async fn sign(hash: TxHash) -> Result<EcdsaSignature, EcdsaError> {
    let mine = canister_address().await?;
    let (answer,): (SignWithEcdsaResponse,) = call_with_payment128(
        Principal::management_canister(),
        "sign_with_ecdsa",
        (SignWithEcdsaArgument {
            message_hash: hash.as_ref().to_vec(),
            derivation_path: vec![],
            key_id: key_id(),
        },),
        CYCLES_PER_SIGNATURE,
    )
    .await
    .map_err(|(code, message)| EcdsaError::Signature {
        message: format!("{code:?}: {message}"),
    })?;
    let raw: [u8; 64] = answer
        .signature
        .try_into()
        .map_err(|_| EcdsaError::UnreadableSignature)?;
    signature_for(&raw, hash, mine)
}

impl From<EcdsaError> for settlement_api::types::evm::EcdsaError {
    fn from(error: EcdsaError) -> Self {
        match error {
            EcdsaError::PublicKey { message } => Self::PublicKey { message },
            EcdsaError::Signature { message } => Self::Signature { message },
            EcdsaError::UnreadablePublicKey => Self::UnreadablePublicKey,
            EcdsaError::UnreadableSignature => Self::UnreadableSignature,
            EcdsaError::NoParityRecovers => Self::NoParityRecovers,
        }
    }
}

/// What the EVM's `ecrecover` answers for `digest` and `(v, r, s)`: the signer, for `v`
/// 27 or 28 and an `r` and an `s` the curve takes, in either half of the range; nothing
/// otherwise, where the precompile answers the zero address. A verifier's own rules (a
/// signature's length, a low `s`) are its caller's.
pub fn ecrecover(digest: [u8; 32], v: u8, r: [u8; 32], s: [u8; 32]) -> Option<EvmAddress> {
    let parity = match v {
        27 => false,
        28 => true,
        _ => return None,
    };
    let mut raw = [0_u8; 64];
    raw[..32].copy_from_slice(&r);
    raw[32..].copy_from_slice(&s);
    // the precompile takes s in either half, the curve library a low s only: a high s is
    // reflected and its recovery bit flipped with it, which recovers the same key
    let signature = Signature::from_slice(&raw).ok()?;
    let (raw, parity): ([u8; 64], bool) = match signature.normalize_s() {
        Some(low) => (low.to_bytes().into(), !parity),
        None => (raw, parity),
    };
    recover_key(&raw, TxHash::new(digest), parity)
        .ok()
        .map(|key| address_of(&key))
}
