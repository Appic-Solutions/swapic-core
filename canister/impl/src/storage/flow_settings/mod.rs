//! The flows' settings: the routers, the ck canisters, the ICPSwap pools a quote may name,
//! and the windows. Stored as their candid bytes in one stable cell, set by a controller,
//! and read in their parsed form.

use crate::storage::memory::{flow_settings_memory, Memory};
use candid::{Decode, Encode, Nat, Principal};
use ic_stable_structures::StableCell;
use settlement_api::types::flow::FlowSettings as Wire;
use std::cell::RefCell;
use std::collections::BTreeMap;
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
    pub routers: BTreeMap<ChainId, EvmAddress>,
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
}

impl FlowSettings {
    /// The router on `chain_id`, if one is set.
    pub fn router(&self, chain_id: ChainId) -> Option<EvmAddress> {
        self.routers.get(&chain_id).copied()
    }
}

fn parse(wire: &Wire) -> Result<FlowSettings, String> {
    let mut routers = BTreeMap::new();
    for (chain, address) in &wire.routers {
        let address: EvmAddress = address
            .parse()
            .map_err(|e| format!("routers[{chain}] is not an address: {e:?}"))?;
        routers.insert(ChainId::new(*chain), address);
    }
    let ethereum_usdc = wire
        .ethereum_usdc
        .as_ref()
        .map(|text| text.parse::<EvmAddress>())
        .transpose()
        .map_err(|e| format!("ethereum_usdc is not an address: {e:?}"))?;
    let cketh_fee_allowance = TokenAmount::try_from(wire.cketh_fee_allowance.clone())
        .map_err(|_| "cketh_fee_allowance is too large".to_string())?;
    Ok(FlowSettings {
        routers,
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
