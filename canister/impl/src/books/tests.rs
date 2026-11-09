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

/// The spec defaults (hourly checks) with Base as the pool chain.
fn pool_config() -> types::Config {
    types::Config {
        pool_chain: Some(ChainId::BASE),
        ..types::Config::default()
    }
}

fn status(halted: bool, now: Timestamp) -> Result<(), PoolClosed> {
    let reads = timer_state::get();
    read_state(|state| pool_status(state, &state.pool(), &reads, &pool_config(), halted, now))
}

/// A read of `chain_id` that passed at `minute`, kept as the hourly check keeps it.
fn read_passed(chain_id: ChainId, minute: u64) {
    timer_state::update(|state| {
        state.books_reads.entry(chain_id).or_default().read = Some((
            at_minute(minute),
            BalanceCheck {
                block: BlockNumber::new(minute),
                observed: TokenAmount::ZERO,
                expected: TokenAmount::ZERO,
            },
        ));
        ((), true)
    });
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
        read_passed(ChainId::BASE, 1);
        assert_eq!(status(false, at_minute(2)), Ok(()));
        assert_eq!(status(true, at_minute(2)), Err(PoolClosed::Halted));
        let reads = timer_state::get();
        assert_eq!(
            read_state(|state| pool_status(
                state,
                &state.pool(),
                &reads,
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
        // a clean read does not lift a shortfall: a controller looks first
        read_passed(ChainId::BASE, 5);
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

/// Finding 2: a pool chain not read lately closes the pool, and so does one never read.
#[test]
fn stale_books_close_the_pool() {
    on_fresh_memory(|| {
        crate::storage::init();
        assert_eq!(
            status(false, at_minute(1)),
            Err(PoolClosed::NeverChecked(ChainId::BASE))
        );
        read_passed(ChainId::BASE, 10);
        // three hourly intervals is the most the pool chain may go unread
        assert_eq!(status(false, at_minute(190)), Ok(()));
        assert!(matches!(
            status(false, at_minute(191)),
            Err(PoolClosed::Stale { .. })
        ));
        // a read of another chain is not the pool chain's
        read_passed(ChainId::ARBITRUM, 191);
        assert!(matches!(
            status(false, at_minute(192)),
            Err(PoolClosed::Stale { .. })
        ));
    });
}

/// Finding 2: a failed read writes no line and refreshes nothing the guard reads, so a
/// chain whose balance cannot be read goes stale and closes the pool.
#[test]
fn failed_read_does_not_refresh_the_check() {
    on_fresh_memory(|| {
        crate::storage::init();
        read_passed(ChainId::BASE, 10);
        let count = crate::storage::events::event_count();
        read_failed(ChainId::BASE, ReadFailure::Unreachable, at_minute(35));
        assert_eq!(crate::storage::events::event_count(), count, "no line");
        let read = timer_state::get().books_reads[&ChainId::BASE];
        assert_eq!(read.read_at(), Some(at_minute(10)));
        assert_eq!(read.failed_at, Some(at_minute(35)));
        assert!(matches!(
            status(false, at_minute(191)),
            Err(PoolClosed::Stale { .. })
        ));
    });
}

/// A failed read keeps no trace of its error's text: an outcall's refusal or a provider's
/// answer can carry the provider's url and its api key. Only the failure's kind is printed,
/// and only the time it failed at is kept.
#[test]
fn a_failed_read_leaves_no_secret_behind() {
    use crate::rpc::{RpcError, Summary};
    const SECRET: &str = "https://base-mainnet.g.alchemy.com/v2/sk-live-0123456789";
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
    let kinds: Vec<ReadFailure> = errors.iter().map(RpcError::kind).collect();
    assert_eq!(
        kinds,
        vec![
            ReadFailure::Unreachable,
            ReadFailure::Http { status: 401 },
            ReadFailure::Provider
        ]
    );
    for (error, kind) in errors.iter().zip(kinds) {
        assert!(
            error.to_string().contains(SECRET),
            "the error itself carries it"
        );
        let printed = format!("{kind:?}");
        for secret in [SECRET, "sk-live", "alchemy"] {
            assert!(!printed.contains(secret), "{printed}");
        }
    }
}

/// The hourly check's read: one that matches the last line writes none and is kept beside
/// the timers, dropping what its balance holds from the unseen lists; one with news is a
/// line; a read below the last read's block is refused and keeps nothing.
#[test]
fn a_read_with_no_news_is_kept_without_a_line() {
    on_fresh_memory(|| {
        crate::storage::init();
        let config = pool_config();
        append_event_at(
            fee_earned(ChainId::BASE, usdc_micro(400), Some(20)),
            at_minute(1),
        )
        .unwrap();
        let count = crate::storage::events::event_count();
        // at block 10 the fee from block 20 is not in the balance yet: nothing is short
        keep_read(
            &config,
            ChainId::BASE,
            BlockNumber::new(10),
            TokenAmount::ZERO,
            at_minute(2),
        )
        .unwrap();
        assert_eq!(
            crate::storage::events::event_count(),
            count,
            "no news, no line"
        );
        assert_eq!(
            timer_state::get().books_reads[&ChainId::BASE].read_at(),
            Some(at_minute(2))
        );
        let unseen = || read_state(|state| state.book(&ChainId::BASE).unseen_fees.0.len());
        assert_eq!(unseen(), 1, "the fee is still unseen at block 10");
        // at block 20 it is in the balance, and leaves the list
        keep_read(
            &config,
            ChainId::BASE,
            BlockNumber::new(20),
            usdc_micro(400),
            at_minute(3),
        )
        .unwrap();
        assert_eq!(crate::storage::events::event_count(), count);
        assert_eq!(unseen(), 0);
        // a read further back is refused and keeps nothing
        assert_eq!(
            keep_read(
                &config,
                ChainId::BASE,
                BlockNumber::new(19),
                usdc_micro(400),
                at_minute(4)
            ),
            Err(ReadFailure::Behind)
        );
        assert_eq!(
            timer_state::get().books_reads[&ChainId::BASE].read_at(),
            Some(at_minute(3))
        );
        // a donation is news: a line, then the same surplus again is none
        keep_read(
            &config,
            ChainId::BASE,
            BlockNumber::new(30),
            usdc_micro(900),
            at_minute(5),
        )
        .unwrap();
        assert_eq!(crate::storage::events::event_count(), count + 1);
        keep_read(
            &config,
            ChainId::BASE,
            BlockNumber::new(40),
            usdc_micro(900),
            at_minute(6),
        )
        .unwrap();
        assert_eq!(crate::storage::events::event_count(), count + 1);
        // a shortfall is always a line, and pauses the pool
        keep_read(
            &config,
            ChainId::BASE,
            BlockNumber::new(50),
            TokenAmount::ZERO,
            at_minute(7),
        )
        .unwrap();
        assert_eq!(crate::storage::events::event_count(), count + 2);
        assert_eq!(
            status(false, at_minute(8)),
            Err(PoolClosed::Shortfall(ChainId::BASE))
        );
    });
}

/// Only the pool chain's fees carry their block: no other chain is checked, so another
/// chain's fee would sit in its unseen list for good.
#[test]
fn only_the_pool_chains_fees_wait_for_a_check() {
    let tx = TxHash::new([9; 32]);
    let block = BlockNumber::new(7);
    let fee = TokenAmount::from(5_u8);
    let with_block = |chain| match super::fee_earned(chain, Some(ChainId::BASE), tx, block, fee) {
        BookStep::FeeEarned { block, .. } => block,
        _ => unreachable!(),
    };
    assert_eq!(with_block(ChainId::BASE), Some(block));
    assert_eq!(with_block(ChainId::ARBITRUM), None);
}

/// Whole USDC, in micro-USDC.
fn usdc_micro(amount: u64) -> TokenAmount {
    TokenAmount::from(amount * 1_000_000)
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

// ---- the fees users pay in their own transactions, on real mainnet receipts

const REAL_CANISTER: &str = "0x8Fd51f1C630ae3AFd0Bf1AFFeCc5cFBdB66e3023";
const REAL_RUNNER: &str = "0x34d8a2debe49dcca0f7cc77d74163cfb94cc19f5";

fn real_canister() -> EvmAddress {
    REAL_CANISTER.parse().unwrap()
}

/// A chain as far as the fee reader cares: its USDC and the one shared Runner.
fn source_chain(usdc: &str, decimals: Option<u8>) -> EvmChain {
    EvmChain {
        swapic: EvmAddress::new([0x51; 20]),
        runner: REAL_RUNNER.parse().unwrap(),
        cctp_receiver: EvmAddress::new([0x52; 20]),
        usdc: usdc.parse().unwrap(),
        token_messenger: EvmAddress::new([0x53; 20]),
        message_transmitter: EvmAddress::new([0x54; 20]),
        gas_limit: types::config::PerAction::all(types::GasAmount::from(300_000_u64)),
        gas_budget: types::config::PerAction::all(types::Wei::ZERO),
        min_network_fee: types::config::PerAction::all(TokenAmount::ZERO),
        usdc_name: None,
        usdc_version: None,
        block_time: std::time::Duration::from_secs(2),
        usdc_decimals: decimals,
        across: None,
    }
}

fn base_usdc() -> EvmChain {
    source_chain("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913", None)
}

fn real(sample: &str) -> Receipt {
    let json: serde_json::Value = serde_json::from_str(sample).unwrap();
    Receipt::parse(&json).unwrap()
}

fn user_pays_base() -> Receipt {
    real(include_str!("samples/user_pays_base_to_arbitrum.json"))
}

fn empty_state() -> State<crate::state::MemoryStore> {
    State::new(crate::state::MemoryStore::default())
}

/// A "You pay gas" swap from Base: 2 USDC in, $0.06461 to the canister, the rest burned.
#[test]
fn a_user_paid_fee_is_read_off_a_real_receipt() {
    let fee = user_fee_in(&user_pays_base(), &base_usdc(), real_canister());
    assert_eq!(fee, TokenAmount::from(64_610_u32));
}

/// A stake from Polygon pays its network fee on Polygon, in its own transaction.
#[test]
fn a_stake_from_another_chain_pays_its_fee_there() {
    let polygon = source_chain("0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359", None);
    let receipt = real(include_str!("samples/user_pays_stake_polygon.json"));
    let fee = user_fee_in(&receipt, &polygon, real_canister());
    assert_eq!(fee, TokenAmount::from(53_498_u32));
}

/// The $260 same chain stake reaches the canister through the Runner too, explained by a
/// `PaidOut`: pool money, never a fee.
#[test]
fn a_same_chain_deposit_is_no_fee() {
    let receipt = real(include_str!("samples/same_chain_deposit_base.json"));
    let step = user_fee_step(
        &empty_state(),
        ChainId::BASE,
        Some(ChainId::BASE),
        &receipt,
        &base_usdc(),
        real_canister(),
    );
    assert_eq!(step, Err(NoUserFee::NoFee));
}

/// BNB Chain's USDC has 18 decimals, and the books count micro USD.
#[test]
fn a_fee_on_an_18_decimal_usdc_books_in_micro_usd() {
    let bsc = source_chain("0x8AC76a51cc950d9822D68b83fE1Ad97B32Cd580d", Some(18));
    let receipt = real(include_str!("samples/gasless_relay_bsc_18_decimals.json"));
    let step = user_fee_step(
        &empty_state(),
        ChainId::new(56),
        Some(ChainId::BASE),
        &receipt,
        &bsc,
        real_canister(),
    );
    let Ok(BookStep::UserFeeEarned { amount, block, .. }) = step else {
        panic!("{step:?}");
    };
    assert_eq!(amount, TokenAmount::from(91_239_u32));
    assert_eq!(block, None, "only the pool chain's fees carry a block");
}

/// On the pool chain the line carries its receipt's block, for the balance check.
#[test]
fn a_fee_on_the_pool_chain_carries_its_block() {
    let receipt = user_pays_base();
    let step = user_fee_step(
        &empty_state(),
        ChainId::BASE,
        Some(ChainId::BASE),
        &receipt,
        &base_usdc(),
        real_canister(),
    );
    let Ok(BookStep::UserFeeEarned {
        tx_hash,
        amount,
        block,
    }) = step
    else {
        panic!("{step:?}");
    };
    assert_eq!(tx_hash, receipt.tx);
    assert_eq!(amount, TokenAmount::from(64_610_u32));
    assert_eq!(block, Some(receipt.block));
}

/// A relay the canister signed is booked at its close: the finish that reads it books nothing,
/// and that holds whichever of the two comes first.
#[test]
fn a_transaction_the_canister_signed_is_not_booked_again() {
    let arbitrum = source_chain("0xaf88d065e77c8cC2239327C5EDb3A432268e5831", None);
    let receipt = real(include_str!("samples/gasless_relay_arbitrum.json"));
    let canister = real_canister();
    let chain = ChainId::new(42_161);

    let unknown = user_fee_step(&empty_state(), chain, None, &receipt, &arbitrum, canister);
    assert!(
        unknown.is_ok(),
        "a stranger who relays it is booked: {unknown:?}"
    );

    let mut store = crate::state::MemoryStore::default();
    store.put_flow_tx(receipt.tx, types::QuoteHash::new([1; 32]));
    let relayed = user_fee_step(
        &State::new(store),
        chain,
        None,
        &receipt,
        &arbitrum,
        canister,
    );
    assert_eq!(relayed, Err(NoUserFee::Ours));
}

/// A reverted source took nothing from the user: its logs are gone, and its status says so even
/// if a provider answered logs.
#[test]
fn a_reverted_source_books_nothing() {
    let mut receipt = user_pays_base();
    receipt.success = false;
    let step = user_fee_step(
        &empty_state(),
        ChainId::BASE,
        None,
        &receipt,
        &base_usdc(),
        real_canister(),
    );
    assert_eq!(step, Err(NoUserFee::Reverted));
}

/// Two finishes of one transaction, a retry, or the catch-up beside a live finish: one line. The
/// fold refuses a second whoever writes it, the check before it answers quietly, and the fee
/// landed on the source chain's books, the income line and the lifetime line alike.
#[test]
fn a_user_paid_fee_is_booked_once() {
    on_fresh_memory(|| {
        crate::storage::init();
        let receipt = user_pays_base();
        let chain = ChainId::BASE;
        let at = Timestamp::from_nanos(1);
        let step = read_state(|state| {
            user_fee_step(
                state,
                chain,
                Some(chain),
                &receipt,
                &base_usdc(),
                real_canister(),
            )
        })
        .unwrap();
        let line = EventType::Books {
            chain_id: chain,
            step,
        };
        append_event_at(line.clone(), at).unwrap();

        let books = read_state(|state| state.book(&chain));
        assert_eq!(books.fee_income, TokenAmount::from(64_610_u32));
        assert_eq!(books.user_paid(), TokenAmount::from(64_610_u32));

        let again = read_state(|state| {
            user_fee_step(
                state,
                chain,
                Some(chain),
                &receipt,
                &base_usdc(),
                real_canister(),
            )
        });
        assert_eq!(again, Err(NoUserFee::Booked));
        assert!(
            append_event_at(line, at).is_err(),
            "the fold refuses a second line for one transaction"
        );
        let books = read_state(|state| state.book(&chain));
        assert_eq!(books.fee_income, TokenAmount::from(64_610_u32));
    });
}

/// The same hash on another chain is another key: a key is its chain and its hash.
#[test]
fn the_key_is_the_chain_and_the_hash() {
    on_fresh_memory(|| {
        crate::storage::init();
        let at = Timestamp::from_nanos(1);
        let line = |chain_id| EventType::Books {
            chain_id,
            step: BookStep::UserFeeEarned {
                tx_hash: TxHash::new([7; 32]),
                amount: TokenAmount::from(1_u8),
                block: None,
            },
        };
        append_event_at(line(ChainId::BASE), at).unwrap();
        append_event_at(line(ChainId::ETHEREUM), at).unwrap();
        assert!(append_event_at(line(ChainId::BASE), at).is_err());
    });
}

/// What the live books showed before: the fee in the balance and not on a line reads as surplus.
/// Booked, the surplus is gone, and the balance check expects exactly what is there.
#[test]
fn booking_a_user_paid_fee_takes_it_out_of_the_surplus() {
    on_fresh_memory(|| {
        crate::storage::init();
        let tolerance = TokenAmount::from(1_000_000_u32);
        let block = BlockNumber::new(100_000_000);
        let observed = TokenAmount::from(64_610_u32);
        let chain = ChainId::BASE;
        let before =
            read_state(|state| expected_at(state, chain, Some(chain), block, observed, tolerance));
        assert_eq!(types::books::surplus_of(observed, before.0), 64_610);

        let step = read_state(|state| {
            user_fee_step(
                state,
                chain,
                Some(chain),
                &user_pays_base(),
                &base_usdc(),
                real_canister(),
            )
        })
        .unwrap();
        append_event_at(
            EventType::Books {
                chain_id: chain,
                step,
            },
            Timestamp::from_nanos(1),
        )
        .unwrap();

        let after =
            read_state(|state| expected_at(state, chain, Some(chain), block, observed, tolerance));
        assert_eq!(types::books::surplus_of(observed, after.0), 0);
        assert!(!after.1, "no shortfall");
    });
}
