use super::*;
use crate::storage::events::{append_event_at, verify_replay};
use crate::storage::on_fresh_memory;
use types::events::TxPurpose;
use types::flow::{finish_key, FinishAction};
use types::{
    ChainId, EventType, EvmAddress, FlowStep, GasAmount, Nonce, Timestamp, TxHash, Wei, WeiPerGas,
};

fn created(key: QuoteHash, nonce: u64) -> EventType {
    EventType::TxCreated {
        purpose: TxPurpose::Finish(key),
        chain_id: ChainId::ARBITRUM,
        nonce: Nonce::new(nonce),
        to: EvmAddress::new([0x81; 20]),
        value: Wei::ZERO,
        data: vec![],
        gas_limit: GasAmount::from(200_000_u32),
        max_fee: WeiPerGas::from(20_000_000_u64),
        max_priority_fee: WeiPerGas::from(1_000_000_u64),
    }
}

/// Every line that records signed bytes is in the page, in log order after the cursor,
/// and a replay of the log rebuilds the index.
#[test]
fn signed_txs_pages_every_signed_line_after_the_cursor() {
    on_fresh_memory(|| {
        crate::storage::init();
        let at = Timestamp::from_nanos(7);
        let message_hash = [3; 32];
        let key = finish_key(message_hash, FinishAction::Receive);
        append_event_at(created(key, 0), at).unwrap();
        let finish = append_event_at(
            EventType::Flow {
                quote_hash: key,
                step: FlowStep::CctpFinishSigned {
                    chain_id: ChainId::ARBITRUM,
                    nonce: Nonce::ZERO,
                    tx_hash: TxHash::new([1; 32]),
                    raw_tx: vec![0x02, 0x01],
                    action: FinishAction::Receive,
                    message_hash,
                    source_tx: None,
                },
            },
            at,
        )
        .unwrap()
        .get();
        let replaced = append_event_at(
            EventType::TxReplaced {
                purpose: TxPurpose::Finish(key),
                chain_id: ChainId::ARBITRUM,
                nonce: Nonce::ZERO,
                max_fee: WeiPerGas::from(40_000_000_u64),
                max_priority_fee: WeiPerGas::from(2_000_000_u64),
                tx_hash: TxHash::new([2; 32]),
                raw_tx: vec![0x02, 0x02],
            },
            at,
        )
        .unwrap()
        .get();

        let page = signed_txs(0, 10);
        assert_eq!(
            page.iter().map(|tx| tx.id).collect::<Vec<_>>(),
            vec![finish, replaced]
        );
        assert_eq!(page[0].chain, ChainRef::Evm(42161));
        assert_eq!(page[0].raw, vec![0x02, 0x01]);
        assert_eq!(page[0].hash, vec![1; 32]);
        assert_eq!(page[0].signed_at_ns, 7);
        assert_eq!(signed_txs(finish, 10)[0].id, replaced);
        assert_eq!(signed_txs(0, 1).len(), 1);
        assert!(signed_txs(replaced, 10).is_empty());
        assert!(verify_replay(), "the index is the log's fold");
    });
}
