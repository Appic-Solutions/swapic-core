use super::*;
use candid::{Decode, Encode};

fn ckusdc() -> Principal {
    Principal::from_text(types::pool::CKUSDC_LEDGER).unwrap()
}

fn icp(subaccount: Option<Vec<u8>>) -> IcpAccount {
    IcpAccount {
        owner: Principal::from_slice(&[7; 10]),
        subaccount,
    }
}

#[test]
fn an_evm_address_in_any_case_is_the_same_account() {
    let checksummed = "0xf00EBA8E5Fd231b526ADFAAa68B4d0ede72A89AC";
    let accounts: Vec<types::pool::PoolAccount> = [
        checksummed.to_string(),
        checksummed.to_ascii_lowercase(),
        format!("0x{}", checksummed[2..].to_ascii_uppercase()),
    ]
    .into_iter()
    .map(|text| PoolAccount::Evm(text).try_into().unwrap())
    .collect();
    assert!(accounts.windows(2).all(|pair| pair[0] == pair[1]));
    assert!(accounts
        .windows(2)
        .all(|pair| pair[0].key() == pair[1].key()));
    // a mixed case that is not the checksum is refused
    let bad = "0xF00EBA8E5Fd231b526ADFAAa68B4d0ede72A89AC".to_string();
    assert_eq!(
        types::pool::PoolAccount::try_from(PoolAccount::Evm(bad)),
        Err(PoolWireError::NotAnAddress { field: "owner" })
    );
    // and it reads back as its checksum
    assert_eq!(
        PoolAccount::from(accounts[0]),
        PoolAccount::Evm(checksummed.to_string())
    );
}

#[test]
fn a_subaccount_is_thirty_two_bytes() {
    assert_eq!(
        types::pool::IcpAccount::try_from(icp(Some(vec![1; 31]))),
        Err(PoolWireError::NotThirtyTwo {
            field: "subaccount",
            len: 31
        })
    );
    let account = types::pool::IcpAccount::try_from(icp(Some(vec![1; 32]))).unwrap();
    assert_eq!(account.subaccount, Some([1; 32]));
    assert_eq!(IcpAccount::from(account), icp(Some(vec![1; 32])));
    let none = types::pool::IcpAccount::try_from(icp(None)).unwrap();
    assert_eq!(IcpAccount::from(none), icp(None));
}

/// A memo's terms reach a deposit view whole: the arrival's registration carries every term
/// the memo committed, and the view gives them back.
#[test]
fn a_memos_terms_reach_the_deposit_view() {
    let terms = types::pool::DepositTerms {
        kind: types::pool::DepositKind::Swap {
            recipient: types::pool::IcpAccount {
                owner: Principal::anonymous(),
                subaccount: None,
            },
            token_out: ckusdc(),
            min_out: 1_000_000,
        },
        min_usdc: TokenAmount::from(1_100_000_u64),
        premium: TokenAmount::from(10_000_u64),
        icp_fees: TokenAmount::from(10_000_u64),
        move_fee: TokenAmount::ZERO,
        salt: [0x11; 32],
    };
    let memo = terms.memo().unwrap();
    let r#ref = types::pool::memo::memo_ref(&memo);
    let registration = types::pool::DepositTerms::from_memo(&memo)
        .unwrap()
        .registration(
            types::ChainId::ARBITRUM,
            None,
            types::pool::PoolRail::Cctp,
            types::Timestamp::from_nanos(1_800_000_000_000_000_000),
        );
    assert_eq!(registration.pool_ref(), Some(r#ref));
    assert_eq!(registration.src_payer, EvmAddress::ZERO);
    let view = PoolDepositView::of(r#ref, types::pool::Registered::new(registration));
    assert_eq!(view.kind, PoolDepositKind::from(terms.kind));
    assert_eq!(view.min_usdc, Nat::from(1_100_000_u64));
    assert_eq!(view.premium, Nat::from(10_000_u64));
    assert_eq!(view.move_fee, Nat::from(0_u8));
    assert_eq!(view.status, PoolDepositStatus::Registered);
}

#[test]
fn settings_go_to_the_domain_and_back() {
    let wire = PoolSettings {
        tvl_cap: Nat::from(10_000_000_000_u64),
        per_swap_cap: Nat::from(1_000_000_000_u64),
        premium_bps: 10,
        ckusdc_ledger: ckusdc(),
        icp_tokens: vec![IcpTokenEntry {
            ledger: Principal::from_slice(&[1; 10]),
            icpswap_pool: Principal::from_slice(&[2; 10]),
        }],
        arrival_backup_s: 300,
        pool_open_for_swaps: true,
        large_deposit_usdc: Some(Nat::from(1_000_000_000_u64)),
        large_deposit_confirmations: Some(6),
    };
    let domain = types::pool::PoolSettings::try_from(wire.clone()).unwrap();
    assert_eq!(domain.arrival_backup, Duration::from_secs(300));
    assert_eq!(domain.validate(), Ok(()));
    assert!(
        domain.ledger_fees.is_empty(),
        "the fees are read, never sent"
    );
    assert_eq!(PoolSettings::from(&domain), wire);
    // the domain's defaults are the interface's launch values
    let defaults = PoolSettings::from(&types::pool::PoolSettings::default());
    assert_eq!(
        defaults,
        PoolSettings {
            icp_tokens: vec![],
            pool_open_for_swaps: false,
            ..wire
        }
    );
}

/// The deposit tiers (2026-10-23) are `opt` on the wire: a caller that sends the settings as
/// they were before them still decodes, with no tier, and the canister answers the defaults
/// it runs on.
#[test]
fn settings_sent_without_the_deposit_tiers_still_decode() {
    #[derive(candid::CandidType)]
    struct Before {
        tvl_cap: Nat,
        per_swap_cap: Nat,
        premium_bps: u16,
        ckusdc_ledger: Principal,
        icp_tokens: Vec<IcpTokenEntry>,
        arrival_backup_s: u64,
        pool_open_for_swaps: bool,
    }
    let bytes = Encode!(&Before {
        tvl_cap: Nat::from(10_000_000_000_u64),
        per_swap_cap: Nat::from(1_000_000_000_u64),
        premium_bps: 10,
        ckusdc_ledger: ckusdc(),
        icp_tokens: vec![],
        arrival_backup_s: 300,
        pool_open_for_swaps: false,
    })
    .unwrap();
    let decoded = Decode!(&bytes, PoolSettings).unwrap();
    assert_eq!(decoded.large_deposit_usdc, None);
    assert_eq!(decoded.large_deposit_confirmations, None);
    let domain = types::pool::PoolSettings::try_from(decoded).unwrap();
    assert_eq!(
        domain.large_deposit(),
        (
            types::pool::DEFAULT_LARGE_DEPOSIT_USDC,
            types::BlockDepth::new(6)
        )
    );
}

#[test]
fn every_wire_type_survives_candid() {
    let arrival = PoolArrival {
        base_tx: vec![0xab; 32],
        src_chain: 42_161,
        src_tx: vec![0xcd; 32],
        memo: Some(vec![1, 1, 2, 3]),
    };
    let bytes = Encode!(&arrival).unwrap();
    assert_eq!(Decode!(&bytes, PoolArrival).unwrap(), arrival);
    let errors = [
        PoolError::Closed("paused".into()),
        PoolError::AboveTvlCap {
            cap: Nat::from(1_u8),
            would_be: Nat::from(2_u8),
        },
        PoolError::NotDeep { needed: 1, have: 0 },
        PoolError::TokenNotListed(ckusdc()),
    ];
    for error in errors {
        let bytes = Encode!(&Result::<(), PoolError>::Err(error.clone())).unwrap();
        assert_eq!(Decode!(&bytes, Result<(), PoolError>).unwrap(), Err(error));
    }
    let outcome = PoolArrivalOutcome::Credited {
        r#ref: vec![1; 32],
        amount: Nat::from(5_u8),
    };
    let bytes = Encode!(&outcome).unwrap();
    assert_eq!(Decode!(&bytes, PoolArrivalOutcome).unwrap(), outcome);
}

#[test]
fn every_refusal_is_an_interface_error() {
    let cap = TokenAmount::from(1_000_u32);
    assert_eq!(
        PoolError::from(RegisterRefusal::AboveSwapCap { cap }),
        PoolError::AboveSwapCap {
            cap: Nat::from(1_000_u32)
        }
    );
    assert_eq!(
        PoolError::from(RegisterRefusal::PocketShort),
        PoolError::Closed("the ICP pocket is short".into())
    );
    assert_eq!(PoolError::from(RegisterRefusal::BadRef), PoolError::BadRef);
    assert_eq!(PoolError::from(RegisterRefusal::Known), PoolError::Known);
}
