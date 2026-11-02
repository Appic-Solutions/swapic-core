//! The flows' settings: the routers, the ck canisters, the ICPSwap pools a quote may name,
//! and the windows. Stored as their candid bytes in one stable cell, set by a controller,
//! and read in their parsed form. Stored bytes that no longer decode or parse are never read
//! as the defaults (L-08): `post_upgrade` traps on them, keeping the wasm that wrote them,
//! and a read traps. Their layout is pinned by `impl/golden/candid_cells_v1.txt` (C-11).

#[cfg(test)]
mod tests;

use crate::storage::memory::{flow_settings_memory, Memory};
use candid::{Decode, Encode, Nat, Principal};
use ic_stable_structures::StableCell;
use settlement_api::types::flow::FlowSettings as Wire;
use std::cell::RefCell;
use std::time::Duration;
use types::{EvmAddress, TokenAmount};

thread_local! {
    static SETTINGS: RefCell<StableCell<Vec<u8>, Memory>> = RefCell::new(
        StableCell::init(flow_settings_memory(), Vec::new()).expect("flow settings cell init"),
    );
}

/// The settings, parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlowSettings {
    pub ck_minter: Option<Principal>,
    pub cketh_ledger: Option<Principal>,
    pub cketh_fee_allowance: TokenAmount,
    pub relay_window: Duration,
    /// The ck minter's ERC-20 deposit helper on Ethereum.
    pub ck_helper: Option<EvmAddress>,
    /// Eco's prover a rebalance proposal's intent may name.
    pub eco_prover: Option<EvmAddress>,
}

#[cfg(test)]
thread_local! {
    // how many times the settings were parsed, for the test that a read parses once
    pub(crate) static PARSES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

fn parse(wire: &Wire) -> Result<FlowSettings, String> {
    #[cfg(test)]
    PARSES.with(|parses| parses.set(parses.get() + 1));
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
        ck_minter: wire.ck_minter,
        cketh_ledger: wire.cketh_ledger,
        cketh_fee_allowance,
        relay_window: Duration::from_secs(if wire.relay_window_s == 0 {
            3_600
        } else {
            wire.relay_window_s
        }),
        ck_helper,
        eco_prover,
    })
}

pub fn init() {
    SETTINGS.with(|_| ());
}

/// [`parse`], for the golden test of the stored bytes.
#[cfg(test)]
pub(crate) fn parse_for_tests(wire: &Wire) -> Result<FlowSettings, String> {
    parse(wire)
}

/// The stored settings, decoded and parsed once: None for a cell never written (the
/// defaults), and the refusal for bytes that no longer decode or parse.
fn stored() -> Result<Option<(Wire, FlowSettings)>, String> {
    let bytes = SETTINGS.with(|cell| cell.borrow().get().clone());
    if bytes.is_empty() {
        return Ok(None);
    }
    let wire = Decode!(&bytes, Wire)
        .map_err(|e| format!("the stored flow settings do not decode: {e}"))?;
    let parsed = parse(&wire).map_err(|e| format!("the stored flow settings do not parse: {e}"))?;
    Ok(Some((wire, parsed)))
}

fn stored_checked() -> Option<(Wire, FlowSettings)> {
    stored().expect("BUG: the stored flow settings are checked at every upgrade")
}

/// Whether the stored settings decode and parse under this build: what an upgrade checks
/// before it takes this wasm.
pub fn check() -> Result<(), String> {
    stored().map(drop)
}

/// The wire settings as a controller set them, or the defaults when none were.
pub fn get_wire() -> Wire {
    stored_checked().map(|(wire, _)| wire).unwrap_or_default()
}

/// The parsed settings, or the defaults' when none were set.
pub fn get() -> FlowSettings {
    match stored_checked() {
        Some((_, parsed)) => parsed,
        None => parse(&Wire::default()).expect("BUG: the default flow settings parse"),
    }
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
