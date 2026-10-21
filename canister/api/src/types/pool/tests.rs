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

fn terms() -> PoolTerms {
    let kind = types::pool::DepositKind::Swap {
        recipient: types::pool::IcpAccount {
            owner: Principal::anonymous(),
            subaccount: None,
        },
        token_out: ckusdc(),
        min_out: 1_000_000,
    };
    PoolTerms {
        kind: kind.into(),
        premium: Nat::from(10_000_u64),
        icp_fees: Nat::from(10_000_u64),
        rebalance_share: Nat::from(0_u8),
        salt: vec![0x11; 32],
    }
}

/// The interface's new vector V1 (P11d) through the wire: the ref the canister recomputes
/// from the terms handed with the arrival, the ICP fees committed.
#[test]
fn terms_read_into_their_ref() {
    let domain = terms().into_domain().unwrap();
    assert_eq!(
        domain.pool_ref().unwrap().to_string(),
        "2ca558b613087d4829efcfc433da96c2803012b2840e9775d83f94e880a03a35"
    );
    assert!(domain.is_canonical());
    // the arrival's registration carries every term, and the view gives them back
    let registration = domain.registration(
        TokenAmount::from(1_100_000_u64),
        types::ChainId::ARBITRUM,
        None,
        types::pool::PoolRail::Cctp,
        types::Timestamp::from_nanos(1_800_000_000_000_000_000),
    );
    assert_eq!(registration.pool_ref(), domain.pool_ref());
    assert_eq!(registration.src_payer, EvmAddress::ZERO);
    let r#ref = domain.pool_ref().unwrap();
    let view = PoolDepositView::of(r#ref, types::pool::Registered::new(registration));
    assert_eq!(view.kind, terms().kind);
    assert_eq!(view.min_usdc, Nat::from(1_100_000_u64));
    assert_eq!(view.status, PoolDepositStatus::Registered);
    // a swap carrying a rebalancing share its ref does not commit is no one's terms
    let both = PoolTerms {
        rebalance_share: Nat::from(1_u8),
        ..terms()
    };
    assert!(!both.into_domain().unwrap().is_canonical());
    // a short salt, an amount past 128 bits
    let short_salt = PoolTerms {
        salt: vec![0; 33],
        ..terms()
    };
    assert!(short_salt.into_domain().is_err());
    let wide = PoolTerms {
        icp_fees: Nat::from(u128::MAX) + Nat::from(1_u8),
        ..terms()
    };
    assert_eq!(
        wide.into_domain(),
        Err(PoolWireError::TooLarge { field: "icp_fees" })
    );
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

#[test]
fn every_wire_type_survives_candid() {
    let arrival = PoolArrival {
        base_tx: vec![0xab; 32],
        src_chain: 42_161,
        src_tx: vec![0xcd; 32],
        terms: terms(),
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
