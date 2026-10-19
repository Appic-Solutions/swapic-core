use super::*;
use crate::storage::on_fresh_memory;
use types::numeric::{BlockNumber, WeiPerGas};

fn reading(block: u64, base_fee: u64) -> ChainReading {
    ChainReading {
        block: BlockNumber::new(block),
        base_fee: WeiPerGas::from(base_fee),
        priority_fee: WeiPerGas::from(100_000_000_u64),
    }
}

fn at(secs: u64) -> Timestamp {
    Timestamp::from_secs(secs).expect("a test instant is inside the epoch")
}

/// One entry per chain: a push replaces the chain's entry and touches no other chain.
#[test]
fn a_push_replaces_the_chains_own_entry_and_leaves_the_others_alone() {
    on_fresh_memory(|| {
        init();
        assert_eq!(get(ChainId::BASE), None, "an unpushed chain has no data");
        put(ChainId::BASE, reading(10, 1_000), at(100));
        put(ChainId::ARBITRUM, reading(20, 2_000), at(100));
        put(ChainId::BASE, reading(11, 1_500), at(200));
        assert_eq!(
            get(ChainId::BASE),
            Some(reading(11, 1_500).pushed_at(at(200)))
        );
        assert_eq!(
            get(ChainId::ARBITRUM),
            Some(reading(20, 2_000).pushed_at(at(100)))
        );
    });
}

/// The freshness read is the one money-deciding door onto the cache: it answers the entry
/// while it is inside the cap and nothing once it ages out, so a caller cannot decide on
/// stale gas prices by forgetting to check.
#[test]
fn the_fresh_read_answers_nothing_once_the_data_ages_out() {
    on_fresh_memory(|| {
        init();
        let max_age = Duration::from_secs(10);
        put(ChainId::BASE, reading(10, 1_000), at(100));
        assert_eq!(
            fresh(ChainId::BASE, at(105), max_age),
            Some(reading(10, 1_000).pushed_at(at(100)))
        );
        assert_eq!(
            fresh(ChainId::BASE, at(110), max_age),
            Some(reading(10, 1_000).pushed_at(at(100))),
            "exactly the cap is still fresh"
        );
        assert_eq!(fresh(ChainId::BASE, at(111), max_age), None);
        assert_eq!(
            fresh(ChainId::ARBITRUM, at(100), max_age),
            None,
            "a chain never pushed is never fresh"
        );
        // a later push makes it fresh again, without a removal
        put(ChainId::BASE, reading(12, 1_200), at(200));
        assert_eq!(
            fresh(ChainId::BASE, at(205), max_age),
            Some(reading(12, 1_200).pushed_at(at(200)))
        );
    });
}

/// The one door every reading takes in, the push's and the one a finish or a relay
/// carries: a valid reading is stored and fresh at once, and an invalid one is refused and
/// leaves the stored one as it was.
#[test]
fn a_stored_reading_is_fresh_at_once_and_an_invalid_one_is_not_stored() {
    on_fresh_memory(|| {
        init();
        let chain = ChainId::new(42161);
        let good = settlement_api::types::chain_data::ChainData {
            base_fee_wei_per_gas: candid::Nat::from(20_000_000u64),
            block: 516_007_616,
            priority_fee_wei_per_gas: candid::Nat::from(0u64),
        };
        crate::updates::store_reading(chain, good.clone(), at(100)).unwrap();
        assert_eq!(get(chain).map(|d| d.block.get()), Some(516_007_616));
        assert!(fresh(chain, at(100), Duration::from_secs(10)).is_some());
        let mut bad = good;
        bad.block = 516_007_700;
        // 2^256, the first fee no 256-bit price holds
        bad.base_fee_wei_per_gas = candid::Nat::parse(
            b"115792089237316195423570985008687907853269984665640564039457584007913129639936",
        )
        .unwrap();
        assert!(crate::updates::store_reading(chain, bad, at(101)).is_err());
        assert_eq!(get(chain).map(|d| d.block.get()), Some(516_007_616));
        assert_eq!(get(chain).map(|d| d.pushed_at), Some(at(100)));
    });
}
