use crate::storage::ecdsa_address;
use ic_cdk::query;

/// The EVM address this canister signs from, in its EIP-55 checksum, or nothing before it
/// has been derived. World-readable: it is the canister the Swapic contracts on every chain
/// name, and ops fund it before the first transaction.
///
/// Derived once from the canister's threshold key and then read from stable memory, so it
/// is fixed for the life of the canister. `derive_evm_address` is the door that derives it.
#[query]
pub fn evm_address() -> Option<String> {
    ecdsa_address::get().map(|address| address.to_string())
}
