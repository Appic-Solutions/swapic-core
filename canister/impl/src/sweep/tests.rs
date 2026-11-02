use super::*;
use crate::storage::events::{append_event_at, read_state};
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{account, at, funded_pool, usdc};
use crate::storage::pool::{ledger, record_at};
use types::books::BookStep;
use types::hash::PoolOpId;
use types::pool::{PayoutAttempt, PayoutPath, PoolAccount, PoolStep, PrincipalBytes};
use types::sweep::{Sweep, SweepTo};
use types::{ChainId, EventType, EvmAddress, TxHash};

fn fee_sweep(id: u64, chain: ChainId, from_pool_fees: u64, from_fee_income: u64) -> Sweep {
    Sweep {
        id,
        asset: SweepAsset::FeeUsdc,
        to: SweepTo::Evm {
            chain,
            to: EvmAddress::new([0x51; 20]),
        },
        amount: usdc(from_pool_fees + from_fee_income),
        from_pool_fees: usdc(from_pool_fees),
        from_fee_income: usdc(from_fee_income),
    }
}

fn swept(sweep: Sweep) -> Result<types::EventIndex, crate::storage::events::AppendError> {
    append_event_at(EventType::Swept { sweep }, at(5))
}

fn reverted(id: u64) -> Result<types::EventIndex, crate::storage::events::AppendError> {
    append_event_at(
        EventType::SweepReverted {
            id,
            why: "reverted".to_string(),
        },
        at(6),
    )
}

/// A USDC fee sweep takes the pool's Base fee line first, then the chain's fee income, and
/// refuses past what both spare.
#[test]
fn a_fee_sweep_takes_the_pool_line_first_and_no_more_than_both() {
    let parts = |amount, pool, income| evm::fee_parts(usdc(amount), usdc(pool), usdc(income));
    assert_eq!(parts(5, 8, 3), Ok((usdc(5), usdc(0))));
    assert_eq!(parts(10, 8, 3), Ok((usdc(8), usdc(2))));
    assert_eq!(parts(11, 8, 3), Ok((usdc(8), usdc(3))));
    assert_eq!(parts(12, 8, 3), Err(usdc(11)));
    assert_eq!(parts(2, 0, 3), Ok((usdc(0), usdc(2))));
}

/// A sweep's line takes its amount off the books it names, in order, never past what they
/// spare; a sweep that moved nothing puts it back, once.
#[test]
fn a_sweep_debits_its_books_and_a_revert_puts_them_back_once() {
    on_fresh_memory(|| {
        crate::storage::init();
        let chain = ChainId::ARBITRUM;
        append_event_at(
            EventType::Books {
                chain_id: chain,
                step: BookStep::FeeEarned {
                    tx_hash: TxHash::new([1; 32]),
                    amount: usdc(5),
                    block: None,
                },
            },
            at(1),
        )
        .unwrap();
        let income = || read_state(|state| state.book(&chain).fee_income);
        // out of order, past the fee income, off a pool line that holds nothing
        assert!(swept(fee_sweep(1, chain, 0, 3)).is_err());
        assert!(swept(fee_sweep(0, chain, 0, 6)).is_err());
        assert!(swept(fee_sweep(0, chain, 1, 3)).is_err());
        // parts that do not add up to the amount
        let mut odd = fee_sweep(0, chain, 0, 3);
        odd.amount = usdc(4);
        assert!(swept(odd).is_err());
        swept(fee_sweep(0, chain, 0, 3)).unwrap();
        assert_eq!(income(), usdc(2));
        assert!(swept(fee_sweep(1, chain, 0, 3)).is_err(), "only 2 left");
        reverted(0).unwrap();
        assert_eq!(income(), usdc(5));
        assert!(reverted(0).is_err(), "taken back once");
        assert!(reverted(1).is_err(), "no such sweep");
        swept(fee_sweep(1, chain, 0, 5)).unwrap();
        assert_eq!(income(), TokenAmount::ZERO);
    });
}

/// The ICP fee line: a ckUSDC sweep takes its amount and the ledger's fee off it, and never
/// more than it holds; ckETH and ICP sweeps touch no line.
#[test]
fn a_ckusdc_sweep_takes_its_amount_and_fee_off_the_icp_fee_line() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        // a stake on ICP pays a move fee of 2 to the ICP fee line
        let id = PoolOpId::new([0x61; 32]);
        record_at(
            PoolStep::StakeStarted {
                id,
                owner: PoolAccount::Icp(account(9)),
                from: account(9),
                amount: usdc(100),
                created_at: at(2),
                move_fee: usdc(2),
            },
            at(2),
        )
        .unwrap();
        let shares = ledger().mint_now(usdc(98)).unwrap();
        record_at(
            PoolStep::StakedIcp {
                id,
                owner: PoolAccount::Icp(account(9)),
                amount: usdc(100),
                move_fee: usdc(2),
                shares,
                block: 4,
            },
            at(3),
        )
        .unwrap();
        assert_eq!(read_state(spare_fees_icp), usdc(2));
        let ckusdc = |amount: u64, taken: u64| Sweep {
            id: 0,
            asset: SweepAsset::CkUsdcFees,
            to: SweepTo::Icp(account(77)),
            amount: TokenAmount::from(amount),
            from_pool_fees: TokenAmount::from(taken),
            from_fee_income: TokenAmount::ZERO,
        };
        // the ledger's fee must come off the line too, and the line holds 2,000,000
        assert!(swept(ckusdc(1_990_000, 1_990_000)).is_err());
        assert!(swept(ckusdc(1_995_000, 2_005_000)).is_err());
        swept(ckusdc(1_990_000, 2_000_000)).unwrap();
        assert_eq!(ledger().lines().fees_icp, TokenAmount::ZERO);
        reverted(0).unwrap();
        assert_eq!(ledger().lines().fees_icp, usdc(2));
        // ICP names no line
        let icp = Sweep {
            id: 1,
            asset: SweepAsset::Icp,
            to: SweepTo::Icp(account(77)),
            amount: TokenAmount::from(5_u8),
            from_pool_fees: TokenAmount::ONE,
            from_fee_income: TokenAmount::ZERO,
        };
        assert!(swept(icp).is_err());
        swept(Sweep {
            from_pool_fees: TokenAmount::ZERO,
            ..icp
        })
        .unwrap();
    });
}

/// ICP and ckETH are refused while anything may hold them between legs: an ICP withdrawal
/// still paying. ckUSDC fees are bounded by their line, never by open work.
#[test]
fn icp_is_not_swept_while_a_withdrawal_may_hold_it() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        assert_eq!(read_state(|state| busy(state, SweepAsset::Icp)), None);
        let owner = account(200);
        let burned = read_state(|state| {
            state
                .store()
                .pool_share(&PoolAccount::Icp(owner).key())
                .unwrap()
                .shares
        });
        let burn =
            types::pool::Shares::burn_for(usdc(10), ledger().total_assets(), ledger().total_shares)
                .unwrap()
                .min(burned);
        record_at(
            PoolStep::WithdrawStarted {
                id: PoolOpId::new([0x62; 32]),
                owner,
                burned: burn,
                usdc: usdc(10),
                to: owner,
                token_out: crate::storage::pool::tests::ckusdc(),
                min_out: 0,
                attempt: PayoutAttempt {
                    created_at: at(4),
                    memo: [0x62; 32],
                    path: PayoutPath::Ckusdc,
                    amount: None,
                    token: Some(PrincipalBytes(crate::storage::pool::tests::ckusdc())),
                },
                move_fee: TokenAmount::ZERO,
            },
            at(4),
        )
        .unwrap();
        assert!(read_state(|state| busy(state, SweepAsset::Icp)).is_some());
        assert!(read_state(|state| busy(state, SweepAsset::CkEth)).is_some());
        assert_eq!(
            read_state(|state| busy(state, SweepAsset::CkUsdcFees)),
            None
        );
    });
}
