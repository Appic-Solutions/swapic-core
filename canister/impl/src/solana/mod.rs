//! The Solana line (design 5.2). The canister holds two threshold Ed25519 keys and signs on Solana
//! only what it built itself or what its co-sign checker passed. Pure rules live beside the doors
//! that call them, so every rule is a unit test.

pub mod arrival;
pub mod cosign;
pub mod keeper;
pub mod rpc;
pub mod source;
pub mod tx;
pub mod upgrade;

use crate::relay::{sanctioned_party, Party};
use crate::storage::{config, solana_keys};
use swapic_solana_wire::pda::SwapicAddresses;
use types::solana::SolanaConfig;

/// Everything a Solana door reads besides its arguments.
pub struct Deployment {
    pub config: SolanaConfig,
    pub a: SwapicAddresses,
}

/// The deployment, or why the canister signs nothing on Solana yet.
pub fn deployment() -> Result<Deployment, String> {
    let config = config::get()
        .solana
        .ok_or("the canister has no Solana config")?;
    let (payer, admin) = solana_keys::get().ok_or("the Solana keys are not derived")?;
    let a = SwapicAddresses::derive(config.program_id, payer, admin);
    Ok(Deployment { config, a })
}

/// The addresses of a deployment, when there is one: a door that only reads them on a Solana arm.
pub fn addresses() -> Option<SwapicAddresses> {
    deployment().ok().map(|d| d.a)
}

/// What a burn into Solana is read against under `config`. No read of Solana here: a door that takes
/// the 32-byte Forwarder form reads its owner first, so a payout plan names the 65-byte form.
pub fn chains_of(config: &types::Config) -> Option<crate::relay::SolanaChains> {
    let solana = config.solana.clone()?;
    let (payer, admin) = solana_keys::get()?;
    let a = SwapicAddresses::derive(solana.program_id, payer, admin);
    Some(crate::relay::SolanaChains {
        a,
        config: solana,
        now_s: now_s(),
        forward_owner: None,
    })
}

pub fn now_s() -> u64 {
    types::Timestamp::from_nanos(ic_cdk::api::time())
        .as_secs()
        .get()
}

/// Refuses a list of parties any of which is sanctioned.
pub fn screen(parties: &[Party]) -> Result<(), String> {
    match sanctioned_party(parties) {
        Some(party) => Err(format!("{party} is sanctioned")),
        None => Ok(()),
    }
}

/// `sha256` of bytes.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    use k256::sha2::{Digest, Sha256};
    Sha256::digest(bytes).into()
}

#[cfg(test)]
pub(crate) fn config_for_tests() -> SolanaConfig {
    use types::solana::SolanaFees;
    use types::TokenAmount;
    SolanaConfig {
        program_id: [1; 32],
        sol_rpc: candid::Principal::anonymous(),
        key_name: "key_1".to_string(),
        min_network_fee: SolanaFees {
            gasless_burn: TokenAmount::from(440_000u64),
            complete_new_account: TokenAmount::from(240_000u64),
            release_new_account: TokenAmount::from(240_000u64),
            forward_relay: TokenAmount::from(130_000u64),
            reclaim: TokenAmount::from(1_000u64),
        },
        max_cu_limit: 1_400_000,
        max_cu_price: 20_000,
        max_priority_lamports: 28_000,
        loaded_data_limit: 64 << 20,
        rent_float_low: 200_000_000,
        rent_float_target: 500_000_000,
        max_complete_attempts: 3,
        max_release_attempts: 3,
        complete_margin_s: 60,
        cosign_per_signer_per_hour: 10,
        cosign_global_per_hour: 300,
        cosign_failures_per_hour: 30,
        gasless_min_usdc: 10_000_000,
        deadline_margin_s: 120,
        program_min_deadline_s: 600,
        program_min_burn: 1_000_000,
        max_fee_deci_bps: 100,
        max_forward_fee: 500_000,
        upgrade_delay_s: 172_800,
        enabled_gasless: true,
        enabled_arrivals: true,
        arrival_table: None,
        program_event_keypair_allowed: false,
        program_paused: false,
    }
}
