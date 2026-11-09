//! The Solana line on the wire. Addresses are base58 text, case exact.

use candid::{CandidType, Nat, Principal};
use serde::{Deserialize, Serialize};
use swapic_solana_wire::base58;
use types::numeric::TokenAmount;
use types::solana::{SolanaFees, SolanaTable};

fn address(field: &str, text: &str) -> Result<[u8; 32], String> {
    base58::decode_pubkey(text).ok_or_else(|| format!("{field} is not a Solana address: {text}"))
}

/// The least network fee of each Solana action, in micro USDC.
#[derive(CandidType, Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct SolanaFeesConfig {
    pub gasless_burn: Nat,
    pub complete_new_account: Nat,
    pub release_new_account: Nat,
    pub forward_relay: Nat,
    pub reclaim: Nat,
}

/// The frozen Swapic lookup table: its address and every entry, in order.
#[derive(CandidType, Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct SolanaTableConfig {
    pub address: String,
    pub entries: Vec<String>,
}

/// The Solana line's config. Absent: the canister signs nothing on Solana.
#[derive(CandidType, Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct SolanaConfig {
    pub program_id: String,
    pub sol_rpc: Principal,
    pub key_name: String,
    pub min_network_fee: SolanaFeesConfig,
    pub max_cu_limit: u32,
    pub max_cu_price: u64,
    pub max_priority_lamports: u64,
    pub loaded_data_limit: u32,
    pub rent_float_low: u64,
    pub rent_float_target: u64,
    pub max_complete_attempts: u32,
    pub max_release_attempts: u32,
    pub complete_margin_s: u64,
    pub cosign_per_signer_per_hour: u32,
    pub cosign_global_per_hour: u32,
    pub cosign_failures_per_hour: u32,
    pub gasless_min_usdc: u64,
    pub deadline_margin_s: u64,
    pub program_min_deadline_s: u64,
    pub program_min_burn: u64,
    pub max_fee_deci_bps: u16,
    pub max_forward_fee: u64,
    pub upgrade_delay_s: u64,
    pub enabled_gasless: bool,
    pub enabled_arrivals: bool,
    pub arrival_table: Option<SolanaTableConfig>,
    pub program_event_keypair_allowed: bool,
}

impl From<types::solana::SolanaConfig> for SolanaConfig {
    fn from(c: types::solana::SolanaConfig) -> Self {
        Self {
            program_id: base58::encode(&c.program_id),
            sol_rpc: c.sol_rpc,
            key_name: c.key_name,
            min_network_fee: SolanaFeesConfig {
                gasless_burn: c.min_network_fee.gasless_burn.into(),
                complete_new_account: c.min_network_fee.complete_new_account.into(),
                release_new_account: c.min_network_fee.release_new_account.into(),
                forward_relay: c.min_network_fee.forward_relay.into(),
                reclaim: c.min_network_fee.reclaim.into(),
            },
            max_cu_limit: c.max_cu_limit,
            max_cu_price: c.max_cu_price,
            max_priority_lamports: c.max_priority_lamports,
            loaded_data_limit: c.loaded_data_limit,
            rent_float_low: c.rent_float_low,
            rent_float_target: c.rent_float_target,
            max_complete_attempts: c.max_complete_attempts,
            max_release_attempts: c.max_release_attempts,
            complete_margin_s: c.complete_margin_s,
            cosign_per_signer_per_hour: c.cosign_per_signer_per_hour,
            cosign_global_per_hour: c.cosign_global_per_hour,
            cosign_failures_per_hour: c.cosign_failures_per_hour,
            gasless_min_usdc: c.gasless_min_usdc,
            deadline_margin_s: c.deadline_margin_s,
            program_min_deadline_s: c.program_min_deadline_s,
            program_min_burn: c.program_min_burn,
            max_fee_deci_bps: c.max_fee_deci_bps,
            max_forward_fee: c.max_forward_fee,
            upgrade_delay_s: c.upgrade_delay_s,
            enabled_gasless: c.enabled_gasless,
            enabled_arrivals: c.enabled_arrivals,
            arrival_table: c.arrival_table.map(|t| SolanaTableConfig {
                address: base58::encode(&t.address),
                entries: t.entries.iter().map(|e| base58::encode(e)).collect(),
            }),
            program_event_keypair_allowed: c.program_event_keypair_allowed,
        }
    }
}

impl TryFrom<SolanaConfig> for types::solana::SolanaConfig {
    type Error = String;

    fn try_from(c: SolanaConfig) -> Result<Self, String> {
        let amount = |field: &str, n: Nat| -> Result<TokenAmount, String> {
            TokenAmount::try_from(n).map_err(|_| format!("{field} is above u128::MAX"))
        };
        let table = match c.arrival_table {
            None => None,
            Some(t) => Some(SolanaTable {
                address: address("arrival_table.address", &t.address)?,
                entries: t
                    .entries
                    .iter()
                    .map(|e| address("arrival_table.entries", e))
                    .collect::<Result<_, _>>()?,
            }),
        };
        Ok(Self {
            program_id: address("program_id", &c.program_id)?,
            sol_rpc: c.sol_rpc,
            key_name: c.key_name,
            min_network_fee: SolanaFees {
                gasless_burn: amount("gasless_burn", c.min_network_fee.gasless_burn)?,
                complete_new_account: amount(
                    "complete_new_account",
                    c.min_network_fee.complete_new_account,
                )?,
                release_new_account: amount(
                    "release_new_account",
                    c.min_network_fee.release_new_account,
                )?,
                forward_relay: amount("forward_relay", c.min_network_fee.forward_relay)?,
                reclaim: amount("reclaim", c.min_network_fee.reclaim)?,
            },
            max_cu_limit: c.max_cu_limit,
            max_cu_price: c.max_cu_price,
            max_priority_lamports: c.max_priority_lamports,
            loaded_data_limit: c.loaded_data_limit,
            rent_float_low: c.rent_float_low,
            rent_float_target: c.rent_float_target,
            max_complete_attempts: c.max_complete_attempts,
            max_release_attempts: c.max_release_attempts,
            complete_margin_s: c.complete_margin_s,
            cosign_per_signer_per_hour: c.cosign_per_signer_per_hour,
            cosign_global_per_hour: c.cosign_global_per_hour,
            cosign_failures_per_hour: c.cosign_failures_per_hour,
            gasless_min_usdc: c.gasless_min_usdc,
            deadline_margin_s: c.deadline_margin_s,
            program_min_deadline_s: c.program_min_deadline_s,
            program_min_burn: c.program_min_burn,
            max_fee_deci_bps: c.max_fee_deci_bps,
            max_forward_fee: c.max_forward_fee,
            upgrade_delay_s: c.upgrade_delay_s,
            enabled_gasless: c.enabled_gasless,
            enabled_arrivals: c.enabled_arrivals,
            arrival_table: table,
            program_event_keypair_allowed: c.program_event_keypair_allowed,
        })
    }
}

/// The canister's two Solana keys.
#[derive(CandidType, Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct SolanaAddresses {
    pub payer: String,
    pub admin: String,
}

/// What `finish_solana` is asked to do.
#[derive(CandidType, Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SolanaFinishAction {
    Complete,
    Release,
}

/// One account of a route: the program sets the signer flag, so none is taken here.
#[derive(CandidType, Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct SolanaRouteAccount {
    pub address: String,
    pub writable: bool,
}

#[derive(CandidType, Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct SolanaRoute {
    pub data: Vec<u8>,
    pub accounts: Vec<SolanaRouteAccount>,
}

/// A blockhash and when it stops being valid: the watcher's, and it decides no money.
#[derive(CandidType, Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct SolanaLifetime {
    pub blockhash: String,
    pub last_valid_block_height: u64,
}

#[derive(CandidType, Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct FinishSolanaArgs {
    pub message: Vec<u8>,
    pub attestation: Vec<u8>,
    pub src_tx_hash: Vec<u8>,
    pub action: SolanaFinishAction,
    pub route: Option<SolanaRoute>,
    pub cu_limit: u32,
    pub create_account: bool,
    /// The output token is a Token-2022 token: its account derives under that program.
    pub token_2022: bool,
    pub lifetime: SolanaLifetime,
}

#[derive(CandidType, Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct ReleaseMisroutedArgs {
    pub message: Vec<u8>,
    pub attestation: Vec<u8>,
    pub order_ref: Vec<u8>,
    pub payer: String,
    pub lifetime: SolanaLifetime,
}

#[derive(CandidType, Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct SolanaReclaimItem {
    pub r#ref: Vec<u8>,
    pub attestation: Vec<u8>,
    pub destination_message: Vec<u8>,
}

#[derive(CandidType, Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct RelaySolanaForwardArgs {
    pub message: Vec<u8>,
    pub attestation: Vec<u8>,
    pub src_tx_hash: Vec<u8>,
    pub lifetime: SolanaLifetime,
}

#[derive(CandidType, Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct SweepSolanaFeesArgs {
    pub to: String,
    pub amount: u64,
    pub lifetime: SolanaLifetime,
}

/// The program's settings. Nothing here moves money: its keys and receiver are compiled in.
#[derive(CandidType, Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct SolanaProgramConfigArgs {
    pub paused: bool,
    pub event_keypair_allowed: bool,
    pub swap_domains: u64,
    pub ck_domains: u64,
    pub min_burn: u64,
    pub min_deadline_s: u32,
    pub max_deadline_s: u32,
    pub lifetime: SolanaLifetime,
}

#[derive(CandidType, Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct ProposeSolanaUpgradeArgs {
    pub buffer: String,
    pub build_hash: Vec<u8>,
    pub source_commit: String,
}

#[derive(CandidType, Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct SolanaUpgradeView {
    pub id: u64,
    pub buffer: String,
    pub build_hash: Vec<u8>,
    pub source_commit: String,
    pub proposed_at_s: u64,
    pub not_before_s: u64,
    pub status: String,
}

/// What `solana_budget` shows ops and the watcher.
#[derive(CandidType, Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct SolanaBudget {
    pub cosigns_this_hour: u32,
    pub failures_this_hour: u32,
}
