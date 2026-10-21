//! The flows' settings: the routers, the ck canisters, the ICPSwap pools a quote may name,
//! and the windows. Stored as their candid bytes in one stable cell, set by a controller,
//! and read in their parsed form.

#[cfg(test)]
mod tests;

use crate::storage::memory::{flow_settings_memory, Memory};
use candid::{Decode, Encode, Nat, Principal};
use ic_stable_structures::StableCell;
use settlement_api::types::flow::FlowSettings as Wire;
use std::cell::RefCell;
use std::time::Duration;
use types::{ChainId, EvmAddress, TokenAmount};

thread_local! {
    static SETTINGS: RefCell<StableCell<Vec<u8>, Memory>> = RefCell::new(
        StableCell::init(flow_settings_memory(), Vec::new()).expect("flow settings cell init"),
    );
}

/// The settings, parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlowSettings {
    pub ethereum: ChainId,
    pub ethereum_usdc: Option<EvmAddress>,
    pub ckusdc_ledger: Option<Principal>,
    pub ck_minter: Option<Principal>,
    pub cketh_ledger: Option<Principal>,
    pub cketh_fee_allowance: TokenAmount,
    pub icpswap_pools: Vec<Principal>,
    pub relay_window: Duration,
    pub continuation_deadline: Duration,
    pub max_gas_limit: u64,
    /// Whether the ICP flows run. Off unless a controller turns them on.
    pub icp_enabled: bool,
    /// The ck minter's ERC-20 deposit helper on Ethereum (P11c).
    pub ck_helper: Option<EvmAddress>,
    /// Eco's prover a rebalance proposal's intent may name (P11c).
    pub eco_prover: Option<EvmAddress>,
}

fn parse(wire: &Wire) -> Result<FlowSettings, String> {
    // the ICP flows sign router ABI v2's `swapFor`, which a design C Swapic does not
    // have: on, they would sign it to a v3 Swapic. Refused here, so neither a write nor a
    // stored setting can turn them on before they move to ABI v3 (P11)
    if wire.icp_enabled == Some(true) {
        return Err(
            "IcpFlowsNotOnV3: the ICP flows stay off until they move to ABI v3".to_string(),
        );
    }
    let ethereum_usdc = wire
        .ethereum_usdc
        .as_ref()
        .map(|text| text.parse::<EvmAddress>())
        .transpose()
        .map_err(|e| format!("ethereum_usdc is not an address: {e:?}"))?;
    let address = |field: &str, text: &Option<String>| {
        text.as_ref()
            .map(|text| text.parse::<EvmAddress>())
            .transpose()
            .map_err(|e| format!("{field} is not an address: {e:?}"))
    };
    let ck_helper = address("ck_helper", &wire.ck_helper)?;
    let eco_prover = address("eco_prover", &wire.eco_prover)?;
    let cketh_fee_allowance = TokenAmount::try_from(wire.cketh_fee_allowance.clone())
        .map_err(|_| "cketh_fee_allowance is too large".to_string())?;
    Ok(FlowSettings {
        ethereum: ChainId::new(if wire.ethereum_chain_id == 0 {
            1
        } else {
            wire.ethereum_chain_id
        }),
        ethereum_usdc,
        ckusdc_ledger: wire.ckusdc_ledger,
        ck_minter: wire.ck_minter,
        cketh_ledger: wire.cketh_ledger,
        cketh_fee_allowance,
        icpswap_pools: wire.icpswap_pools.clone(),
        relay_window: Duration::from_secs(if wire.relay_window_s == 0 {
            3_600
        } else {
            wire.relay_window_s
        }),
        continuation_deadline: Duration::from_secs(if wire.continuation_deadline_s == 0 {
            1_800
        } else {
            wire.continuation_deadline_s
        }),
        max_gas_limit: if wire.max_gas_limit == 0 {
            2_000_000
        } else {
            wire.max_gas_limit
        },
        icp_enabled: false,
        ck_helper,
        eco_prover,
    })
}

pub fn init() {
    SETTINGS.with(|_| ());
}

/// The wire settings as a controller set them, or the defaults.
pub fn get_wire() -> Wire {
    let bytes = SETTINGS.with(|cell| cell.borrow().get().clone());
    if bytes.is_empty() {
        return Wire::default();
    }
    Decode!(&bytes, Wire).unwrap_or_default()
}

/// The parsed settings.
pub fn get() -> FlowSettings {
    parse(&get_wire())
        .unwrap_or_else(|_| parse(&Wire::default()).expect("BUG: the default flow settings parse"))
}

/// Validates and stores `wire`. The caller does the authorization.
pub fn set(wire: Wire) -> Result<(), String> {
    parse(&wire)?;
    let bytes = Encode!(&wire).map_err(|e| e.to_string())?;
    SETTINGS.with(|cell| {
        cell.borrow_mut()
            .set(bytes)
            .expect("flow settings cell write")
    });
    Ok(())
}

/// What a Nat is as a TokenAmount, when it fits a canonical field.
pub fn amount_of(value: &Nat) -> Option<TokenAmount> {
    TokenAmount::from_canonical_nat(value.clone())
}
