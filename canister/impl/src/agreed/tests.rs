use super::*;

fn word_of(last: u8) -> Value {
    let mut word = [0_u8; 32];
    word[31] = last;
    json!(hex0x(&word))
}

fn header(number: u64, timestamp: u64) -> Value {
    json!({
        "number": format!("0x{number:x}"),
        "hash": hex0x(&[7; 32]),
        "timestamp": format!("0x{timestamp:x}"),
    })
}

/// Swapic's `used` answers a boolean word: true, false, or no answer to trust.
#[test]
fn swapics_used_reads_as_a_boolean_or_nothing() {
    let read = |value: Value| word(Ok(value)).and_then(ran_of);
    assert_eq!(read(word_of(1)), Ok(true));
    assert_eq!(read(word_of(0)), Ok(false));
    assert!(read(word_of(2)).is_err(), "no boolean");
    assert!(read(json!("0x01")).is_err(), "no word");
    assert!(word(Err(RpcError::Unanswered {
        method: "eth_call".to_string()
    }))
    .is_err());
}

/// L-03 and M-03: "not filled" is believed only at an agreed block past the time asked (the
/// route's deadline): before it, too early, whatever the Portal says; at it, still too
/// early (a fill in that very block is in time). A filled or cancelled intent reads as it
/// is, with its block.
#[test]
fn a_fill_is_read_only_at_a_block_past_the_time() {
    let deadline = 1_800_000_000;
    let open = word_of(0);
    assert_eq!(
        fill_of(Ok(header(100, deadline)), Ok(open.clone()), deadline),
        Err(FillRead::TooEarly)
    );
    assert_eq!(
        fill_of(Ok(header(100, deadline - 30)), Ok(open.clone()), deadline),
        Err(FillRead::TooEarly)
    );
    assert_eq!(
        fill_of(Ok(header(101, deadline + 2)), Ok(open), deadline),
        Ok((Fill::Open, BlockNumber::new(101)))
    );
    assert_eq!(
        fill_of(
            Ok(header(101, deadline + 2)),
            Ok(json!(hex0x(&types::eco::CANCELLED))),
            deadline
        ),
        Ok((Fill::Cancelled, BlockNumber::new(101)))
    );
    assert_eq!(
        fill_of(
            Ok(header(101, deadline + 2)),
            Ok(json!(hex0x(&[9; 32]))),
            deadline
        ),
        Ok((Fill::Filled, BlockNumber::new(101)))
    );
    // a block the nodes do not see, or a read that failed: unread, asked again
    assert!(matches!(
        fill_of(Ok(Value::Null), Ok(word_of(0)), deadline),
        Err(FillRead::Unread(_))
    ));
    assert!(matches!(
        fill_of(
            Ok(header(101, deadline + 2)),
            Err(RpcError::Rpc {
                method: "eth_call".to_string(),
                message: "execution reverted".to_string()
            }),
            deadline
        ),
        Err(FillRead::Unread(_))
    ));
}

/// The calls name their block, never `latest`, and carry no sender: every node asks the
/// same.
#[test]
fn an_agreed_call_names_its_block() {
    let to = EvmAddress::new([5; 20]);
    let params = call(to, &[1, 2], BlockNumber::new(0x2a));
    assert_eq!(params[1], json!("0x2a"));
    assert_eq!(params[0].get("from"), None);
    assert_eq!(params[0]["data"], json!("0x0102"));
}

/// Review of the merged canister (item 1): `used` is read at a block at or after the reverted
/// transaction's own block and at least the crediting depth below the agreed head. The head
/// rounded down to sixteen blocks can name a block before the transaction (as low as eleven
/// blocks before it): there `used` is false whatever the transaction did, so no answer is
/// given until a block that late is agreed on.
#[test]
fn swapics_used_is_read_only_at_or_after_the_transactions_block() {
    let depth = types::BlockDepth::new(5);
    let at = BlockNumber::new;
    // the transaction at 1_000; an agreed head rounded down to 1_001 - 5 = 996, eleven below
    // the transaction's block plus its depth
    assert_eq!(read_block(at(1_001), depth, at(1_000)), None);
    assert_eq!(read_block(at(1_005), depth, at(1_000)), Some(at(1_000)));
    assert_eq!(read_block(at(1_040), depth, at(1_000)), Some(at(1_035)));
}

/// The last review (item 3): one rule for how long after a time a read for a block past it
/// waits, whatever reads it (a payout's fill, a move's, a refund's proof): the agreed block
/// trails the head by the grain and the crediting depth, so that many of the chain's blocks,
/// and a minute.
#[test]
fn a_read_past_a_time_waits_the_grain_and_the_depth_in_the_chains_blocks() {
    use std::time::Duration;
    assert_eq!(
        fill_wait(ChainId::BASE, 1),
        Duration::from_secs((16 + 1) * 2 + 60)
    );
    assert_eq!(
        fill_wait(ChainId::ETHEREUM, 12),
        Duration::from_secs((16 + 12) * 12 + 60)
    );
}
