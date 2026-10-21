//! The wire of the canister's new roles: the gasless relay, ICP to EVM, and EVM to ICP.
//!
//! Router params and the gasless authorization cross as their ABI encoding (one tuple
//! each, what viem's `encodeAbiParameters` produces), so the canister decodes exactly the
//! bytes the router will be handed and no field is re-spelled on the way.

use crate::types::errors::GuardError;
use crate::types::events::Hash32;
use candid::{CandidType, Nat, Principal};
use serde::Deserialize;

/// What the flows read at runtime. Set by a controller; the canister's `Config` is the
/// chain layer's and stays as it was.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct FlowSettings {
    /// The chain the ck minter withdraws to and the continuation runs on: Ethereum.
    pub ethereum_chain_id: u64,
    /// USDC on that chain, which the withdrawal lands as.
    pub ethereum_usdc: Option<String>,
    pub ckusdc_ledger: Option<Principal>,
    /// The ckETH minter, which withdraws ckERC20 tokens too.
    pub ck_minter: Option<Principal>,
    /// The ckETH ledger, when the minter charges the withdrawal's gas in ckETH: the
    /// canister approves the minter for `cketh_fee_allowance` of it before a withdrawal.
    pub cketh_ledger: Option<Principal>,
    pub cketh_fee_allowance: Nat,
    /// The only ICPSwap pools a quote may name.
    pub icpswap_pools: Vec<Principal>,
    /// The furthest ahead a relayed quote's deadline may be.
    pub relay_window_s: u64,
    /// How long the continuation's params stay valid on chain.
    pub continuation_deadline_s: u64,
    /// The most gas a relay or a continuation may be given.
    pub max_gas_limit: u64,
    /// Whether the ICP flows run: `register_icp_quote`, `start_icp_swap`,
    /// `register_evm_quote` and their engine steps. They still speak router ABI v2, so
    /// `true` is refused (`IcpFlowsNotOnV3`) until they move to ABI v3 with the pool (P11).
    pub icp_enabled: Option<bool>,
    /// The ck minter's ERC-20 deposit helper on Ethereum (`depositErc20` with subaccounts):
    /// a Base to ICP rebalance move filled by Eco is deposited through it (P11c).
    pub ck_helper: Option<String>,
    /// Eco's prover, the one a rebalance proposal's intent may name: an intent proven by
    /// any other pays its reward to whoever that prover says (P11c).
    pub eco_prover: Option<String>,
}

/// Why a flow door refused. Nothing moved.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum FlowCallError {
    Guard(GuardError),
    /// The request is not one the canister acts on: the field and the reason.
    Invalid(String),
    /// A rule refused it: the reason.
    Refused(String),
    /// A chain or a canister did not answer usably; the caller may try again.
    Unavailable(String),
}

/// The arrival swap a CCTP Swap completes with (ABI v3 `Recipe`): `target` is called with
/// `data` paying the Runner, rescaled to what arrived by `scaler` when one is named.
/// Addresses are EVM addresses.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Recipe {
    pub target: String,
    pub data: Vec<u8>,
    pub scaler: Option<String>,
}

/// A flow as the fold holds it.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FlowView {
    pub kind: String,
    pub status: String,
    pub held: Nat,
    pub held_ledger: Option<Principal>,
    pub eth_held: Nat,
    pub withdrawal_id: Option<u64>,
    pub landed_tx: Option<Hash32>,
    pub chain_id: Option<u64>,
    pub tx_hash: Option<Hash32>,
    pub tx_open: bool,
    pub tx_block: Option<u64>,
    pub continuation_action: Option<u8>,
    pub failed_continuations: u32,
    pub delivered: Option<Nat>,
    pub reason: Option<String>,
}

/// A relay's status.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum RelayStatus {
    /// Signed and on its way; `tx_hash` is the first broadcast's.
    Signed { chain_id: u64, tx_hash: Hash32 },
    Confirmed {
        chain_id: u64,
        tx_hash: Hash32,
        block: u64,
    },
    Failed {
        chain_id: u64,
        tx_hash: Hash32,
        block: u64,
    },
}
