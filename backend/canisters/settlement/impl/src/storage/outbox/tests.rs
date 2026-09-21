use super::*;
use crate::storage::on_fresh_memory;
use types::events::TxPurpose;
use types::{Attempt, QuoteHash, Timestamp, TxHash, WeiPerGas};

fn entry(chain_id: ChainId, nonce: u64, status: OutboxStatus) -> OutboxEntry {
    OutboxEntry {
        purpose: TxPurpose::Burn(QuoteHash::new([1; 32])),
        chain_id,
        nonce: Nonce::new(nonce),
        attempt: Some(Attempt::FIRST),
        hashes: vec![TxHash::new([nonce as u8; 32])],
        raw_tx: vec![0x02, 0xf8, 0x6b],
        max_fee: WeiPerGas::from(2_000_000_000_u64),
        max_priority_fee: WeiPerGas::from(100_000_000_u64),
        status,
        created_at: Timestamp::from_nanos(1_000),
        last_sent_at: None,
        to: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
            .parse()
            .unwrap(),
        value: types::Wei::ZERO,
        data: vec![],
        gas_limit: types::GasAmount::from(21_000_u32),
    }
}

/// The outbox is read a chain at a time, oldest nonce first, and a bounded pass reads only
/// the batch it works on.
#[test]
fn entries_are_read_by_chain_and_status_oldest_nonce_first() {
    on_fresh_memory(|| {
        init();
        assert!(is_empty());
        assert_eq!(chains(), vec![]);

        put(entry(ChainId::BASE, 1, OutboxStatus::Queued));
        put(entry(ChainId::BASE, 0, OutboxStatus::Sent));
        put(entry(ChainId::BASE, 2, OutboxStatus::Queued));
        put(entry(ChainId::ARBITRUM, 5, OutboxStatus::Queued));

        assert!(!is_empty());
        assert_eq!(chains(), vec![ChainId::BASE, ChainId::ARBITRUM]);
        assert_eq!(
            nonces(by_status(ChainId::BASE, OutboxStatus::Queued, 10)),
            vec![1, 2]
        );
        assert_eq!(
            nonces(by_status(ChainId::BASE, OutboxStatus::Sent, 10)),
            vec![0]
        );
        assert_eq!(
            nonces(by_status(ChainId::BASE, OutboxStatus::Queued, 1)),
            vec![1],
            "the cap is the cap"
        );
        assert_eq!(
            nonces(by_status(ChainId::ARBITRUM, OutboxStatus::Queued, 10)),
            vec![5],
            "one chain's entries are not another's"
        );
    });
}

/// A replacement takes the slot of what it replaces, so one nonce can never hold two
/// entries, and closing an attempt empties the slot.
#[test]
fn one_nonce_holds_one_entry_and_closing_it_empties_the_slot() {
    on_fresh_memory(|| {
        init();
        let first = entry(ChainId::BASE, 7, OutboxStatus::Sent);
        put(first.clone());
        let replacement = first.replaced(
            TxHash::new([9; 32]),
            vec![0x02, 0xff],
            WeiPerGas::from(4_000_000_000_u64),
            WeiPerGas::from(200_000_000_u64),
        );
        put(replacement.clone());
        assert_eq!(
            by_status(ChainId::BASE, OutboxStatus::Queued, 10),
            vec![replacement.clone()]
        );
        assert_eq!(by_status(ChainId::BASE, OutboxStatus::Sent, 10), vec![]);
        assert_eq!(get(first.key()), Some(replacement));

        remove(first.key());
        assert_eq!(get(first.key()), None);
        assert!(is_empty());
    });
}

fn nonces(entries: Vec<OutboxEntry>) -> Vec<u64> {
    entries.iter().map(|entry| entry.nonce.get()).collect()
}
