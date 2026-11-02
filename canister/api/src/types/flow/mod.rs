//! The wire of the canister's new roles: the gasless relay, ICP to EVM, and EVM to ICP.
//!
//! Router params and the gasless authorization cross as their ABI encoding (one tuple
//! each, what viem's `encodeAbiParameters` produces), so the canister decodes exactly the
//! bytes the router will be handed and no field is re-spelled on the way.

use crate::types::errors::GuardError;
use candid::{CandidType, Nat, Principal};
use serde::Deserialize;

/// What the flows and the rebalancing read at runtime, beside `Config`. Set by a controller.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct FlowSettings {
    /// The ckETH minter, which withdraws ckERC20 tokens too: an ICP to Base move's ckUSDC
    /// leaves ICP through it.
    pub ck_minter: Option<Principal>,
    /// The ckETH ledger, when the minter charges the withdrawal's gas in ckETH: the
    /// canister approves the minter for `cketh_fee_allowance` of it before a withdrawal.
    pub cketh_ledger: Option<Principal>,
    pub cketh_fee_allowance: Nat,
    /// The furthest ahead a relayed order's deadline may be.
    pub relay_window_s: u64,
    /// The ck minter's ERC-20 deposit helper on Ethereum (`depositErc20` with subaccounts):
    /// a Base to ICP rebalance move filled by Eco is deposited through it.
    pub ck_helper: Option<String>,
    /// Eco's prover, the one a rebalance proposal's intent may name: an intent proven by
    /// any other pays its reward to whoever that prover says.
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
