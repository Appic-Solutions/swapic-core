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
    ]
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
