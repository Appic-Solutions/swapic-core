use super::*;
use crate::state::transitions::apply_state_transition;
use crate::state::transitions::tests::{funds, swap_id};
use crate::state::State;
use crate::storage::on_fresh_memory;
use ic_stable_structures::Storable;
use std::borrow::Cow;
use types::events::EventType;
use types::{ChainId, Event, Timestamp, TokenAmount};

/// A fold with something in every collection: a pocket, a swap, and its wait in the index.
fn sample_fold() -> MemoryStore {
    let mut state = State::<MemoryStore>::default();
    let steps = [
        (
            1,
            EventType::PocketFunded {
                chain_id: ChainId::BASE,
                amount: TokenAmount::from(1_000_u32),
            },
        ),
        (2, funds(1)),
        (
            3,
            EventType::DecisionRequired {
                quote_hash: swap_id(1),
                reason: "slippage".into(),
            },
        ),
    ];
    for (nanos, payload) in steps {
        state.check(&payload).expect("the fold admits it");
        let meta = state.meta();
        let event = Event::seal(
            meta.next_event_index,
            Timestamp::from_nanos(nanos),
            meta.last_event_hash,
            payload,
        )
        .expect("the payload has a preimage");
        apply_state_transition(&mut state, &event);
    }
    state.into_store()
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
const PINNED: &str = "8184a15820eef2a093530cd1a7b5ddf1c212ddd2243c5329150cbb10a4a91d961a44a09a7889\
    5881010000000000002105000000047573646300000000000000000000000000000064000000000000a4b1\
    00000004757364630000000000000000000000000000006300000000000000000000000000000062000000\
    063078757365720000000001000000000c636374705f76325f66617374000000006b49d200000000000000\
    000104f6f619210564757364631864f603a1192105821903e80083000358205c8d761508201400bbd75a99\
    20b03c5419344867dc65edaeea0fe00ba825500c8182035820eef2a093530cd1a7b5ddf1c212ddd2243c53\
    29150cbb10a4a91d961a44a09a78";

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
