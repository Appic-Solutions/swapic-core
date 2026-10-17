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

const MINUTE: u64 = 60_000_000_000;

fn at_minute(minute: u64) -> Timestamp {
    Timestamp::from_nanos(minute * MINUTE)
}

/// The spec defaults (ten-minute checks) with Base as the pool chain.
fn pool_config() -> types::Config {
    types::Config {
        pool_chain: Some(ChainId::BASE),
        ..types::Config::default()
    }
}

fn status(halted: bool, now: Timestamp) -> Result<(), PoolClosed> {
    read_state(|state| pool_status(state, &pool_config(), halted, now))
}

fn pool_step(paused: bool) -> EventType {
    EventType::Books {
        chain_id: ChainId::BASE,
        step: BookStep::PoolPaused { paused },
    }
}

/// Finding 2: the pause the books write is read. A shortfall closes the pool until a
/// controller reopens it, a controller's pause closes it, and a halt closes it; nothing
/// without a pool chain is open.
#[test]
fn pool_pause_is_enforced() {
    on_fresh_memory(|| {
        crate::storage::init();
        append_event_at(checked(150, 100, false), at_minute(1)).unwrap();
        assert_eq!(status(false, at_minute(2)), Ok(()));
        assert_eq!(status(true, at_minute(2)), Err(PoolClosed::Halted));
        assert_eq!(
            read_state(|state| pool_status(state, &types::Config::default(), false, at_minute(2))),
            Err(PoolClosed::NoPoolChain)
        );

        append_event_at(checked(50, 100, true), at_minute(3)).unwrap();
        assert_eq!(
            status(false, at_minute(4)),
            Err(PoolClosed::Shortfall(ChainId::BASE))
        );
        // a clean check does not lift a shortfall: a controller looks first
        append_event_at(checked(150, 100, false), at_minute(5)).unwrap();
        assert_eq!(
            status(false, at_minute(6)),
            Err(PoolClosed::Shortfall(ChainId::BASE))
        );
        append_event_at(pool_step(false), at_minute(7)).unwrap();
        assert_eq!(status(false, at_minute(8)), Ok(()));

        append_event_at(pool_step(true), at_minute(9)).unwrap();
        assert_eq!(status(false, at_minute(10)), Err(PoolClosed::Paused));
        append_event_at(pool_step(false), at_minute(11)).unwrap();
        assert_eq!(status(false, at_minute(12)), Ok(()));
    });
}

/// Finding 2: books nobody has read lately close the pool, and so do books never read.
#[test]
fn stale_books_close_the_pool() {
    on_fresh_memory(|| {
        crate::storage::init();
        assert_eq!(
            status(false, at_minute(1)),
            Err(PoolClosed::NeverChecked(ChainId::BASE))
        );
        append_event_at(checked(150, 100, false), at_minute(10)).unwrap();
        // three ten-minute intervals is the most the books may go unread
        assert_eq!(status(false, at_minute(40)), Ok(()));
        assert!(matches!(
            status(false, at_minute(41)),
            Err(PoolClosed::Stale { .. })
        ));
        // a check on another chain is not the pool chain's
        append_event_at(
            EventType::Books {
                chain_id: ChainId::ARBITRUM,
                step: BookStep::Checked {
                    check: BalanceCheck {
                        block: BlockNumber::new(10),
                        observed: TokenAmount::from(1_u32),
                        expected: TokenAmount::ZERO,
                    },
                    shortfall: false,
                },
            },
            at_minute(41),
        )
        .unwrap();
        assert!(matches!(
            status(false, at_minute(42)),
            Err(PoolClosed::Stale { .. })
        ));
    });
}

/// Finding 2: a failed read is recorded, and it refreshes nothing the guard reads, so a
/// chain whose balance cannot be read goes stale and closes the pool.
#[test]
fn failed_read_does_not_refresh_the_check() {
    on_fresh_memory(|| {
        crate::storage::init();
        append_event_at(checked(150, 100, false), at_minute(10)).unwrap();
        let failed = EventType::Books {
            chain_id: ChainId::BASE,
            step: BookStep::CheckFailed {
                reason: "the read failed: unreachable".into(),
            },
        };
        let index = append_event_at(failed.clone(), at_minute(35)).unwrap();
        assert_eq!(
            crate::storage::events::event_at(index.get()).map(|event| event.payload),
            Some(failed),
            "the failure is a line in the log"
        );
        let books = read_state(|state| state.book(&ChainId::BASE));
        assert_eq!(books.checked_at, Some(at_minute(10)));
        assert_eq!(books.failed_at, Some(at_minute(35)));
        assert!(matches!(
            status(false, at_minute(41)),
            Err(PoolClosed::Stale { .. })
        ));
    });
}
