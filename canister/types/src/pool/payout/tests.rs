use super::*;
use std::borrow::Cow;

fn withdrawal(usdc: u64, move_fee: u64, network_fee: u64) -> EvmWithdrawal {
    EvmWithdrawal {
        staker: EvmAddress::new([0x11; 20]),
        usdc: TokenAmount::from(usdc),
        max_shares: Shares::new(10),
        move_fee: TokenAmount::from(move_fee),
        network_fee: TokenAmount::from(network_fee),
        nonce: 7,
        burned: Shares::new(9),
    }
}

#[test]
fn a_withdrawal_pays_what_is_left_after_its_share_and_its_fee() {
    assert_eq!(
        withdrawal(5_000_000, 1_000, 50_000).payout_amount(),
        Some(TokenAmount::from(4_949_000_u64))
    );
    assert_eq!(withdrawal(51_000, 1_000, 50_000).payout_amount(), None);
    assert_eq!(withdrawal(50_000, 1_000, 50_000).payout_amount(), None);
}

#[test]
fn the_settings_hold_together_or_are_refused() {
    let good = PayoutSettings::default();
    assert_eq!(good.validate(), Ok(()));
    assert_eq!(
        PayoutSettings {
            order_wait: Duration::ZERO,
            ..good
        }
        .validate(),
        Err(PayoutSettingsError::OrderWait)
    );
    assert_eq!(
        PayoutSettings {
            max_payout_gas: MIN_PAYOUT_GAS - 1,
            ..good
        }
        .validate(),
        Err(PayoutSettingsError::Gas)
    );
    assert_eq!(
        PayoutSettings {
            max_payout_gas: MAX_PAYOUT_GAS + 1,
            ..good
        }
        .validate(),
        Err(PayoutSettingsError::Gas)
    );
}

/// A CCTP payout is signed only while CCTP payouts are on, and only when its network fee
/// covers the destination's floor: the pool chain's least fee for a burning order and the
/// destination's for the finish.
#[test]
fn a_cctp_payout_needs_the_switch_and_the_floor() {
    let floor =
        cctp_payout_floor(TokenAmount::from(40_000_u64), TokenAmount::from(90_000_u64)).unwrap();
    assert_eq!(floor, TokenAmount::from(130_000_u64));
    let off = PayoutSettings::default();
    assert!(off
        .admits_cctp(TokenAmount::from(1_000_000_u64), floor)
        .is_err());
    let on = PayoutSettings {
        cctp_payouts: true,
        ..off
    };
    assert_eq!(on.admits_cctp(floor, floor), Ok(()));
    assert!(on
        .admits_cctp(TokenAmount::from(129_999_u64), floor)
        .is_err());
    assert_eq!(
        cctp_payout_floor(TokenAmount::MAX, TokenAmount::from(1_u64)),
        None
    );
}

#[test]
fn a_nonce_use_orders_by_staker_then_nonce_and_round_trips() {
    let a = NonceUse {
        staker: EvmAddress::new([0x01; 20]),
        nonce: u64::MAX,
    };
    let b = NonceUse {
        staker: EvmAddress::new([0x02; 20]),
        nonce: 0,
    };
    assert!(a.to_bytes() < b.to_bytes());
    assert_eq!(NonceUse::from_bytes(Cow::Owned(a.to_bytes().to_vec())), a);
    assert_eq!(a.to_bytes().len(), 28);
}

#[test]
fn a_payout_holds_its_amount_and_a_withdrawals_fee_on_base() {
    let mut payout = EvmPayout::new(
        1,
        PayoutKind::Withdrawal(withdrawal(5_000_000, 1_000, 50_000)),
        PayoutTerms {
            dst_chain: ChainId::BASE,
            recipient: EvmAddress::new([0x22; 20]),
            token_out: EvmAddress::new([0x33; 20]),
            min_out: TokenAmount::from(1_u8),
            deadline: Timestamp::from_nanos(9),
        },
        Undo::Remint {
            owner: PoolAccount::Evm(EvmAddress::new([0x11; 20])),
            shares: Shares::new(9),
        },
        Timestamp::from_nanos(1),
    );
    assert_eq!(payout.owed(), TokenAmount::ZERO);
    payout.amount = Some(TokenAmount::from(4_949_000_u64));
    payout.base_fee = Some(TokenAmount::from(50_000_u64));
    assert_eq!(payout.base_hold(), TokenAmount::from(4_999_000_u64));
    assert_eq!(payout.owed(), TokenAmount::from(4_999_000_u64));
    assert_eq!(payout.icp_hold(), TokenAmount::ZERO);
    assert!(!payout.status.is_final());
}
