use super::*;
use crate::storage::events::{append_event_at, verify_replay};
use crate::storage::on_fresh_memory;
use types::events::TxPurpose;
use types::flow::{finish_key, FinishAction};
use types::QuoteHash;
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
        action: None,
    }
}

/// Every line that records signed bytes is in the page, in log order after the cursor,
/// and a replay of the log rebuilds the index.
#[test]
fn signed_txs_pages_every_signed_line_after_the_cursor() {
    on_fresh_memory(|| {
        crate::storage::init();
        let at = Timestamp::from_nanos(7);
        let arrival = [3; 32];
        let key = finish_key(arrival, FinishAction::Receive);
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
                    arrival,
                    source_tx: None,
                    paid: None,
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

        let page = signed_page(0, 10, true, 0);
        assert_eq!(
            page.iter().map(|tx| tx.id).collect::<Vec<_>>(),
            vec![finish, replaced]
        );
        assert_eq!(page[0].chain, ChainRef::Evm(42161));
        assert_eq!(page[0].raw, vec![0x02, 0x01]);
        assert_eq!(page[0].hash, vec![1; 32]);
        assert_eq!(page[0].signed_at_ns, 7);
        assert_eq!(signed_page(finish, 10, true, 0)[0].id, replaced);
        assert_eq!(signed_page(0, 1, true, 0).len(), 1);
        assert!(signed_page(replaced, 10, true, 0).is_empty());
        assert!(verify_replay(), "the index is the log's fold");
    });
}

/// A reader other than the watcher and the controllers sees a signed transaction only once it
/// is an hour old (audit C5).
#[test]
fn a_public_reader_sees_a_signed_transaction_only_once_it_is_old() {
    on_fresh_memory(|| {
        crate::storage::init();
        let at = Timestamp::from_nanos(7);
        let arrival = [3; 32];
        let key = finish_key(arrival, FinishAction::Receive);
        append_event_at(created(key, 0), at).unwrap();
        append_event_at(
            EventType::Flow {
                quote_hash: key,
                step: FlowStep::CctpFinishSigned {
                    chain_id: ChainId::ARBITRUM,
                    nonce: Nonce::ZERO,
                    tx_hash: TxHash::new([1; 32]),
                    raw_tx: vec![0x02, 0x01],
                    action: FinishAction::Receive,
                    arrival,
                    source_tx: None,
                    paid: None,
                },
            },
            at,
        )
        .unwrap();
        let hour = crate::queries::events::HIDE_TX_BYTES_NS;
        assert!(signed_page(0, 10, false, 7 + hour - 1).is_empty());
        assert_eq!(signed_page(0, 10, false, 7 + hour).len(), 1);
        assert_eq!(signed_page(0, 10, true, 8).len(), 1);
    });
}

/// One line of `data_len` calldata, at `at` nanoseconds.
fn big_line(index: u64, at: u64, data_len: usize) -> types::events::Event {
    let mut payload = created(finish_key([index as u8; 32], FinishAction::Receive), index);
    if let EventType::TxCreated { data, .. } = &mut payload {
        *data = vec![0xab; data_len];
    }
    types::events::Event {
        index: types::EventIndex::new(index),
        timestamp: Timestamp::from_nanos(at),
        payload,
    }
}

/// A page stops before a reply too big to return, always holds a line, and carries on from
/// where it stopped (audit C15).
#[test]
fn a_page_is_cut_by_its_size_and_always_holds_a_line() {
    use crate::queries::events::{page_of, MAX_PAGE_BYTES};
    let lines: Vec<_> = (0..200).map(|i| big_line(i, 7, 22_000)).collect();
    let page = page_of(lines.clone(), true, 8);
    assert!(
        !page.is_empty() && page.len() < lines.len(),
        "{}",
        page.len()
    );
    let bytes: usize = page
        .iter()
        .map(|line| candid::encode_one(line).unwrap().len())
        .sum();
    assert!(bytes <= MAX_PAGE_BYTES, "{bytes}");
    // the whole reply fits what a canister may answer
    assert!(candid::encode_one(&page).unwrap().len() < 3 * 1024 * 1024);
    let rest = page_of(lines[page.len()..].to_vec(), true, 8);
    assert!(!rest.is_empty(), "the reader carries on");
    assert_eq!(
        page_of(vec![big_line(0, 7, 2 * MAX_PAGE_BYTES)], true, 8).len(),
        1
    );
}

/// A public reader sees no transaction bytes of a young line, in any field of it; the watcher
/// and the controllers do, and everyone does an hour on (audit C5).
#[test]
fn a_young_lines_bytes_are_hidden_from_a_public_reader_only() {
    use crate::queries::events::{page_of, HIDE_TX_BYTES_NS};
    use settlement_api::types::events::EventType as Wire;
    let raw = vec![0xcd; 40];
    let signed = types::events::Event {
        index: types::EventIndex::new(0),
        timestamp: Timestamp::from_nanos(7),
        payload: EventType::Flow {
            quote_hash: finish_key([3; 32], FinishAction::Receive),
            step: FlowStep::CctpFinishSigned {
                chain_id: ChainId::ARBITRUM,
                nonce: Nonce::ZERO,
                tx_hash: TxHash::new([1; 32]),
                raw_tx: raw.clone(),
                action: FinishAction::Receive,
                arrival: [3; 32],
                source_tx: None,
                paid: None,
            },
        },
    };
    let holds = |page: &[settlement_api::types::events::Event]| {
        let bytes = candid::encode_one(page).unwrap();
        bytes
            .windows(raw.len())
            .any(|window| window == raw.as_slice())
    };
    assert!(!holds(&page_of(vec![signed.clone()], false, 8)));
    assert!(holds(&page_of(vec![signed.clone()], true, 8)));
    assert!(holds(&page_of(vec![signed], false, 7 + HIDE_TX_BYTES_NS)));
    let page = page_of(vec![big_line(0, 7, 64)], false, 8);
    assert!(matches!(&page[0].payload, Wire::TxCreated { data, .. } if data.is_empty()));
    let page = page_of(vec![big_line(0, 7, 64)], true, 8);
    assert!(matches!(&page[0].payload, Wire::TxCreated { data, .. } if data.len() == 64));
}
