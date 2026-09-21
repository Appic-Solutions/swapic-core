use super::*;
use crate::hash::QuoteHash;
use crate::numeric::{GasAmount, Wei};

fn entry() -> OutboxEntry {
    OutboxEntry {
        purpose: TxPurpose::Burn(QuoteHash::new([1; 32])),
        chain_id: ChainId::BASE,
        nonce: Nonce::new(7),
        attempt: Some(Attempt::FIRST),
        hashes: vec![TxHash::new([2; 32])],
        raw_tx: vec![0x02, 0xf8, 0x6b],
        max_fee: WeiPerGas::from(2_000_000_000_u64),
        max_priority_fee: WeiPerGas::from(100_000_000_u64),
        status: OutboxStatus::Queued,
        created_at: Timestamp::from_nanos(1_700_000_000_000_000_000),
        last_sent_at: None,
        to: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
            .parse()
            .unwrap(),
        value: Wei::ZERO,
        data: vec![0xde, 0xad, 0xbe, 0xef],
        gas_limit: GasAmount::from(120_000_u32),
    }
}

/// The key is the chain and the nonce, in that order and big-endian, so a walk of the
/// outbox meets each chain's transactions in allocation order.
#[test]
fn an_outbox_key_orders_by_chain_then_nonce() {
    let key = |chain: ChainId, nonce: u64| NonceKey {
        chain_id: chain,
        nonce: Nonce::new(nonce),
    };
    let mut keys = vec![
        key(ChainId::ARBITRUM, 1),
        key(ChainId::BASE, 9),
        key(ChainId::BASE, 2),
    ];
    keys.sort_by_key(|key| key.to_bytes().into_owned());
    assert_eq!(
        keys,
        vec![
            key(ChainId::BASE, 2),
            key(ChainId::BASE, 9),
            key(ChainId::ARBITRUM, 1)
        ]
    );
    for key in keys {
        assert_eq!(key.to_bytes().len(), 16);
        assert_eq!(NonceKey::from_bytes(key.to_bytes()), key);
    }
}

/// A replacement keeps the nonce and every hash before it: the transaction it replaces is
/// still on the network and may be the one that lands.
#[test]
fn a_replacement_keeps_the_nonce_and_remembers_the_hash_it_replaced() {
    let mut sent = entry();
    sent.sent(Timestamp::from_nanos(2_000));
    assert_eq!(sent.status, OutboxStatus::Sent);

    let replacement = sent.replaced(
        TxHash::new([3; 32]),
        vec![0x02, 0xff],
        WeiPerGas::from(4_000_000_000_u64),
        WeiPerGas::from(200_000_000_u64),
    );
    assert_eq!(replacement.key(), sent.key(), "the nonce is the same");
    assert_eq!(
        replacement.hashes,
        vec![TxHash::new([2; 32]), TxHash::new([3; 32])]
    );
    assert_eq!(replacement.tx_hash(), TxHash::new([3; 32]));
    assert_eq!(replacement.status, OutboxStatus::Queued);
    assert_eq!(replacement.last_sent_at, None, "the new bytes are unsent");
    assert_eq!(replacement.attempt, sent.attempt);
    assert!(replacement.max_fee > sent.max_fee);
}

/// How long the current bytes have been out, which is what decides a rebroadcast from a
/// replacement.
#[test]
fn an_entry_knows_how_long_its_bytes_have_been_out() {
    let mut entry = entry();
    assert_eq!(entry.sent_for(Timestamp::from_nanos(9_000)), None);
    entry.sent(Timestamp::from_nanos(1_000_000_000));
    assert_eq!(
        entry.sent_for(Timestamp::from_nanos(4_000_000_000)),
        Some(Duration::from_secs(3))
    );
}

/// The receipt's own block is the first confirmation, so a depth of one is satisfied by a
/// receipt in the head block itself, and a receipt ahead of the head is no confirmation at
/// all.
#[test]
fn a_receipt_is_confirmed_once_its_block_is_deep_enough() {
    let block = BlockNumber::new(100);
    let depth = |blocks| BlockDepth::new(blocks);
    assert!(is_confirmed(block, BlockNumber::new(100), depth(1)));
    assert!(!is_confirmed(block, BlockNumber::new(100), depth(2)));
    assert!(is_confirmed(block, BlockNumber::new(101), depth(2)));
    assert!(is_confirmed(block, BlockNumber::new(105), depth(6)));
    assert!(!is_confirmed(block, BlockNumber::new(104), depth(6)));
    assert!(
        !is_confirmed(block, BlockNumber::new(99), depth(1)),
        "a receipt the head has not reached is two moments of the chain, not a confirmation"
    );
    assert!(
        is_confirmed(block, BlockNumber::new(100), depth(0)),
        "a chain configured with no depth still needs the receipt's own block"
    );
}

/// The entry survives the stable map it lives in.
#[test]
fn an_outbox_entry_round_trips_through_storage() {
    let entry = entry();
    assert_eq!(OutboxEntry::from_bytes(entry.to_bytes()), entry);
}

/// An allocation waiting for its signature survives the stable map it lives in, and knows
/// when it has waited long enough that its transaction is never coming.
#[test]
fn an_unsigned_nonce_round_trips_and_knows_when_it_was_abandoned() {
    let unsigned = UnsignedTx {
        purpose: TxPurpose::Payout(QuoteHash::new([4; 32])),
        created_at: Timestamp::from_nanos(1_000_000_000),
    };
    assert_eq!(UnsignedTx::from_bytes(unsigned.to_bytes()), unsigned);

    let window = Duration::from_secs(2);
    assert!(!unsigned.is_stranded(Timestamp::from_nanos(2_999_999_999), window));
    assert!(
        unsigned.is_stranded(Timestamp::from_nanos(3_000_000_000), window),
        "a whole window is long enough: the append and the signed record are one message chain"
    );
    assert!(
        !unsigned.is_stranded(Timestamp::from_nanos(0), window),
        "a clock behind the stamp has waited no time at all"
    );
}
