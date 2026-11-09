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

/// BSC design 6.2.7 and D8: the SpokePool's own relay hash must be the canister's; a fill
/// counts at any agreed block (Jeff saw a delivered payout shown in progress until its
/// deadline), an open deposit only past its deadline.
#[test]
fn an_across_fill_counts_at_once_and_an_open_one_only_past_its_deadline() {
    let local = [0x5a; 32];
    let hash = json!(hex0x(&local));
    let deadline = 1_000;
    let read = |at: u64, hash: Value, status: Value| {
        across_fill_of(Ok(header(90, at)), Ok(hash), Ok(status), local, deadline)
    };
    // before the deadline: filled is filled, open is too early to say, a foreign hash stops
    assert!(matches!(
        read(deadline, hash.clone(), word_of(2)),
        Ok((AcrossFill::Filled, Header { number, .. })) if number == BlockNumber::new(90)
    ));
    for open in [0, 1] {
        assert_eq!(
            read(deadline, hash.clone(), word_of(open)),
            Err(FillRead::TooEarly)
        );
    }
    assert!(matches!(
        read(deadline, json!(hex0x(&[0x5b; 32])), word_of(2)),
        Ok((AcrossFill::HashDiffers, _))
    ));
    let past = deadline + 1;
    assert!(matches!(
        read(past, hash.clone(), word_of(2)),
        Ok((AcrossFill::Filled, Header { number, .. })) if number == BlockNumber::new(90)
    ));
    for open in [0, 1] {
        assert!(matches!(
            read(past, hash.clone(), word_of(open)),
            Ok((AcrossFill::Open, _))
        ));
    }
    assert!(matches!(
        read(past, json!(hex0x(&[0x5b; 32])), word_of(0)),
        Ok((AcrossFill::HashDiffers, _))
    ));
    assert!(matches!(
        read(past, hash, word_of(3)),
        Err(FillRead::Unread(_))
    ));
}

/// Attack M5, on T0's real fill on Arbitrum: a page's fill of the canister's own relay counts,
/// a removed one does not; a fill of another relay reusing the deposit's origin and id (a
/// griefer's own) stops nothing; a page not answered is no answer.
#[test]
fn only_a_fill_of_the_canisters_own_relay_is_a_fill() {
    let receipts: Value = serde_json::from_str(include_str!(
        "../../../types/src/swapic/across/samples/t0_receipts.json"
    ))
    .unwrap();
    let topic = format!(
        "0x{}",
        hex::encode(types::swapic::across::topics::filled_relay())
    );
    let fill = receipts["fill"]["logs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|log| log["topics"][0] == topic.as_str())
        .unwrap()
        .clone();
    let spoke_pool: EvmAddress = fill["address"].as_str().unwrap().parse().unwrap();
    let logs = crate::pool::arrival::logs_of(&json!({ "logs": [fill.clone()] }));
    let key = types::swapic::across::read_filled_relay(&logs[0].topics, &logs[0].data)
        .unwrap()
        .key;
    let page = |logs: Value| Ok(logs);
    assert_eq!(relay_logged(vec![page(json!([fill.clone()]))], spoke_pool, &key), Ok(true));
    assert_eq!(
        relay_logged(vec![page(json!([])), page(json!([]))], spoke_pool, &key),
        Ok(false)
    );
    let mut removed = fill.clone();
    removed["removed"] = json!(true);
    assert_eq!(relay_logged(vec![page(json!([removed]))], spoke_pool, &key), Ok(false));
    // another relay with the deposit's origin and id: a different amount
    let other = types::swapic::across::RelayKey {
        input_amount: key.input_amount + alloy_primitives::U256::from(1u8),
        ..key
    };
    assert_eq!(relay_logged(vec![page(json!([fill.clone()]))], spoke_pool, &other), Ok(false));
    // a log of another contract, or one that is no FilledRelay
    let elsewhere: EvmAddress = "0x7777777777777777777777777777777777777777".parse().unwrap();
    assert_eq!(relay_logged(vec![page(json!([fill.clone()]))], elsewhere, &key), Ok(false));
    assert_eq!(
        relay_logged(vec![page(json!([{"removed": false}]))], spoke_pool, &key),
        Ok(false)
    );
    assert!(relay_logged(
        vec![
            page(json!([])),
            Err(RpcError::Unanswered {
                method: "eth_getLogs".to_string()
            })
        ],
        spoke_pool,
        &key
    )
    .is_err());
}
