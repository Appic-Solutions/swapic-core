use super::*;
use serde_json::Value;

const GOLDEN: &str = include_str!("../../../../golden/interface.json");

fn golden() -> Value {
    serde_json::from_str(GOLDEN).expect("the golden file is JSON")
}

fn hex_of(value: &Value) -> Vec<u8> {
    hex::decode(value.as_str().unwrap().trim_start_matches("0x")).unwrap()
}

fn amount(value: &Value) -> TokenAmount {
    TokenAmount::new(value.as_u64().unwrap().into())
}

/// The terms a golden memo entry names, read field by field.
fn terms_of(entry: &Value) -> DepositTerms {
    let terms = &entry["terms"];
    let salt: [u8; 32] = hex_of(&terms["salt"]).try_into().unwrap();
    match terms["kind"].as_str().unwrap() {
        "stake" => DepositTerms {
            kind: DepositKind::Stake {
                owner: PoolAccount::Evm(terms["owner"]["evm"].as_str().unwrap().parse().unwrap()),
            },
            min_usdc: amount(&terms["min_usdc"]),
            premium: TokenAmount::ZERO,
            icp_fees: TokenAmount::ZERO,
            move_fee: amount(&terms["move_fee"]),
            salt,
        },
        "swap" => DepositTerms {
            kind: DepositKind::Swap {
                recipient: IcpAccount {
                    owner: Principal::from_text(terms["recipient"]["owner"].as_str().unwrap())
                        .unwrap(),
                    subaccount: Some(
                        hex_of(&terms["recipient"]["subaccount"])
                            .try_into()
                            .unwrap(),
                    ),
                },
                token_out: Principal::from_text(terms["token_out"].as_str().unwrap()).unwrap(),
                min_out: terms["min_out"].as_u64().unwrap().into(),
            },
            min_usdc: amount(&terms["min_usdc"]),
            premium: amount(&terms["premium"]),
            icp_fees: amount(&terms["icp_fees"]),
            move_fee: TokenAmount::ZERO,
            salt,
        },
        kind => panic!("no golden kind {kind}"),
    }
}

/// Every golden memo is exactly the bytes its terms encode to, decodes back to its terms,
/// and hashes to its ref: the vectors the quoter, the watcher and the app are held to.
#[test]
fn the_golden_memos_encode_decode_and_hash_to_their_refs() {
    let golden = golden();
    let memos = golden["memos"].as_array().unwrap();
    assert_eq!(memos.len(), 2);
    for entry in memos {
        let terms = terms_of(entry);
        let bytes = hex_of(&entry["bytes"]);
        assert_eq!(encode(&terms).unwrap(), bytes, "{}", entry["about"]);
        assert_eq!(decode(&bytes).unwrap(), terms, "{}", entry["about"]);
        assert_eq!(
            memo_ref(&bytes).into_bytes().to_vec(),
            hex_of(&entry["ref"]),
            "{}",
            entry["about"]
        );
        assert_eq!(terms.pool_ref(), Some(memo_ref(&bytes)));
    }
}

/// The order in the golden file carries the stake's memo, and its ref is that memo's.
#[test]
fn the_golden_order_carries_the_stake_memo_under_its_ref() {
    let golden = golden();
    let stake = &golden["memos"][0];
    assert_eq!(golden["order"]["value"]["memo"], stake["bytes"]);
    assert_eq!(golden["order"]["value"]["ref"], stake["ref"]);
}

fn stake_terms() -> DepositTerms {
    terms_of(&golden()["memos"][0])
}

fn swap_terms() -> DepositTerms {
    terms_of(&golden()["memos"][1])
}

/// Every committed field moves the ref: no two terms share a memo.
#[test]
fn every_field_moves_the_ref() {
    let base = swap_terms();
    let changed = [
        DepositTerms {
            min_usdc: TokenAmount::new(1),
            ..base.clone()
        },
        DepositTerms {
            premium: TokenAmount::new(1),
            ..base.clone()
        },
        DepositTerms {
            icp_fees: TokenAmount::new(1),
            ..base.clone()
        },
        DepositTerms {
            salt: [0; 32],
            ..base.clone()
        },
    ];
    for terms in changed {
        assert_ne!(terms.pool_ref(), base.pool_ref());
    }
    let stake = stake_terms();
    assert_ne!(
        DepositTerms {
            move_fee: TokenAmount::new(1),
            ..stake.clone()
        }
        .pool_ref(),
        stake.pool_ref()
    );
}

/// A fee the kind has no place for has no memo, so terms carrying one are no one's terms.
#[test]
fn a_fee_the_kind_has_no_place_for_has_no_memo() {
    let swap = DepositTerms {
        move_fee: TokenAmount::new(1),
        ..swap_terms()
    };
    assert_eq!(encode(&swap), Err(MemoError::NotCanonical));
    assert_eq!(swap.pool_ref(), None);
    let stake = DepositTerms {
        premium: TokenAmount::new(1),
        ..stake_terms()
    };
    assert_eq!(encode(&stake), Err(MemoError::NotCanonical));
}

/// An ICP owner and the default subaccount: the zero subaccount reads back as none, the
/// same ICRC-1 account, and the memo re-encodes to itself.
#[test]
fn an_icp_stake_round_trips_and_the_zero_subaccount_is_none() {
    let terms = DepositTerms {
        kind: DepositKind::Stake {
            owner: PoolAccount::Icp(IcpAccount {
                owner: Principal::from_slice(&[7; 29]),
                subaccount: None,
            }),
        },
        ..stake_terms()
    };
    let bytes = encode(&terms).unwrap();
    assert_eq!(bytes.len(), 2 + 32 + 16 + 1 + 1 + 29 + 32 + 16);
    assert_eq!(decode(&bytes).unwrap(), terms);
    let zero = DepositTerms {
        kind: DepositKind::Stake {
            owner: PoolAccount::Icp(IcpAccount {
                owner: Principal::from_slice(&[7; 29]),
                subaccount: Some([0; 32]),
            }),
        },
        ..stake_terms()
    };
    assert_eq!(encode(&zero).unwrap(), bytes);
}

/// The longest terms fit the bound with room to spare.
#[test]
fn the_longest_terms_fit_the_bound() {
    let terms = DepositTerms {
        kind: DepositKind::Swap {
            recipient: IcpAccount {
                owner: Principal::from_slice(&[1; 29]),
                subaccount: Some([2; 32]),
            },
            token_out: Principal::from_slice(&[3; 29]),
            min_out: u128::MAX,
        },
        min_usdc: TokenAmount::new(u128::MAX),
        premium: TokenAmount::new(u128::MAX),
        icp_fees: TokenAmount::new(u128::MAX),
        move_fee: TokenAmount::ZERO,
        salt: [4; 32],
    };
    let bytes = encode(&terms).unwrap();
    assert_eq!(bytes.len(), 190);
    assert!(bytes.len() <= MAX_MEMO_BYTES);
    assert_eq!(decode(&bytes).unwrap(), terms);
}

/// Bytes that are not exactly one memo are refused, naming why.
#[test]
fn bytes_that_are_not_one_memo_are_refused() {
    let good = encode(&stake_terms()).unwrap();
    let mut trailing = good.clone();
    trailing.push(0);
    assert_eq!(
        decode(&trailing),
        Err(MemoError::TrailingBytes {
            len: good.len() + 1,
            consumed: good.len()
        })
    );
    assert_eq!(
        decode(&good[..good.len() - 1]),
        Err(MemoError::Truncated { field: "move_fee" })
    );
    assert_eq!(decode(&[]), Err(MemoError::Truncated { field: "version" }));
    let mut version = good.clone();
    version[0] = 2;
    assert_eq!(decode(&version), Err(MemoError::Version(2)));
    let mut kind = good.clone();
    kind[1] = 2;
    assert_eq!(decode(&kind), Err(MemoError::Kind(2)));
    let mut owner = good.clone();
    owner[2 + 32 + 16] = 3;
    assert_eq!(decode(&owner), Err(MemoError::OwnerTag(3)));
    assert_eq!(
        decode(&vec![1; MAX_MEMO_BYTES + 1]),
        Err(MemoError::TooLong {
            len: MAX_MEMO_BYTES + 1
        })
    );
    let swap = encode(&swap_terms()).unwrap();
    let mut empty_principal = swap.clone();
    empty_principal[2 + 32 + 16] = 0;
    assert_eq!(
        decode(&empty_principal),
        Err(MemoError::PrincipalLength {
            field: "recipient",
            len: 0
        })
    );
    let mut long_principal = swap;
    long_principal[2 + 32 + 16] = 30;
    assert_eq!(
        decode(&long_principal),
        Err(MemoError::PrincipalLength {
            field: "recipient",
            len: 30
        })
    );
}
