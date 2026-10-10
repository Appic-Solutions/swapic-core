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
