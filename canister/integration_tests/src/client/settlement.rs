//! Typed callers for the settlement canister, one per endpoint the suite calls. Each takes
//! `(pic, canister, sender, ..)` and answers with the api crate's `Response` for that
//! endpoint, so an api type that drifts from the canister fails to decode here.

use crate::client::pocket::{query, update};
use candid::{encode_args, encode_one, Principal};
use pocket_ic::PocketIc;
use settlement_api::queries::{
    event_count, events_page, evm_address, get_chain_data, get_config, get_config_full,
    get_pending, get_swap, halted, test_outbox_armed, verify_chain, verify_replay,
};
use settlement_api::types::errors::{GuardError, TestAppendError};
use settlement_api::types::events::EventType;
use settlement_api::updates::{
    audit_replay_step, clear_pending_quotes, derive_evm_address, push_chain_data, register_quote,
    set_config, set_halted, set_roles, set_sanctioned, test_rpc_batch as test_rpc_batch_api,
    test_send, test_sign,
};

pub fn event_count(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
) -> event_count::Response {
    query(
        pic,
        canister,
        sender,
        "event_count",
        encode_one(()).unwrap(),
    )
}

/// Positional, as the endpoint takes them.
pub fn events_page(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    start: u64,
    len: u64,
) -> events_page::Response {
    query(
        pic,
        canister,
        sender,
        "events_page",
        encode_args((start, len)).unwrap(),
    )
}

pub fn verify_chain(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
) -> verify_chain::Response {
    query(
        pic,
        canister,
        sender,
        "verify_chain",
        encode_one(()).unwrap(),
    )
}

pub fn verify_replay(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
) -> verify_replay::Response {
    query(
        pic,
        canister,
        sender,
        "verify_replay",
        encode_one(()).unwrap(),
    )
}

pub fn get_config(pic: &PocketIc, canister: Principal, sender: Principal) -> get_config::Response {
    query(pic, canister, sender, "get_config", encode_one(()).unwrap())
}

pub fn get_config_full(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
) -> get_config_full::Response {
    query(
        pic,
        canister,
        sender,
        "get_config_full",
        encode_one(()).unwrap(),
    )
}

pub fn set_config(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    new: &set_config::Args,
) -> set_config::Response {
    update(
        pic,
        canister,
        sender,
        "set_config",
        encode_one(new).unwrap(),
    )
}

/// Positional, as the endpoint takes them.
pub fn set_roles(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    quoter: Principal,
    watcher: Principal,
) -> set_roles::Response {
    update(
        pic,
        canister,
        sender,
        "set_roles",
        encode_args((quoter, watcher)).unwrap(),
    )
}

pub fn register_quote(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    quote: &register_quote::Args,
) -> register_quote::Response {
    update(
        pic,
        canister,
        sender,
        "register_quote",
        encode_one(quote).unwrap(),
    )
}

pub fn clear_pending_quotes(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
) -> clear_pending_quotes::Response {
    update(
        pic,
        canister,
        sender,
        "clear_pending_quotes",
        encode_one(()).unwrap(),
    )
}

pub fn get_pending(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    hash: get_pending::Args,
) -> get_pending::Response {
    query(
        pic,
        canister,
        sender,
        "get_pending",
        encode_one(hash).unwrap(),
    )
}

pub fn get_swap(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    hash: get_swap::Args,
) -> get_swap::Response {
    query(pic, canister, sender, "get_swap", encode_one(hash).unwrap())
}

/// Positional, as the endpoint takes them.
pub fn push_chain_data(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    chain_id: u64,
    data: &settlement_api::types::chain_data::ChainData,
) -> push_chain_data::Response {
    update(
        pic,
        canister,
        sender,
        "push_chain_data",
        encode_args((chain_id, data)).unwrap(),
    )
}

pub fn get_chain_data(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    chain_id: get_chain_data::Args,
) -> get_chain_data::Response {
    query(
        pic,
        canister,
        sender,
        "get_chain_data",
        encode_one(chain_id).unwrap(),
    )
}

pub fn halted(pic: &PocketIc, canister: Principal, sender: Principal) -> halted::Response {
    query(pic, canister, sender, "halted", encode_one(()).unwrap())
}

/// The test door onto the outbox timer: whether a pass is on its way.
pub fn test_outbox_armed(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
) -> test_outbox_armed::Response {
    query(
        pic,
        canister,
        sender,
        "test_outbox_armed",
        encode_one(()).unwrap(),
    )
}

pub fn set_halted(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    halted: set_halted::Args,
) -> set_halted::Response {
    update(
        pic,
        canister,
        sender,
        "set_halted",
        encode_one(halted).unwrap(),
    )
}

pub fn audit_replay_step(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    max_events: audit_replay_step::Args,
) -> audit_replay_step::Response {
    update(
        pic,
        canister,
        sender,
        "audit_replay_step",
        encode_one(max_events).unwrap(),
    )
}

/// The canister's own EVM address, or nothing before it has been derived.
pub fn evm_address(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
) -> evm_address::Response {
    query(
        pic,
        canister,
        sender,
        "evm_address",
        encode_one(()).unwrap(),
    )
}

/// Derives the canister's EVM address, or answers the one already derived.
pub fn derive_evm_address(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
) -> derive_evm_address::Response {
    update(
        pic,
        canister,
        sender,
        "derive_evm_address",
        encode_one(()).unwrap(),
    )
}

/// The test door onto one threshold signature.
pub fn test_sign(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    hash: test_sign::Args,
) -> test_sign::Response {
    update(
        pic,
        canister,
        sender,
        "test_sign",
        encode_one(hash).unwrap(),
    )
}

/// The test door onto one transaction: create, sign and queue a payout for `quote_hash`.
pub fn test_send(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    quote_hash: settlement_api::types::events::Hash32,
    chain_id: u64,
    to: &str,
    gas_limit: u64,
) -> test_send::Response {
    update(
        pic,
        canister,
        sender,
        "test_send",
        encode_args((quote_hash, chain_id, to.to_string(), gas_limit)).unwrap(),
    )
}

/// The test door onto one unreplicated JSON-RPC batch; every call goes out with empty
/// params.
pub fn test_rpc_batch(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    chain_id: u64,
    methods: &[String],
    max_bytes: u64,
) -> test_rpc_batch_api::Response {
    update(
        pic,
        canister,
        sender,
        "test_rpc_batch",
        encode_args((chain_id, methods, max_bytes)).unwrap(),
    )
}

/// The test door onto `append_event`; the inner Result is the canister's own answer.
pub fn append(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    event: &EventType,
) -> Result<u64, TestAppendError> {
    update(
        pic,
        canister,
        sender,
        "test_append",
        encode_one(event).unwrap(),
    )
}

/// The test-only door that flips one bit of the fold's chain head and leaves the log alone.
/// Calling it twice puts the head back.
pub fn test_skew_state(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
) -> Result<(), GuardError> {
    update(
        pic,
        canister,
        sender,
        "test_skew_state",
        encode_one(()).unwrap(),
    )
}

/// Positional, as the endpoint takes them: adds `add` to the sanctions set, then removes
/// `remove`, and answers the count held after.
pub fn set_sanctioned(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    add: &[&str],
    remove: &[&str],
) -> set_sanctioned::Response {
    let add: Vec<String> = add.iter().map(|text| text.to_string()).collect();
    let remove: Vec<String> = remove.iter().map(|text| text.to_string()).collect();
    update(
        pic,
        canister,
        sender,
        "set_sanctioned",
        encode_args((add, remove)).unwrap(),
    )
}
