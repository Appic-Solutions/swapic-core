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
/// while it is inside [`READING_MAX_AGE`] and nothing once it ages out, so a caller cannot
/// decide on stale gas prices by forgetting to check.
#[test]
fn the_fresh_read_answers_nothing_once_the_data_ages_out() {
    on_fresh_memory(|| {
        init();
        put(ChainId::BASE, reading(10, 1_000), at(100));
        assert_eq!(
            fresh(ChainId::BASE, at(105)),
            Some(reading(10, 1_000).pushed_at(at(100)))
        );
        assert_eq!(
            fresh(ChainId::BASE, at(110)),
            Some(reading(10, 1_000).pushed_at(at(100))),
            "exactly the cap is still fresh"
        );
        assert_eq!(fresh(ChainId::BASE, at(111)), None);
        assert_eq!(
            fresh(ChainId::ARBITRUM, at(100)),
            None,
            "a chain never pushed is never fresh"
        );
        // a later push makes it fresh again, without a removal
        put(ChainId::BASE, reading(12, 1_200), at(200));
        assert_eq!(
            fresh(ChainId::BASE, at(205)),
            Some(reading(12, 1_200).pushed_at(at(200)))
        );
    });
}

/// A reading a call carries is stored and fresh at once; an invalid one is refused and
/// leaves the stored one as it was, and one whose block is not above the stored one's is
/// kept out.
#[test]
fn a_carried_reading_is_fresh_at_once_and_an_invalid_one_is_not_stored() {
    on_fresh_memory(|| {
        init();
        let chain = ChainId::new(42161);
        let good = settlement_api::types::chain_data::ChainData {
            base_fee_wei_per_gas: candid::Nat::from(20_000_000u64),
            block: 516_007_616,
            priority_fee_wei_per_gas: candid::Nat::from(0u64),
        };
        crate::relay::take_reading(chain, Some(good.clone()), at(100)).unwrap();
        assert_eq!(get(chain).map(|d| d.block.get()), Some(516_007_616));
        assert!(fresh(chain, at(100)).is_some());
        let mut bad = good.clone();
        bad.block = 516_007_700;
        // 2^256, the first fee no 256-bit price holds
        bad.base_fee_wei_per_gas = candid::Nat::parse(
            b"115792089237316195423570985008687907853269984665640564039457584007913129639936",
        )
        .unwrap();
        assert!(crate::relay::take_reading(chain, Some(bad), at(101)).is_err());
        assert_eq!(get(chain).map(|d| d.pushed_at), Some(at(100)));
        let older = settlement_api::types::chain_data::ChainData {
            block: 516_007_600,
            ..good
        };
        crate::relay::take_reading(chain, Some(older), at(102)).unwrap();
        assert_eq!(get(chain).map(|d| d.pushed_at), Some(at(100)));
        crate::relay::take_reading(chain, None, at(103)).unwrap();
        assert_eq!(get(chain).map(|d| d.pushed_at), Some(at(100)));
    });
}
