use super::*;
use candid::{Decode, Encode};
use types::pool::payout as domain;
use types::{EvmAddress, TokenAmount};

fn terms() -> PayoutTerms {
    PayoutTerms {
        dst_chain: 42_161,
        recipient: "0xf00EBA8E5Fd231b526ADFAAa68B4d0ede72A89AC".to_string(),
        token_out: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831".to_string(),
        min_out: Nat::from(4_949_000_u64),
        deadline_ns: 1_800_000_000_000_000_000,
    }
}

#[test]
fn payout_terms_cross_the_wire_and_back() {
    let wire = terms();
    let bytes = Encode!(&wire).unwrap();
    assert_eq!(Decode!(&bytes, PayoutTerms).unwrap(), wire);
    let domain = domain::PayoutTerms::try_from(wire.clone()).unwrap();
    assert_eq!(domain.dst_chain.get(), 42_161);
    assert_eq!(domain.min_out, TokenAmount::from(4_949_000_u64));
    assert_eq!(PayoutTerms::from(domain), wire);
}

#[test]
fn payout_terms_refuse_what_is_no_address() {
    let bad = PayoutTerms {
        recipient: "0x12".to_string(),
        ..terms()
    };
    assert_eq!(
        domain::PayoutTerms::try_from(bad),
        Err(PoolWireError::NotAnAddress { field: "recipient" })
    );
}

#[test]
fn payout_settings_cross_the_wire_and_back() {
    let wire = PayoutSettings {
        min_network_fee: Nat::from(50_000_u64),
        rebalance_share_floor_bps: 2,
        order_wait_s: 120,
        max_payout_gas: 2_000_000,
        cctp_min_network_fee: None,
    };
    let bytes = Encode!(&wire).unwrap();
    assert_eq!(Decode!(&bytes, PayoutSettings).unwrap(), wire);
    let domain = domain::PayoutSettings::try_from(wire.clone()).unwrap();
    assert_eq!(domain, domain::PayoutSettings::default());
    assert_eq!(PayoutSettings::from(&domain), wire);
    // the CCTP payouts' least fee, set: a payout below it is refused, at it signed
    let cctp = PayoutSettings {
        cctp_min_network_fee: Some(Nat::from(300_000_u64)),
        ..wire
    };
    let domain = domain::PayoutSettings::try_from(cctp.clone()).unwrap();
    assert_eq!(PayoutSettings::from(&domain), cctp);
    assert!(domain.admits_cctp(TokenAmount::from(299_999_u64)).is_err());
    assert_eq!(domain.admits_cctp(TokenAmount::from(300_000_u64)), Ok(()));
    assert!(domain::PayoutSettings::default()
        .admits_cctp(TokenAmount::MAX)
        .is_err());
}

#[test]
fn a_payout_view_says_what_the_payout_spends_and_where_it_stands() {
    let staker: EvmAddress = "0xf00EBA8E5Fd231b526ADFAAa68B4d0ede72A89AC"
        .parse()
        .unwrap();
    let mut payout = domain::EvmPayout::new(
        7,
        domain::PayoutKind::Withdrawal(domain::EvmWithdrawal {
            staker,
            usdc: TokenAmount::from(5_000_000_u64),
            max_shares: types::pool::Shares::new(5_000_000_000_000_000_000),
            rebalance_share: TokenAmount::from(1_000_u64),
            network_fee: TokenAmount::from(50_000_u64),
            nonce: 7,
            burned: types::pool::Shares::new(4_000_000_000_000_000_000),
        }),
        domain::PayoutTerms::try_from(terms()).unwrap(),
        domain::Undo::Remint {
            owner: types::pool::PoolAccount::Evm(staker),
            shares: types::pool::Shares::new(4_000_000_000_000_000_000),
        },
        types::Timestamp::from_nanos(1),
    );
    payout.amount = Some(TokenAmount::from(4_949_000_u64));
    payout.status = domain::PayoutStatus::Signed(42);
    payout.order = Some(domain::PinnedOrder {
        order_id: [9; 32],
        order: vec![1, 2, 3],
        gas_limit: 300_000,
        deadline_s: 1_800_000_000,
    });
    let view = PayoutView::from(&payout);
    assert_eq!(view.kind, PayoutKind::Withdrawal);
    assert_eq!(view.amount, Some(Nat::from(4_949_000_u64)));
    assert_eq!(view.status, PayoutStatus::Signed(42));
    assert_eq!(view.order_id, Some(vec![9; 32]));
    let bytes = Encode!(&view).unwrap();
    assert_eq!(Decode!(&bytes, PayoutView).unwrap(), view);
    let waiting = PayoutWaiting::of(&payout).unwrap();
    assert_eq!(waiting.amount, Nat::from(4_949_000_u64));
    // an undo that began reads as undone: nothing is signed for it again
    payout.status = domain::PayoutStatus::Undoing("past its deadline".to_string());
    assert_eq!(
        PayoutStatus::from(&payout.status),
        PayoutStatus::Undone("past its deadline".to_string())
    );
}

#[test]
fn icp_swap_terms_and_withdrawals_cross_the_wire() {
    let swap = IcpSwapTerms {
        token_in: Principal::from_slice(&[1; 10]),
        amount_in: Nat::from(1_000_000_u64),
        from_subaccount: None,
        min_usdc: Nat::from(1_000_000_u64),
        premium: Nat::from(10_000_u64),
        network_fee: Nat::from(50_000_u64),
        payout: terms(),
        refund_to: None,
    };
    let bytes = Encode!(&swap).unwrap();
    assert_eq!(Decode!(&bytes, IcpSwapTerms).unwrap(), swap);
    let withdrawal = EvmWithdrawal {
        staker: "0xf00EBA8E5Fd231b526ADFAAa68B4d0ede72A89AC".to_string(),
        usdc: Nat::from(5_000_000_u64),
        max_shares: Nat::from(5_000_000_000_000_000_000_u128),
        rebalance_share: Nat::from(1_000_u64),
        network_fee: Nat::from(50_000_u64),
        payout: terms(),
        nonce: 7,
    };
    let bytes = Encode!(&withdrawal).unwrap();
    assert_eq!(Decode!(&bytes, EvmWithdrawal).unwrap(), withdrawal);
}

/// P11e: a waiting payout names its own network fee, which the watcher's choice of CCTP reads.
#[test]
fn a_waiting_payout_names_its_network_fee() {
    use types::pool::payout::{EvmWithdrawal as Withdrawal, PayoutKind as Kind, Undo};
    use types::pool::{PoolAccount, Shares};
    let staker = types::EvmAddress::new([0x11; 20]);
    let mut payout = domain::EvmPayout::new(
        4,
        Kind::Withdrawal(Withdrawal {
            staker,
            usdc: types::TokenAmount::from(10_000_000_u64),
            max_shares: Shares::new(1),
            rebalance_share: types::TokenAmount::ZERO,
            network_fee: types::TokenAmount::from(70_000_u64),
            nonce: 1,
            burned: Shares::new(1),
        }),
        domain::PayoutTerms {
            dst_chain: types::ChainId::BASE,
            recipient: staker,
            token_out: staker,
            min_out: types::TokenAmount::ONE,
            deadline: types::Timestamp::from_nanos(9),
        },
        Undo::Remint {
            owner: PoolAccount::Evm(staker),
            shares: Shares::new(1),
        },
        types::Timestamp::from_nanos(1),
    );
    assert_eq!(PayoutWaiting::of(&payout), None, "its amount is not known yet");
    payout.amount = Some(types::TokenAmount::from(9_930_000_u64));
    let waiting = PayoutWaiting::of(&payout).unwrap();
    assert_eq!(waiting.network_fee, Some(Nat::from(70_000_u64)));
    let bytes = candid::Encode!(&waiting).unwrap();
    assert_eq!(candid::Decode!(&bytes, PayoutWaiting).unwrap(), waiting);
}
