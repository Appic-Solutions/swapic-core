use super::*;
use crate::state::Store;
use crate::storage::on_fresh_memory;
use ic_stable_structures::Storable;
use std::borrow::Cow;
use types::{
    Attempt, ChainId, EventHash, EventIndex, LedgerMeta, Pocket, QuoteHash, Swap, SwapStatus,
    Timestamp, TokenAmount, WaitingKey,
};

/// A fold with something in every collection, built from literal values so the pinned
/// bytes below move only when the layout does. Every field differs from its neighbours.
fn sample_fold() -> MemoryStore {
    let quote_hash = QuoteHash::new([0x5a; 32]);
    let since = Timestamp::from_nanos(1_700_000_000_123_456_789);
    let mut store = MemoryStore::default();
    store.put_swap(
        quote_hash,
        Swap {
            quote_bytes: vec![0xde, 0xad, 0xbe, 0xef],
            status: SwapStatus::WaitingForUser,
            last_attempt: Some(Attempt::new(3)),
            open_attempt: None,
            src_chain: ChainId::BASE,
            src_token: "USDC".parse().unwrap(),
            amount_in: TokenAmount::from(25_000_000_u32),
            amount_paid: Some(TokenAmount::from(24_990_000_u32)),
            waiting_since: Some(since),
        },
    );
    store.put_pocket(
        ChainId::ARBITRUM,
        Pocket {
            available: TokenAmount::from(1u128 << 70),
            reserved: TokenAmount::from(250_u32),
        },
    );
    store.put_meta(LedgerMeta {
        fees_accrued: TokenAmount::from(7_u32),
        next_event_index: EventIndex::new(19),
        last_event_hash: EventHash::new([0xab; 32]),
    });
    store.put_auto_refund_waiting(WaitingKey { since, quote_hash });
    store
}

/// A canister that has never run the deep audit starts it at genesis: the empty fold, which
/// seals at index zero on the zero hash. A saved fold survives in stable memory whole, and a
/// finished audit puts genesis back.
#[test]
fn a_fresh_canister_starts_the_deep_audit_at_genesis() {
    on_fresh_memory(|| {
        init();
        assert_eq!(get(), ReplayCursor::genesis());
        let saved = ReplayCursor {
            fold: sample_fold(),
        };
        set(saved.clone());
        assert_eq!(get(), saved, "a saved fold reads back whole");
        set(ReplayCursor::genesis());
        assert_eq!(get(), ReplayCursor::genesis());
    });
}

/// The bytes of the sample fold as this wasm writes them. Pinned the way
/// `storage_v1.txt` pins the other stored layouts: the wasm that upgrades over a saved fold
/// reads it with the types it has, so a field that moves must fail here, before anything is
/// deployed, and never on the read after an upgrade.
///
/// The array grew from four elements to five when the fold gained the nonce allocator:
/// `8184` became `8185` and an empty map was appended, which is what appending a field to
/// the fold looks like. Nothing before it moved.
const PINNED: &str =
    "8185a158205a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a8944deadbeef04\
    03f619210564555344431a017d78401a017d51301b17979cfe3d85cd15a119a4b182c2494000000000000000\
    0018fa8307135820abababababababababababababababababababababababababababababababab81821b17\
    979cfe3d85cd1558205a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5aa0";

#[test]
fn a_saved_fold_decodes_from_its_pinned_bytes() {
    let cursor = ReplayCursor {
        fold: sample_fold(),
    };
    let bytes = cursor.to_bytes().into_owned();
    assert_eq!(
        hex::encode(&bytes),
        PINNED,
        "the stored layout moved: append a field, never renumber one"
    );
    assert_eq!(ReplayCursor::from_bytes(Cow::Owned(bytes)), cursor);
}

/// The saved fold is scratch the next step rebuilds from the log, so bytes this wasm cannot
/// read are not a reason to refuse the upgrade that shipped it: they read as genesis, and
/// the audit in progress starts over.
#[test]
fn a_saved_fold_that_no_longer_decodes_reads_as_genesis() {
    for garbage in [vec![], vec![0xff, 0x00, 0x01], vec![0x82, 0x00, 0x00]] {
        assert_eq!(
            ReplayCursor::from_bytes(Cow::Owned(garbage)),
            ReplayCursor::genesis()
        );
    }
}
