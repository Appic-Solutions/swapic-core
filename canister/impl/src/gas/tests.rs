use super::*;

/// Real answers of 2026-11-02 (`cast rpc eth_feeHistory 5 latest '[50]'`). Base: the
/// newest block, its own base fee, and the median of the five tips. Arbitrum: tips of zero.
#[test]
fn a_fee_history_reads_as_its_newest_block_and_the_median_tip() {
    let base: Value = serde_json::from_str(
        r#"{"baseFeePerGas":["0x4c4b40","0x4c4b40","0x4c4b40","0x4c4b40","0x4c4b40","0x4c4b40"],"gasUsedRatio":[0.0570335775,0.055843535,0.079830145,0.1185713675,0.095872145],"baseFeePerBlobGas":["0x1","0x1","0x1","0x1","0x1","0x1"],"blobGasUsedRatio":[0.0,0.0,0.0,0.0,0.0],"oldestBlock":"0x32f3786","reward":[["0x132eee"],["0x2160ec"],["0x143381"],["0x14c5d0"],["0x130498"]]}"#,
    )
    .unwrap();
    assert_eq!(
        reading_of(&base),
        Some(ChainReading {
            block: BlockNumber::new(0x32f3786 + 4),
            base_fee: WeiPerGas::new(5_000_000),
            // the middle of 1,246,360, 1,257,198, 1,323,905, 1,361,360 and 2,187,500
            priority_fee: WeiPerGas::new(0x143381),
        })
    );
    let arbitrum: Value = serde_json::from_str(
        r#"{"oldestBlock":"0x1f0847d2","reward":[["0x0"],["0x0"],["0x0"],["0x0"],["0x0"]],"baseFeePerGas":["0x13255e0","0x1312d00","0x1313ca0","0x1314470","0x1312d00","0x1312d00"],"gasUsedRatio":[0.008059428571428572,0.013609285714285714,0.019413714285714284,0.004440785714285714,0.01634714285714286]}"#,
    )
    .unwrap();
    assert_eq!(
        reading_of(&arbitrum),
        Some(ChainReading {
            block: BlockNumber::new(0x1f0847d2 + 4),
            // the fifth block's own, not the sixth (the next block's)
            base_fee: WeiPerGas::new(0x1312d00),
            priority_fee: WeiPerGas::ZERO,
        })
    );
}

/// A chain that answers no rewards (or an empty window of them) tips nothing; an answer
/// with no window, or a field that is not a number, reads as nothing.
#[test]
fn a_fee_history_without_rewards_tips_nothing_and_a_broken_one_reads_nothing() {
    let answer = json!({
        "oldestBlock": "0x10",
        "baseFeePerGas": ["0x9", "0x9"],
        "gasUsedRatio": [0.5],
    });
    let reading = reading_of(&answer).unwrap();
    assert_eq!(reading.block, BlockNumber::new(0x10));
    assert_eq!(reading.base_fee, WeiPerGas::new(9));
    assert_eq!(reading.priority_fee, WeiPerGas::ZERO);
    assert_eq!(
        reading_of(&json!({ "oldestBlock": "0x10", "baseFeePerGas": ["0x9"], "gasUsedRatio": [] })),
        None
    );
    assert_eq!(
        reading_of(
            &json!({ "oldestBlock": "0x", "baseFeePerGas": ["0x9", "0x9"], "gasUsedRatio": [0.5] })
        ),
        None
    );
    assert_eq!(
        reading_of(&json!({
            "oldestBlock": "0x10",
            "baseFeePerGas": ["0x9", "0x9"],
            "gasUsedRatio": [0.5],
            "reward": [["tip"]]
        })),
        None
    );
}
