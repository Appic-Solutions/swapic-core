//! Threshold Ed25519: the canister's two Solana keys and their signatures (design 5.2.1). Modeled
//! on `ecdsa`: the call is made by hand because `ic_cdk 0.17`'s helper attaches a fixed amount of
//! cycles, and an under-attached signature fails outright while an over-attached one is refunded.

use candid::Principal;
use ic_cdk::api::call::call_with_payment128;
use ic_cdk::api::management_canister::schnorr::{
    schnorr_public_key, SchnorrAlgorithm, SchnorrKeyId, SchnorrPublicKeyArgument,
    SignWithSchnorrArgument, SignWithSchnorrResponse,
};
use types::solana::SolanaKeyRole;

const CYCLES_PER_SIGNATURE: u128 = 80_000_000_000;

/// The derivation path of a role. Two paths, so the key that signs outside bytes never holds an
/// upgrade or a fee.
fn path(role: SolanaKeyRole) -> Vec<Vec<u8>> {
    vec![match role {
        SolanaKeyRole::Payer => b"solana-payer".to_vec(),
        SolanaKeyRole::Admin => b"solana-admin".to_vec(),
    }]
}

fn key_id(key_name: &str) -> SchnorrKeyId {
    SchnorrKeyId {
        algorithm: SchnorrAlgorithm::Ed25519,
        name: key_name.to_string(),
    }
}

/// A role's public key.
pub async fn public_key(role: SolanaKeyRole, key_name: &str) -> Result<[u8; 32], String> {
    let (answer,) = schnorr_public_key(SchnorrPublicKeyArgument {
        canister_id: None,
        derivation_path: path(role),
        key_id: key_id(key_name),
    })
    .await
    .map_err(|(code, message)| format!("the public key: {code:?}: {message}"))?;
    answer
        .public_key
        .try_into()
        .map_err(|_| "the management canister answered a key that is not 32 bytes".to_string())
}

/// A role's signature over `message`.
///
/// A rejected call is an error and not a trap: the signature may exist in the system anyway, so a
/// caller that retries must know it is retrying.
pub async fn sign(
    role: SolanaKeyRole,
    key_name: &str,
    message: Vec<u8>,
) -> Result<[u8; 64], String> {
    let (answer,): (SignWithSchnorrResponse,) = call_with_payment128(
        Principal::management_canister(),
        "sign_with_schnorr",
        (SignWithSchnorrArgument {
            message,
            derivation_path: path(role),
            key_id: key_id(key_name),
        },),
        CYCLES_PER_SIGNATURE,
    )
    .await
    .map_err(|(code, message)| format!("the signature: {code:?}: {message}"))?;
    answer.signature.try_into().map_err(|_| {
        "the management canister answered a signature that is not 64 bytes".to_string()
    })
}
