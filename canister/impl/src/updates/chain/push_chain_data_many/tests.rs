use super::*;
use crate::storage::{chain_data, on_fresh_memory};
use candid::Nat;
use settlement_api::types::chain_data::{ChainData, ChainDataEntry, ChainDataError};
use settlement_api::types::errors::Role;

const BASE: u64 = 8453;
const ARBITRUM: u64 = 42161;
const ETHEREUM: u64 = 1;

fn push(chain_id: u64, block: u64) -> ChainPush {
    ChainPush {
        chain_id,
        data: ChainData {
            block,
            base_fee_wei_per_gas: Nat::from(10_000_000_u64),
            priority_fee_wei_per_gas: Nat::from(1_000_000_u64),
        },
    }
}

/// An entry `push_chain_data` refuses: a base fee above 256 bits.
fn too_large(chain_id: u64) -> ChainPush {
    let mut entry = push(chain_id, 7);
    entry.data.base_fee_wei_per_gas = Nat::from(u128::MAX) * Nat::from(u128::MAX) * Nat::from(4_u8);
    entry
}

fn at(secs: u64) -> Timestamp {
    Timestamp::from_secs(secs).expect("a test instant is inside the epoch")
}

fn stored(chain_id: u64) -> Option<ChainDataEntry> {
    chain_data::get(ChainId::new(chain_id)).map(ChainDataEntry::from)
}

/// One result per entry, in order, each what `push_chain_data` answers for it alone: an
/// entry it refuses is refused and stops none of the others, which are stored stamped with
/// the call's instant.
#[test]
fn every_entry_answers_in_order_and_a_bad_one_stops_none() {
    on_fresh_memory(|| {
        chain_data::init();
        let answers = store_all(
            Ok(()),
            vec![push(BASE, 100), too_large(ARBITRUM), push(ETHEREUM, 300)],
            at(1_000),
        );
        assert_eq!(
            answers,
            vec![
                Ok(()),
                Err(PushChainDataError::InvalidData(
                    ChainDataError::FeeTooLarge {
                        field: "base_fee_wei_per_gas".to_string()
                    }
                )),
                Ok(()),
            ]
        );
        assert_eq!(
            stored(BASE),
            Some(ChainDataEntry {
                data: push(BASE, 100).data,
                pushed_at_ns: at(1_000).as_nanos()
            })
        );
        assert_eq!(stored(ARBITRUM), None, "the refused entry stored nothing");
        assert_eq!(stored(ETHEREUM).map(|entry| entry.data.block), Some(300));
        // each answer is exactly push_chain_data's for that entry on its own
        for entry in [push(BASE, 100), too_large(ARBITRUM), push(ETHEREUM, 300)] {
            let alone = super::super::push_chain_data::store_reading(
                ChainId::new(entry.chain_id),
                entry.data.clone(),
                at(1_000),
            );
            assert_eq!(store_all(Ok(()), vec![entry], at(1_000)), vec![alone]);
        }
    });
}

/// The role is checked once: a caller without it has every entry refused with the guard's
/// error, and nothing is stored.
#[test]
fn a_caller_without_the_role_has_every_entry_refused_and_nothing_stored() {
    on_fresh_memory(|| {
        chain_data::init();
        let refusal = GuardError::CallerNotRole(Role::Watcher);
        let answers = store_all(
            Err(refusal.clone()),
            vec![push(BASE, 100), too_large(ARBITRUM), push(ETHEREUM, 300)],
            at(1_000),
        );
        assert_eq!(answers, vec![Err(PushChainDataError::Guard(refusal)); 3]);
        for chain in [BASE, ARBITRUM, ETHEREUM] {
            assert_eq!(stored(chain), None);
        }
        assert_eq!(
            store_all(Err(GuardError::RolesNotSet), vec![], at(1_000)),
            vec![]
        );
    });
}

/// A chain named twice is stored twice, in order: the later reading is the one kept.
#[test]
fn a_chain_named_twice_is_stored_in_order() {
    on_fresh_memory(|| {
        chain_data::init();
        let answers = store_all(Ok(()), vec![push(BASE, 100), push(BASE, 90)], at(1_000));
        assert_eq!(answers, vec![Ok(()), Ok(())]);
        assert_eq!(stored(BASE).map(|entry| entry.data.block), Some(90));
    });
}
