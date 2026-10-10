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

#[test]
fn the_network_fee_is_read_off_swapics_own_fee_log() {
    let swapic = EvmAddress::new([0x5a; 20]);
    let topic = format!("0x{}", hex::encode(types::router::swap_fees_topic()));
    let data = |platform: u64, network: u64| {
        format!(
            "0x{:064x}{:064x}{:064x}{:064x}",
            platform, network, 0_u64, 0_u64
        )
    };
    let receipt = json!({
        "logs": [
            { "address": format!("0x{}", hex::encode([0x5a; 20])), "topics": [topic, "0x11"],
              "data": data(0, 50_000) },
            // another contract's log of the same shape is not Swapic's
            { "address": format!("0x{}", hex::encode([0x77; 20])), "topics": [topic],
              "data": data(0, 9_999) },
        ]
    });
    assert_eq!(
        network_fee_in(&receipt, swapic),
        TokenAmount::from(50_000_u32)
    );
    assert_eq!(
        network_fee_in(&json!({ "logs": [] }), swapic),
        TokenAmount::ZERO
    );
}
