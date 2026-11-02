use super::*;
use crate::types::sweep::{SweepAsset, SweepTarget};
use types::{EventIndex, Timestamp};

const INTERFACE: &str = include_str!("../../../../golden/interface.json");

/// One wire sample of every variant the canister writes.
fn samples() -> Vec<EventType> {
    vec![
        EventType::ConfigChanged { json: "{}".into() },
        EventType::RolesChanged {
            quoter: "aaaaa-aa".into(),
            watcher: "2vxsx-fae".into(),
        },
        EventType::TxCreated {
            purpose: TxPurpose::PoolPayout(19),
            chain_id: 8453,
            nonce: 7,
            // the wire address is checksummed, and it crosses back as written
            to: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
            value_wei: Nat::from(0_u8),
            data: vec![0xa9, 0x05, 0x9c, 0xbb],
            gas_limit: Nat::from(120_000_u32),
            max_fee_wei_per_gas: Nat::from(2_000_000_000_u64),
            max_priority_fee_wei_per_gas: Nat::from(100_000_000_u64),
            action: Some("pool_payout".into()),
        },
        EventType::TxReplaced {
            purpose: TxPurpose::Cancel(42161),
            chain_id: 42161,
            nonce: 8,
            max_fee_wei_per_gas: Nat::from(4_000_000_000_u64),
            max_priority_fee_wei_per_gas: Nat::from(200_000_000_u64),
            tx_hash: [20; 32],
            raw_tx: vec![0x02, 0xf8, 0x6b],
        },
        EventType::TxCancelled {
            chain_id: 137,
            nonce: 9,
            tx_hash: [21; 32],
            raw_tx: vec![0x02, 0xf8, 0x6c],
        },
        EventType::TxFinal {
            purpose: TxPurpose::Sweep(3),
            chain_id: 8453,
            nonce: 10,
            tx_hash: [22; 32],
            outcome: TxOutcome::Reverted { block: 23 },
        },
        {
            let step = types::FlowStep::Pulled {
                amount: types::TokenAmount::from(25_u8),
                block_index: 26,
            };
            EventType::Flow {
                quote_hash: [24; 32],
                step: step.name().to_string(),
                detail: format!("{step:?}"),
                step_bytes: step.to_bytes(),
            }
        },
        {
            let step = types::books::BookStep::FeeEarned {
                tx_hash: types::TxHash::new([27; 32]),
                amount: types::TokenAmount::from(28_u8),
                block: None,
            };
            EventType::Books {
                chain_id: 8453,
                step: step.name().to_string(),
                detail: format!("{step:?}"),
                step_bytes: step.to_bytes(),
            }
        },
        {
            let step = types::pool::PoolStep::Expired {
                r#ref: types::hash::PoolRef::new([29; 32]),
            };
            EventType::Pool {
                step: step.name().to_string(),
                detail: format!("{step:?}"),
                step_bytes: step.to_bytes(),
            }
        },
        EventType::Swept {
            id: 4,
            asset: SweepAsset::CkUsdcFees,
            to: SweepTarget::Icp(IcpAccount {
                owner: candid::Principal::anonymous(),
                subaccount: Some(vec![5; 32]),
            }),
            amount: Nat::from(6_000_000_u32),
            from_pool_fees: Nat::from(0_u8),
            from_fee_income: Nat::from(0_u8),
        },
        EventType::SweepReverted {
            id: 4,
            why: "the ledger refused it".into(),
        },
    ]
}

#[test]
fn every_variant_survives_the_wire_both_ways() {
    for wire in samples() {
        let domain = types::EventType::try_from(wire.clone()).unwrap();
        assert_eq!(EventType::from(domain), wire);
    }
}

/// A pool line whose bytes are no pool step is refused at the edge, naming the field.
#[test]
fn a_pool_line_that_is_no_pool_step_is_refused() {
    let refused = types::EventType::try_from(EventType::Pool {
        step: "Expired".into(),
        detail: String::new(),
        step_bytes: vec![0xff],
    });
    assert_eq!(
        refused,
        Err(types::events::EventError::NotAPoolStep {
            field: "step_bytes"
        })
    );
}

/// An address that is not an address at all is refused at the edge, naming the field and
/// which of the four rules the text broke.
#[test]
fn a_transaction_to_something_that_is_not_an_address_is_refused() {
    let to_address = |to: &str| {
        types::EventType::try_from(EventType::TxCreated {
            purpose: TxPurpose::PoolApprove,
            chain_id: 8453,
            nonce: 7,
            to: to.into(),
            value_wei: Nat::from(0_u8),
            data: vec![],
            gas_limit: Nat::from(1_u8),
            max_fee_wei_per_gas: Nat::from(1_u8),
            max_priority_fee_wei_per_gas: Nat::from(1_u8),
            action: None,
        })
    };
    use types::evm::EvmAddressError as Reason;
    for (to, reason) in [
        ("not an address", Reason::NoPrefix),
        ("0xabc", Reason::WrongLength { len: 3 }),
        ("0xzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz", Reason::NotHex),
        (
            "0x833589fcD6EDb6E08f4c7C32D4f71b54bdA02913",
            Reason::BadChecksum,
        ),
    ] {
        assert_eq!(
            to_address(to),
            Err(types::events::EventError::NotAnAddress {
                field: "to",
                reason,
            }),
            "{to}"
        );
    }
}

/// A sweep to an account whose subaccount is not 32 bytes is refused at the edge.
#[test]
fn a_sweep_to_a_malformed_account_is_refused() {
    let refused = types::EventType::try_from(EventType::Swept {
        id: 1,
        asset: SweepAsset::Icp,
        to: SweepTarget::Icp(IcpAccount {
            owner: candid::Principal::anonymous(),
            subaccount: Some(vec![1; 31]),
        }),
        amount: Nat::from(1_u8),
        from_pool_fees: Nat::from(0_u8),
        from_fee_income: Nat::from(0_u8),
    });
    assert_eq!(
        refused,
        Err(types::events::EventError::NotAnAccount { field: "to" })
    );
}

/// An event reads back as its index, its time and its payload: no hash, no parent.
#[test]
fn an_event_reads_back_with_its_index_and_time() {
    let payload = types::EventType::RolesChanged {
        quoter: "aaaaa-aa".into(),
        watcher: "2vxsx-fae".into(),
    };
    let sealed = types::Event {
        index: EventIndex::new(4),
        timestamp: Timestamp::from_nanos(1_700_000_000),
        payload: payload,
    };
    let wire = Event::from(sealed);
    assert_eq!(wire.index, 4);
    assert_eq!(wire.time_ns, 1_700_000_000);
    assert_eq!(
        wire.payload,
        EventType::RolesChanged {
            quoter: "aaaaa-aa".into(),
            watcher: "2vxsx-fae".into(),
        }
    );
}

/// The interface golden page of events, as `events_page` answers it: a pool line that
/// books an ICP stake's move fee, a USDC fee sweep on Base, and that sweep's transaction
/// reaching its end. The watcher decodes the same bytes.
fn golden_page() -> Vec<Event> {
    let staked = types::pool::PoolStep::StakedIcp {
        id: types::hash::PoolOpId::new([0x51; 32]),
        owner: types::pool::PoolAccount::Icp(types::pool::IcpAccount {
            owner: candid::Principal::anonymous(),
            subaccount: None,
        }),
        amount: types::TokenAmount::from(250_000_000_u64),
        move_fee: types::TokenAmount::from(1_400_000_u64),
        shares: types::pool::Shares::new(248_600_000_000_000_000_000),
        block: 4_200_000,
    };
    vec![
        Event {
            index: 41,
            time_ns: 1_900_000_000_000_000_000,
            payload: EventType::Pool {
                step: staked.name().to_string(),
                detail: format!("{staked:?}"),
                step_bytes: staked.to_bytes(),
            },
        },
        Event {
            index: 42,
            time_ns: 1_900_000_001_000_000_000,
            payload: EventType::Swept {
                id: 0,
                asset: SweepAsset::FeeUsdc,
                to: SweepTarget::Evm {
                    chain_id: 8453,
                    to: "0x3E5D1A9b7c2f4E6A8b0c1D3E5F7a9B0c2D4e6f80".into(),
                },
                amount: Nat::from(1_000_000_u32),
                from_pool_fees: Nat::from(400_000_u32),
                from_fee_income: Nat::from(600_000_u32),
            },
        },
        Event {
            index: 43,
            time_ns: 1_900_000_060_000_000_000,
            payload: EventType::TxFinal {
                purpose: TxPurpose::Sweep(0),
                chain_id: 8453,
                nonce: 17,
                tx_hash: [0x7f; 32],
                outcome: TxOutcome::Succeeded { block: 30_000_000 },
            },
        },
    ]
}

#[test]
fn the_interface_golden_events_hold() {
    let golden: serde_json::Value = serde_json::from_str(INTERFACE).unwrap();
    let golden = &golden["events"];
    let hex_of = |field: &str| {
        hex::decode(golden[field].as_str().unwrap().trim_start_matches("0x")).unwrap()
    };
    let page = golden_page();
    let EventType::Pool { step_bytes, .. } = &page[0].payload else {
        panic!("the first golden line is a pool line");
    };
    assert_eq!(*step_bytes, hex_of("staked_icp_step_bytes"));
    assert_eq!(candid::encode_one(&page).unwrap(), hex_of("page_candid"));
    let decoded: Vec<Event> = candid::decode_one(&hex_of("page_candid")).unwrap();
    assert_eq!(decoded, page);
}
