use super::*;
use crate::storage::events::append_event_at;
use crate::storage::halt::is_halted;
use crate::storage::on_fresh_memory;
use serde_json::json;
use types::books::UnseenFees;
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
        eco_portal: None,
    };
    let topic = format!("0x{}", hex::encode(types::swapic::topics::transfer()));
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
    read_state(|state| pool_status(state, &state.pool(), &pool_config(), halted, now))
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
            read_state(|state| pool_status(
                state,
                &state.pool(),
                &types::Config::default(),
                false,
                at_minute(2)
            )),
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
                failure: ReadFailure::Unreachable,
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

/// A failed read leaves no trace of its error's text: an outcall's refusal or a provider's
/// answer can carry the provider's url and its api key, and the log, the books query and
/// the events page are world-readable. Only the failure's kind is recorded.
#[test]
fn a_failed_read_leaves_no_secret_in_the_log() {
    use crate::rpc::{RpcError, Summary};
    const SECRET: &str = "https://base-mainnet.g.alchemy.com/v2/sk-live-0123456789";
    on_fresh_memory(|| {
        crate::storage::init();
        let errors = [
            RpcError::Unreachable {
                reason: Summary::of(&format!("error trying to connect to {SECRET}: timeout")),
            },
            RpcError::Http {
                status: 401,
                body: Summary::of(&format!("{{\"error\":\"bad key for {SECRET}\"}}")),
            },
            RpcError::Rpc {
                method: "eth_call".into(),
                message: format!("upstream {SECRET} failed"),
            },
        ];
        for error in &errors {
            assert!(
                error.to_string().contains(SECRET),
                "the error itself carries it"
            );
            // what `record_failure` appends, at a fixed instant
            append_event_at(
                EventType::Books {
                    chain_id: ChainId::BASE,
                    step: BookStep::CheckFailed {
                        failure: error.kind(),
                    },
                },
                at_minute(1),
            )
            .unwrap();
        }
        let count = crate::storage::events::event_count();
        let page = crate::storage::events::events_page(0, count);
        assert_eq!(page.len() as u64, count);
        let failures: Vec<_> = page
            .iter()
            .filter_map(|event| match &event.payload {
                EventType::Books {
                    step: BookStep::CheckFailed { failure },
                    ..
                } => Some(*failure),
                _ => None,
            })
            .collect();
        assert_eq!(
            failures,
            vec![
                ReadFailure::Unreachable,
                ReadFailure::Http { status: 401 },
                ReadFailure::Provider
            ]
        );
        for event in page {
            let wire = settlement_api::types::events::Event::from(event.clone());
            let rendered = format!("{event:?} {wire:?}");
            let bytes = minicbor::to_vec(&event.payload).unwrap();
            for secret in [SECRET, "sk-live", "alchemy"] {
                assert!(!rendered.contains(secret), "{rendered}");
                assert!(!bytes
                    .windows(secret.len())
                    .any(|window| window == secret.as_bytes()));
            }
        }
    });
}

fn fee_earned(chain_id: ChainId, amount: TokenAmount, block: Option<u64>) -> EventType {
    EventType::Books {
        chain_id,
        step: BookStep::FeeEarned {
            tx_hash: TxHash::new([7; 32]),
            amount,
            block: block.map(BlockNumber::new),
        },
    }
}

/// Review fix: a fee is booked off a receipt at the swap depth, 1 block on Ethereum, while
/// the balance check reads the crediting depth behind the head, 12 there. The check leaves
/// out of what it expects every fee booked from a block above the one it read: a fee the
/// read cannot see yet is no shortfall, and it counts once the read reaches its block.
#[test]
fn a_fee_from_a_block_the_check_has_not_read_is_not_expected_yet() {
    on_fresh_memory(|| {
        crate::storage::init();
        let at = Timestamp::from_nanos(1);
        let chain = ChainId::ETHEREUM;
        let fee = TokenAmount::from(500_000_u64);
        append_event_at(fee_earned(chain, fee, Some(100)), at).unwrap();
        let check = |block: u64, observed: TokenAmount| {
            read_state(|state| {
                expected_at(
                    state,
                    chain,
                    None,
                    BlockNumber::new(block),
                    observed,
                    TokenAmount::ZERO,
                )
            })
        };
        // read at 99, before the fee's block: not expected, so no shortfall
        assert_eq!(check(99, TokenAmount::ZERO), (TokenAmount::ZERO, false));
        // read at its block or above: expected
        assert_eq!(check(100, TokenAmount::ZERO), (fee, true));
        assert_eq!(check(100, fee), (fee, false));
        assert_eq!(check(112, fee), (fee, false));

        // a line from before the block was recorded is expected at every block, as before
        let older = TokenAmount::from(1_000_u64);
        append_event_at(fee_earned(chain, older, None), at).unwrap();
        let both = fee.checked_add(older).unwrap();
        assert_eq!(check(99, older), (older, false));
        assert_eq!(check(100, both), (both, false));

        // a check that read at the fee's block has seen it: it is no longer held apart
        append_event_at(
            EventType::Books {
                chain_id: chain,
                step: BookStep::Checked {
                    check: BalanceCheck {
                        block: BlockNumber::new(100),
                        observed: both,
                        expected: both,
                    },
                    shortfall: false,
                },
            },
            at,
        )
        .unwrap();
        assert_eq!(
            read_state(|state| state.book(&chain)).unseen_fees,
            UnseenFees::default()
        );
    });
}

/// P11a: on the pool chain the check expects the pool's Base pocket too, and leaves out a
/// pool arrival from a block above the one it read: an arrival at block N is not expected
/// by a check at N - 1, and is by a check at N.
#[test]
fn a_pool_arrival_is_expected_from_its_block_on() {
    use crate::storage::pool::tests::{
        at as pool_at, credit, funded_pool, register, swap_terms, usdc,
    };
    use types::pool::PoolStep;
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let r#ref = register(swap_terms(1, usdc(100), usdc(1), pool_at(1_000)));
        crate::storage::pool::record_at(
            PoolStep::Arrived {
                r#ref,
                credit: credit(1),
                amount: usdc(100),
                block: BlockNumber::new(500),
            },
            pool_at(3),
        )
        .unwrap();
        let check = |chain: ChainId, block: u64, observed: TokenAmount| {
            read_state(|state| {
                expected_at(
                    state,
                    chain,
                    Some(ChainId::BASE),
                    BlockNumber::new(block),
                    observed,
                    TokenAmount::ZERO,
                )
            })
        };
        assert_eq!(
            check(ChainId::BASE, 499, TokenAmount::ZERO),
            (TokenAmount::ZERO, false)
        );
        assert_eq!(
            check(ChainId::BASE, 500, TokenAmount::ZERO),
            (usdc(100), true)
        );
        assert_eq!(check(ChainId::BASE, 500, usdc(100)), (usdc(100), false));
        // another chain's check expects none of the pool's
        assert_eq!(
            check(ChainId::ARBITRUM, 500, TokenAmount::ZERO),
            (TokenAmount::ZERO, false)
        );
    });
}

/// Review check (accounting): a gasless same-chain pool deposit's receipt pays the canister
/// its fee step and the pool its payout, both Runner transfers. The fee is booked once (the
/// payout is left out of it), the arrival is the pool's once, and the pool chain's check
/// expects each from its block on: nothing twice, nothing left out.
#[test]
fn a_fee_step_and_a_pool_arrival_in_one_receipt_are_each_expected_once() {
    use crate::storage::pool::tests::{
        at as pool_at, credit, funded_pool, register, swap_terms, usdc,
    };
    use types::pool::PoolStep;
    on_fresh_memory(|| {
        crate::storage::init();
        let path = FeePath {
            usdc: EvmAddress::new([0x83; 20]),
            runner: EvmAddress::new([0x7e; 20]),
            canister: EvmAddress::new([0x36; 20]),
            eco_portal: None,
        };
        let topic = format!("0x{}", hex::encode(types::swapic::topics::transfer()));
        let word = |address: EvmAddress| format!("0x{}", hex::encode(address.to_word()));
        let transfer = |amount: u64| {
            json!({ "address": format!("0x{}", hex::encode(path.usdc.as_bytes())),
                    "topics": [topic, word(path.runner), word(path.canister)],
                    "data": format!("0x{amount:064x}") })
        };
        let paid_out = json!({
            "address": format!("0x{}", hex::encode(path.runner.as_bytes())),
            "topics": [format!("0x{}", hex::encode(types::swapic::topics::paid_out())),
                       format!("0x{}", "11".repeat(32)), word(path.canister)],
            "data": format!("0x{}{:064x}", hex::encode(path.usdc.to_word()), 100_000_000_u64),
        });
        let receipt = json!({ "logs": [transfer(166_275), transfer(100_000_000), paid_out] });
        let fee = network_fee_in(&receipt, path);
        assert_eq!(fee, TokenAmount::from(166_275_u32), "the payout is no fee");
        append_event_at(
            fee_earned(ChainId::BASE, fee, Some(500)),
            Timestamp::from_nanos(1),
        )
        .unwrap();
        funded_pool(usdc(1_000));
        let r#ref = register(swap_terms(1, usdc(100), usdc(1), pool_at(1_000)));
        crate::storage::pool::record_at(
            PoolStep::Arrived {
                r#ref,
                credit: credit(2),
                amount: usdc(100),
                block: BlockNumber::new(500),
            },
            pool_at(3),
        )
        .unwrap();
        let expected = |block: u64| {
            read_state(|state| {
                expected_at(
                    state,
                    ChainId::BASE,
                    Some(ChainId::BASE),
                    BlockNumber::new(block),
                    TokenAmount::ZERO,
                    TokenAmount::ZERO,
                )
                .0
            })
        };
        assert_eq!(expected(499), TokenAmount::ZERO);
        assert_eq!(expected(500), usdc(100).checked_add(fee).unwrap());
    });
}
