use super::*;
use types::address::MAX_TEXT_BYTES;
use types::{EventHash, EventIndex, Timestamp};

/// One wire sample of every variant.
fn samples() -> Vec<EventType> {
    vec![
        EventType::ConfigChanged { json: "{}".into() },
        EventType::FundsReceived {
            quote_hash: [1; 32],
            quote_bytes: vec![0xde, 0xad],
            chain_id: 8453,
            token: "USDC".into(),
            amount: Nat::from(u128::MAX),
            tx_ref: "0xfeed".into(),
        },
        EventType::TxSigned {
            quote_hash: [2; 32],
            attempt: 1,
            chain_id: 42161,
            tx_hash: [3; 32],
            raw_tx: vec![0x02],
        },
        EventType::TxConfirmed {
            quote_hash: [4; 32],
            attempt: 2,
            chain_id: 1,
            tx_hash: [5; 32],
            block: 19_000_000,
        },
        EventType::TxFailed {
            quote_hash: [6; 32],
            attempt: 3,
            reason: "reverted".into(),
        },
        EventType::PaidInStable {
            quote_hash: [7; 32],
            chain_id: 8453,
            amount: Nat::from(999_999_u32),
        },
        EventType::DecisionRequired {
            quote_hash: [8; 32],
            reason: "slippage".into(),
        },
        EventType::DecisionMade {
            quote_hash: [9; 32],
            choice: Choice::Refund,
        },
        EventType::RefundStarted {
            quote_hash: [10; 32],
            reason: "timeout".into(),
        },
        EventType::Refunded {
            quote_hash: [11; 32],
            chain_id: 137,
            token: "USDT".into(),
            amount: Nat::from(42_u8),
            to: "0xUser".into(),
        },
        EventType::SwapDone {
            quote_hash: [12; 32],
        },
        EventType::Frozen {
            quote_hash: [13; 32],
            reason: "griefed".into(),
        },
        EventType::FeeAccrued {
            quote_hash: [14; 32],
            amount: Nat::from(7_u8),
        },
        EventType::PocketFunded {
            chain_id: 10,
            amount: Nat::from(1u128 << 70),
        },
        EventType::PocketReserved {
            quote_hash: [15; 32],
            chain_id: 8453,
            amount: Nat::from(3_u8),
        },
        EventType::PocketRebalanced {
            from_chain: 8453,
            to_chain: 42161,
            amount: Nat::from(250_u8),
            route: "cctp".into(),
        },
        EventType::PocketReleased {
            quote_hash: [16; 32],
            chain_id: 8453,
            amount: Nat::from(150_u8),
        },
        EventType::PocketSpent {
            quote_hash: [17; 32],
            chain_id: 42161,
            amount: Nat::from(250_u8),
        },
        EventType::RolesChanged {
            quoter: "aaaaa-aa".into(),
            watcher: "2vxsx-fae".into(),
        },
        EventType::WaitingRepaired {
            quote_hash: [18; 32],
        },
        EventType::TxCreated {
            purpose: TxPurpose::Burn([19; 32]),
            chain_id: 8453,
            nonce: 7,
            // the wire address is checksummed, and it crosses back as written
            to: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
            value_wei: Nat::from(0_u8),
            data: vec![0xa9, 0x05, 0x9c, 0xbb],
            gas_limit: Nat::from(120_000_u32),
            max_fee_wei_per_gas: Nat::from(2_000_000_000_u64),
            max_priority_fee_wei_per_gas: Nat::from(100_000_000_u64),
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
    ]
}

/// An address that is not an address at all is refused at the edge, naming the field AND
/// which of the four rules the text broke. A caller replaying a log with a tampered address
/// is otherwise told the text is too long, which is false for three of the four.
#[test]
fn a_transaction_to_something_that_is_not_an_address_is_refused() {
    let to_address = |to: &str| {
        types::EventType::try_from(EventType::TxCreated {
            purpose: TxPurpose::Burn([19; 32]),
            chain_id: 8453,
            nonce: 7,
            to: to.into(),
            value_wei: Nat::from(0_u8),
            data: vec![],
            gas_limit: Nat::from(1_u8),
            max_fee_wei_per_gas: Nat::from(1_u8),
            max_priority_fee_wei_per_gas: Nat::from(1_u8),
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

/// Every amount field of a transaction answers for itself: "amount is above u128::MAX" is
/// not an answer about a gas limit or a fee, and the field is exactly what the error type
/// exists to name.
#[test]
fn every_amount_field_of_a_transaction_names_itself() {
    // above the 256 bits the domain amount holds, so the edge refuses it by name
    let huge = Nat::from(u128::MAX) * Nat::from(u128::MAX) * Nat::from(4_u8);
    let created = |value_wei: Nat, gas_limit: Nat, max_fee: Nat, tip: Nat| EventType::TxCreated {
        purpose: TxPurpose::Burn([19; 32]),
        chain_id: 8453,
        nonce: 7,
        to: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
        value_wei,
        data: vec![],
        gas_limit,
        max_fee_wei_per_gas: max_fee,
        max_priority_fee_wei_per_gas: tip,
    };
    let one = || Nat::from(1_u8);
    assert!(types::EventType::try_from(created(one(), one(), one(), one())).is_ok());
    for (field, wire) in [
        ("value_wei", created(huge.clone(), one(), one(), one())),
        ("gas_limit", created(one(), huge.clone(), one(), one())),
        (
            "max_fee_wei_per_gas",
            created(one(), one(), huge.clone(), one()),
        ),
        (
            "max_priority_fee_wei_per_gas",
            created(one(), one(), one(), huge.clone()),
        ),
    ] {
        assert_eq!(
            types::EventType::try_from(wire),
            Err(types::events::EventError::AmountTooLarge { field }),
            "{field}"
        );
    }

    let replaced = |max_fee: Nat, tip: Nat| EventType::TxReplaced {
        purpose: TxPurpose::Cancel(42161),
        chain_id: 42161,
        nonce: 8,
        max_fee_wei_per_gas: max_fee,
        max_priority_fee_wei_per_gas: tip,
        tx_hash: [20; 32],
        raw_tx: vec![],
    };
    for (field, wire) in [
        ("max_fee_wei_per_gas", replaced(huge.clone(), one())),
        (
            "max_priority_fee_wei_per_gas",
            replaced(one(), huge.clone()),
        ),
    ] {
        assert_eq!(
            types::EventType::try_from(wire),
            Err(types::events::EventError::AmountTooLarge { field }),
            "{field}"
        );
    }
}

#[test]
fn every_variant_survives_the_wire_both_ways() {
    let samples = samples();
    assert_eq!(samples.len(), types::events::EVENT_VARIANT_COUNT);
    for wire in samples {
        let domain = types::EventType::try_from(wire.clone()).unwrap();
        assert_eq!(EventType::from(domain), wire);
    }
}

#[test]
fn an_event_reads_back_with_its_place_in_the_chain() {
    let payload = types::EventType::SwapDone {
        quote_hash: QuoteHash::new([1; 32]),
    };
    let sealed = types::Event::seal(
        EventIndex::new(4),
        Timestamp::from_nanos(1_700_000_000),
        EventHash::new([9; 32]),
        payload,
    )
    .unwrap();
    let wire = Event::from(sealed.clone());
    assert_eq!(wire.index, 4);
    assert_eq!(wire.time_ns, 1_700_000_000);
    assert_eq!(wire.parent_hash, [9; 32]);
    assert_eq!(wire.hash, sealed.hash.into_bytes());
    assert_eq!(
        wire.payload,
        EventType::SwapDone {
            quote_hash: [1; 32]
        }
    );
}

#[test]
fn an_amount_above_u128_max_is_refused() {
    let wire = EventType::PocketFunded {
        chain_id: 8453,
        amount: Nat::from(u128::MAX) + Nat::from(1_u8),
    };
    assert_eq!(
        types::EventType::try_from(wire),
        Err(DomainEventError::AmountTooLarge { field: "amount" })
    );
}

#[test]
fn text_over_the_cap_is_refused_naming_the_field() {
    let refunded = |token: String, to: String| EventType::Refunded {
        quote_hash: [1; 32],
        chain_id: 137,
        token,
        amount: Nat::from(1_u8),
        to,
    };
    let long = "a".repeat(MAX_TEXT_BYTES + 1);
    assert_eq!(
        types::EventType::try_from(refunded(long.clone(), "0xuser".into())),
        Err(DomainEventError::TextTooLong {
            field: "token",
            len: MAX_TEXT_BYTES + 1
        })
    );
    assert_eq!(
        types::EventType::try_from(refunded("USDT".into(), long)),
        Err(DomainEventError::TextTooLong {
            field: "to",
            len: MAX_TEXT_BYTES + 1
        })
    );
}
