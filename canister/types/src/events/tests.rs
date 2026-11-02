use super::*;
use ic_stable_structures::Storable;

fn quote(byte: u8) -> QuoteHash {
    QuoteHash::new([byte; 32])
}

/// One sample of every variant the canister writes.
pub(crate) fn samples() -> Vec<EventType> {
    vec![
        EventType::ConfigChanged {
            json: "{\"batch_window_ms\":2000}".into(),
        },
        EventType::RolesChanged {
            quoter: "aaaaa-aa".into(),
            watcher: "2vxsx-fae".into(),
        },
        EventType::TxCreated {
            purpose: TxPurpose::PoolPayout(3),
            chain_id: ChainId::BASE,
            nonce: Nonce::new(4),
            to: EvmAddress::new([5; 20]),
            value: Wei::ZERO,
            data: vec![0xa9, 0x05, 0x9c, 0xbb],
            gas_limit: GasAmount::from(120_000_u32),
            max_fee: WeiPerGas::from(2_000_000_000_u64),
            max_priority_fee: WeiPerGas::from(100_000_000_u64),
            action: Some(crate::config::GasAction::PoolPayout),
        },
        EventType::TxReplaced {
            purpose: TxPurpose::Relay(quote(6)),
            chain_id: ChainId::ARBITRUM,
            nonce: Nonce::new(7),
            max_fee: WeiPerGas::from(4_000_000_000_u64),
            max_priority_fee: WeiPerGas::from(200_000_000_u64),
            tx_hash: TxHash::new([8; 32]),
            raw_tx: vec![0x02, 0xf8],
        },
        EventType::TxCancelled {
            chain_id: ChainId::POLYGON,
            nonce: Nonce::new(9),
            tx_hash: TxHash::new([10; 32]),
            raw_tx: vec![0x02, 0xf9],
        },
        EventType::Flow {
            quote_hash: quote(11),
            step: FlowStep::TxClosed {
                tx_hash: TxHash::new([12; 32]),
                block: BlockNumber::new(13),
                success: true,
            },
        },
        EventType::Books {
            chain_id: ChainId::BASE,
            step: BookStep::FeeEarned {
                tx_hash: TxHash::new([14; 32]),
                amount: crate::numeric::TokenAmount::from(15_u32),
                block: Some(BlockNumber::new(16)),
            },
        },
        EventType::Pool {
            step: PoolStep::Expired {
                r#ref: crate::hash::PoolRef::new([17; 32]),
            },
        },
        EventType::TxFinal {
            purpose: TxPurpose::Sweep(18),
            chain_id: ChainId::BASE,
            nonce: Nonce::new(19),
            tx_hash: TxHash::new([20; 32]),
            outcome: TxOutcome::Succeeded {
                block: BlockNumber::new(21),
            },
        },
        EventType::Swept {
            sweep: crate::sweep::Sweep {
                id: 22,
                asset: crate::sweep::SweepAsset::FeeUsdc,
                to: crate::sweep::SweepTo::Evm {
                    chain: ChainId::BASE,
                    to: EvmAddress::new([23; 20]),
                },
                amount: crate::numeric::TokenAmount::from(24_000_000_u32),
                from_pool_fees: crate::numeric::TokenAmount::from(4_000_000_u32),
                from_fee_income: crate::numeric::TokenAmount::from(20_000_000_u32),
            },
        },
        EventType::SweepReverted {
            id: 22,
            why: "reverted".into(),
        },
    ]
}

/// The stable log decodes an event only when it is read, so a variant that no longer
/// decodes would trap every read of it: every variant crosses the log's own `Storable`.
#[test]
fn every_variant_round_trips_through_storage() {
    for (i, payload) in (0u64..).zip(samples()) {
        let event = Event {
            index: EventIndex::new(i),
            timestamp: Timestamp::from_nanos(1_700_000_000_000_000_000 + i),
            payload,
        };
        assert_eq!(Event::from_bytes(event.to_bytes()), event);
    }
}

/// Only a flow or pool step that moves something arms the engine.
#[test]
fn only_steps_that_move_something_arm_the_engine() {
    for sample in samples() {
        let moves = matches!(sample, EventType::Flow { .. } | EventType::Pool { .. })
            && sample.moves_a_step();
        assert_eq!(sample.moves_a_step(), moves, "{sample:?}");
    }
}

/// Every keyed purpose has its own key, so two transactions of different purposes never
/// hold one key; a cancel holds none.
#[test]
fn every_purpose_has_its_own_key() {
    let keyed = [
        TxPurpose::Relay(quote(1)),
        TxPurpose::Finish(quote(2)),
        TxPurpose::PoolPayout(1),
        TxPurpose::PoolTransfer([1; 32]),
        TxPurpose::PoolApprove,
        TxPurpose::PoolMove(1),
        TxPurpose::EcoRescue([1; 32]),
        TxPurpose::Sweep(1),
    ];
    let keys: std::collections::BTreeSet<_> = keyed
        .iter()
        .map(|purpose| purpose.quote_hash().unwrap())
        .collect();
    assert_eq!(keys.len(), keyed.len());
    assert_eq!(TxPurpose::Cancel(ChainId::BASE).quote_hash(), None);
    assert_ne!(
        TxPurpose::Sweep(1).quote_hash(),
        TxPurpose::PoolMove(1).quote_hash()
    );
}

/// A recorded signed transaction is read back for the watcher's broadcast.
#[test]
fn a_replacement_and_a_cancel_carry_their_bytes() {
    for sample in samples() {
        let carries = matches!(
            sample,
            EventType::TxReplaced { .. } | EventType::TxCancelled { .. }
        );
        assert_eq!(sample.signed_tx().is_some(), carries, "{sample:?}");
    }
}
