use super::*;
use crate::storage::events::append_event_at;
use crate::storage::halt::is_halted;
use crate::storage::on_fresh_memory;
use serde_json::json;
use types::BlockNumber;

fn checked(observed: u64, expected: u64, shortfall: bool) -> EventType {
    EventType::Books {
        chain_id: ChainId::BASE,
        step: BookStep::Checked {
            check: BalanceCheck {
                block: BlockNumber::new(10),
                observed: TokenAmount::from(observed),
                expected: TokenAmount::from(expected),
            },
            shortfall,
        },
    }
}

/// A shortfall pauses the pool and nothing else: the canister is not halted, and a check
/// that finds a donation pauses nothing at all.
#[test]
fn shortfall_halts_pool_only() {
    on_fresh_memory(|| {
        crate::storage::init();
        let at = Timestamp::from_nanos(1);
        append_event_at(checked(150, 100, false), at).unwrap();
        assert_eq!(read_state(|state| state.meta().pool_paused_by), None);
        append_event_at(checked(50, 100, true), at).unwrap();
        assert_eq!(
            read_state(|state| state.meta().pool_paused_by),
            Some(ChainId::BASE)
        );
        assert!(!is_halted(), "the canister keeps running");
        let books = read_state(|state| state.book(&ChainId::BASE));
        assert_eq!(books.last_check.unwrap().surplus(), -50);
        assert_eq!(books.fee_income, TokenAmount::ZERO);
    });
}

/// The fee step is a USDC transfer from the Runner to the canister: only those transfers
/// in a receipt are the fee, whatever else moved in it.
#[test]
fn the_network_fee_is_the_fee_steps_transfer() {
    let path = FeePath {
        usdc: EvmAddress::new([0x83; 20]),
        runner: EvmAddress::new([0xd9; 20]),
        canister: EvmAddress::new([0xca; 20]),
    };
    let topic = format!("0x{}", hex::encode(types::router::transfer_topic()));
    let word = |address: EvmAddress| format!("0x{}", hex::encode(address.to_word()));
    let transfer = |token: [u8; 20], from: EvmAddress, to: EvmAddress, amount: u64| {
        json!({ "address": format!("0x{}", hex::encode(token)),
                "topics": [topic, word(from), word(to)],
                "data": format!("0x{amount:064x}") })
    };
    let user = EvmAddress::new([0xe0; 20]);
    let receipt = json!({
        "logs": [
            transfer([0x83; 20], user, path.runner, 100_000_000),
            transfer([0x83; 20], path.runner, path.canister, 166_275),
            // the burn's USDC leaves the Runner for someone else
            transfer([0x83; 20], path.runner, EvmAddress::new([0x28; 20]), 99_833_725),
            // another token's transfer of the same shape is no fee
            transfer([0x77; 20], path.runner, path.canister, 9_999),
        ]
    });
    assert_eq!(
        network_fee_in(&receipt, path),
        TokenAmount::from(166_275_u32)
    );
    assert_eq!(
        network_fee_in(&json!({ "logs": [] }), path),
        TokenAmount::ZERO
    );
}
