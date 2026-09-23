use super::*;
use serde_json::json;
use types::BlockDepth;

const VAULT: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
const USDC: &str = "0xaf88d065e77c8cC2239327C5EDb3A432268e5831";
const USER: &str = "0x7551A66653f9a20979ed81835a0b7008EC83401b";

fn vault() -> EvmAddress {
    VAULT.parse().unwrap()
}

fn quote_hash() -> QuoteHash {
    QuoteHash::new([0x5a; 32])
}

fn word_of(address: &str) -> String {
    let address: EvmAddress = address.parse().unwrap();
    format!("0x{}", hex::encode(address.to_word()))
}

/// A `Deposited` log as a provider answers it, for `quote_hash` at `block`: the fixture's
/// token from the fixture's payer.
fn deposit_log(quote_hash: QuoteHash, block: u64, amount: u64) -> Value {
    deposit_log_of(quote_hash, block, USDC, USER, amount)
}

/// A `Deposited` log of `token` from `from`, for `quote_hash` at `block`.
fn deposit_log_of(
    quote_hash: QuoteHash,
    block: u64,
    token: &str,
    from: &str,
    amount: u64,
) -> Value {
    json!({
        "address": VAULT.to_ascii_lowercase(),
        "topics": [
            format!("0x{}", hex::encode(deposited_topic())),
            format!("0x{}", hex::encode(quote_hash.as_ref())),
            word_of(token),
            word_of(from),
        ],
        "data": format!("0x{:064x}", amount),
        "blockNumber": format!("0x{block:x}"),
        "blockHash": format!("0x{}", hex::encode([block as u8; 32])),
        "transactionHash": format!("0x{}", hex::encode([0x77; 32])),
        "logIndex": "0x2",
        "removed": false,
    })
}

/// One answer walked as the whole range: what the read decides when the range is one
/// window, which is how the tests below pin the rule inside a window.
fn decide(
    logs: &[Value],
    vault: EvmAddress,
    quote_hash: QuoteHash,
    wanted: &Wanted,
    latest: BlockNumber,
    depth: BlockDepth,
) -> Result<VerifiedDeposit, DepositError> {
    let mut walk = Walk::new(quote_hash, depth);
    walk.take(find(logs, vault, quote_hash, wanted, latest, depth))
        .ok_or_else(|| walk.end())
}

/// The deposit the claim wants: the fixture's token, exactly `amount`.
fn exactly(amount: u64) -> Wanted {
    Wanted {
        token: USDC.parse().unwrap(),
        amount: WantedAmount::Exactly(TokenAmount::from(amount)),
    }
}

/// The deposit an arrival read wants: the fixture's token, at least `amount`.
fn at_least(amount: u64) -> Wanted {
    Wanted {
        token: USDC.parse().unwrap(),
        amount: WantedAmount::AtLeast(TokenAmount::from(amount)),
    }
}

/// A valid log decodes to the deposit: the token and the payer out of the indexed words,
/// the amount out of the data, and the transaction and block the chain holds it in.
#[test]
fn a_valid_log_decodes_to_the_deposit() {
    let deposit = decide(
        &[deposit_log(quote_hash(), 19_000_000, 1_000_000)],
        vault(),
        quote_hash(),
        &exactly(1_000_000),
        BlockNumber::new(19_000_000),
        BlockDepth::new(1),
    )
    .expect("a deep enough deposit is verified");
    assert_eq!(
        deposit,
        VerifiedDeposit {
            token: USDC.parse().unwrap(),
            from: USER.parse().unwrap(),
            amount: TokenAmount::from(1_000_000_u32),
            tx_ref: TxHash::new([0x77; 32]),
            block: BlockNumber::new(19_000_000),
            // the fixture's block hash is its number's low byte, 0xc0 for 19,000,000
            block_hash: [0xc0; 32],
        }
    );
}

/// A log the head has not reached, or not by the configured depth, is a deposit the
/// chain may still drop: not verified, and the refusal says how deep it is.
#[test]
fn a_log_shallower_than_the_depth_is_not_confirmed() {
    let logs = [deposit_log(quote_hash(), 19_000_005, 1)];
    let shallow = decide(
        &logs,
        vault(),
        quote_hash(),
        &exactly(1),
        BlockNumber::new(19_000_006),
        BlockDepth::new(6),
    );
    assert_eq!(
        shallow,
        Err(DepositError::NotConfirmed {
            block: BlockNumber::new(19_000_005),
            latest: BlockNumber::new(19_000_006),
            depth: BlockDepth::new(6),
        })
    );
    // a block the head has not reached is two moments of the chain, not a confirmation
    let ahead = decide(
        &logs,
        vault(),
        quote_hash(),
        &exactly(1),
        BlockNumber::new(19_000_004),
        BlockDepth::new(1),
    );
    assert!(
        matches!(ahead, Err(DepositError::NotConfirmed { .. })),
        "{ahead:?}"
    );
    // at depth it is verified
    assert!(decide(
        &logs,
        vault(),
        quote_hash(),
        &exactly(1),
        BlockNumber::new(19_000_010),
        BlockDepth::new(6)
    )
    .is_ok());
}

/// No log at all is no deposit, named by chain and quote.
#[test]
fn no_log_is_not_found() {
    assert_eq!(
        decide(
            &[],
            vault(),
            quote_hash(),
            &exactly(1),
            BlockNumber::new(19_000_000),
            BlockDepth::new(1)
        ),
        Err(DepositError::NotFound {
            quote_hash: quote_hash()
        })
    );
}

/// The provider is asked for one quote's deposits, and is not trusted to have listened: a
/// log for another quote, another contract, another event, a removed log or one not in any
/// block decides nothing, and only a log about this quote counts.
#[test]
fn a_log_that_is_not_this_quotes_deposit_is_ignored() {
    let other = deposit_log(QuoteHash::new([0x5b; 32]), 19_000_000, 5);
    let mut elsewhere = deposit_log(quote_hash(), 19_000_000, 5);
    elsewhere["address"] = json!(USDC);
    let mut another_event = deposit_log(quote_hash(), 19_000_000, 5);
    another_event["topics"][0] = json!(format!("0x{}", hex::encode([0xee; 32])));
    let mut removed = deposit_log(quote_hash(), 19_000_000, 5);
    removed["removed"] = json!(true);
    let mut pending = deposit_log(quote_hash(), 19_000_000, 5);
    pending["blockHash"] = Value::Null;
    let mut short_data = deposit_log(quote_hash(), 19_000_000, 5);
    short_data["data"] = json!("0x05");
    let mut three_topics = deposit_log(quote_hash(), 19_000_000, 5);
    three_topics["topics"].as_array_mut().unwrap().pop();
    let noise = vec![
        other,
        elsewhere,
        another_event,
        removed,
        pending,
        short_data,
        three_topics,
        json!(null),
        json!("not a log"),
    ];
    assert_eq!(
        decide(
            &noise,
            vault(),
            quote_hash(),
            &exactly(7),
            BlockNumber::new(19_000_100),
            BlockDepth::new(1)
        ),
        Err(DepositError::NotFound {
            quote_hash: quote_hash()
        }),
        "none of these is this quote's deposit"
    );

    // and the one that is, behind all of them, is found
    let mut with_ours = noise;
    with_ours.push(deposit_log(quote_hash(), 19_000_001, 7));
    let found = decide(
        &with_ours,
        vault(),
        quote_hash(),
        &exactly(7),
        BlockNumber::new(19_000_100),
        BlockDepth::new(1),
    )
    .expect("the deposit behind the noise is verified");
    assert_eq!(found.amount, TokenAmount::from(7_u8));
    assert_eq!(found.block, BlockNumber::new(19_000_001));
}

/// The read asks for the provider's head alone, then for the logs of the vault for this
/// quote one window at a time. A lookback covers that many blocks ending at the anchor, so
/// a lookback of ten thousand blocks is exactly one window, and the newest window is open
/// at the head so a deposit the watcher's reading has not reached yet is still seen.
///
/// Rewritten for fix wave 5 (M1): the head is asked before any window is built, because
/// every window is built from it, so a window's call is its logs alone where it was the
/// head and the logs in one batch.
#[test]
fn the_read_asks_for_the_head_and_the_quotes_logs_over_the_lookback() {
    let from = range_from(BlockNumber::new(19_000_000), DepositLookback::new(10_000));
    assert_eq!(from, BlockNumber::new(18_990_001));
    assert_eq!(
        range_from(BlockNumber::new(5), DepositLookback::new(10_000)),
        BlockNumber::new(0),
        "a lookback past genesis starts at genesis"
    );
    let windows = windows(from, BlockNumber::new(19_000_000)).unwrap();
    assert_eq!(
        windows,
        vec![Window { from, to: None }],
        "a lookback of one window is one window, open at the head"
    );
    let (method, params) = logs_call(vault(), quote_hash(), &windows[0]);
    assert_eq!(method, "eth_getLogs");
    assert_eq!(
        params,
        json!([{
            "address": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
            "topics": [
                "0xcfccc5211684bc31ce945214025a7453ba30a1ffcc38fcb691ce742437f3f256",
                "0x5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a",
            ],
            "fromBlock": "0x121c3b1",
            "toBlock": "latest",
        }])
    );
    let closed = Window {
        from: BlockNumber::new(18_980_001),
        to: Some(BlockNumber::new(18_990_000)),
    };
    assert_eq!(
        logs_call(vault(), quote_hash(), &closed).1[0]["toBlock"],
        json!("0x121c3b0"),
        "an older window ends where the newer one starts"
    );
}

/// A range wider than one window is walked in windows of [`LOGS_WINDOW_BLOCKS`], oldest
/// first: the deposit that counts is the oldest deep one in range, so the first window
/// holding one ends the walk. The windows tile the range exactly, the newest open at the
/// head, and a range that would take more than [`MAX_LOGS_WINDOWS`] is refused by name
/// before any outcall rather than walked for minutes or cut short in silence.
///
/// Rewritten for fix wave 4 (N2): this was the newest-first walk, which let a window's
/// boundary decide which of two deposits counted.
#[test]
fn a_wide_range_is_walked_in_windows_oldest_first_and_a_wider_one_is_refused() {
    let anchor = BlockNumber::new(19_000_000);
    let from = BlockNumber::new(18_975_000);
    assert_eq!(
        windows(from, anchor).unwrap(),
        vec![
            Window {
                from: BlockNumber::new(18_975_000),
                to: Some(BlockNumber::new(18_980_000)),
            },
            Window {
                from: BlockNumber::new(18_980_001),
                to: Some(BlockNumber::new(18_990_000)),
            },
            Window {
                from: BlockNumber::new(18_990_001),
                to: None,
            },
        ]
    );
    // a registration block past the anchor: one window, from it
    assert_eq!(
        windows(BlockNumber::new(19_000_003), anchor).unwrap(),
        vec![Window {
            from: BlockNumber::new(19_000_003),
            to: None,
        }]
    );
    let widest = BlockNumber::new(anchor.get() + 1 - LOGS_WINDOW_BLOCKS * MAX_LOGS_WINDOWS);
    assert_eq!(
        windows(widest, anchor).unwrap().len() as u64,
        MAX_LOGS_WINDOWS,
        "the cap is inclusive"
    );
    let too_wide = BlockNumber::new(widest.get() - 1);
    assert_eq!(
        windows(too_wide, anchor),
        Err(DepositError::RangeTooWide {
            from: too_wide,
            anchor,
            windows: MAX_LOGS_WINDOWS + 1,
            cap: MAX_LOGS_WINDOWS,
        })
    );
}

/// The deposit that counts is the one that matches what the read is looking for, not the
/// first one logged: the vault marks a quote per payer, so anyone can put a log ahead of
/// the user's under the same hash, and a dust deposit ahead of the real one changes
/// nothing. Forty of them still leave the real one found.
#[test]
fn the_deposit_that_counts_is_the_one_that_matches_not_the_first() {
    let griefer = "0x1111111111111111111111111111111111111111";
    let dust = deposit_log_of(quote_hash(), 18_999_990, USDC, griefer, 1);
    let other_token = deposit_log_of(quote_hash(), 18_999_991, VAULT, griefer, 1_000_000);
    let real = deposit_log(quote_hash(), 18_999_995, 1_000_000);
    let found = decide(
        &[dust.clone(), other_token.clone(), real.clone()],
        vault(),
        quote_hash(),
        &exactly(1_000_000),
        BlockNumber::new(19_000_000),
        BlockDepth::new(1),
    )
    .expect("the real deposit behind the dust is the one");
    assert_eq!(found.from, USER.parse().unwrap());
    assert_eq!(found.amount, TokenAmount::from(1_000_000_u32));
    assert_eq!(found.block, BlockNumber::new(18_999_995));

    let mut forty_ahead: Vec<Value> = (0..40)
        .map(|i| deposit_log_of(quote_hash(), 18_999_900 + i, USDC, griefer, 1 + i))
        .collect();
    forty_ahead.push(real);
    let found = decide(
        &forty_ahead,
        vault(),
        quote_hash(),
        &exactly(1_000_000),
        BlockNumber::new(19_000_000),
        BlockDepth::new(1),
    )
    .expect("forty dust logs ahead change nothing");
    assert_eq!(found.from, USER.parse().unwrap());

    // logs about the quote that are none of them the deposit wanted: refused by count, so
    // an operator can tell stranded funds from no deposit at all
    assert_eq!(
        decide(
            &[dust, other_token],
            vault(),
            quote_hash(),
            &exactly(1_000_000),
            BlockNumber::new(19_000_000),
            BlockDepth::new(1),
        ),
        Err(DepositError::NoneMatches {
            quote_hash: quote_hash(),
            seen: 2,
        })
    );
}

/// An arrival read wants at least the least the user was quoted, in the token the user is
/// paid: a dust deposit into the destination vault under the quote's hash is not the fill
/// and never becomes `PaidInStable`, and the fill behind it is.
#[test]
fn a_dust_arrival_on_the_destination_is_not_the_fill() {
    let griefer = "0x1111111111111111111111111111111111111111";
    let dust = deposit_log_of(quote_hash(), 18_999_990, USDC, griefer, 1);
    assert_eq!(
        decide(
            std::slice::from_ref(&dust),
            vault(),
            quote_hash(),
            &at_least(24_900_000),
            BlockNumber::new(19_000_000),
            BlockDepth::new(1),
        ),
        Err(DepositError::NoneMatches {
            quote_hash: quote_hash(),
            seen: 1,
        })
    );
    let fill = deposit_log_of(quote_hash(), 18_999_995, USDC, griefer, 24_950_000);
    let found = decide(
        &[dust, fill],
        vault(),
        quote_hash(),
        &at_least(24_900_000),
        BlockNumber::new(19_000_000),
        BlockDepth::new(1),
    )
    .expect("a fill of at least the minimum is the arrival");
    assert_eq!(found.amount, TokenAmount::from(24_950_000_u32));
    // exactly the minimum is enough, and one unit under is not
    let exact = deposit_log_of(quote_hash(), 18_999_996, USDC, griefer, 24_900_000);
    assert!(decide(
        &[exact],
        vault(),
        quote_hash(),
        &at_least(24_900_000),
        BlockNumber::new(19_000_000),
        BlockDepth::new(1),
    )
    .is_ok());
    let short = deposit_log_of(quote_hash(), 18_999_996, USDC, griefer, 24_899_999);
    assert!(matches!(
        decide(
            &[short],
            vault(),
            quote_hash(),
            &at_least(24_900_000),
            BlockNumber::new(19_000_000),
            BlockDepth::new(1),
        ),
        Err(DepositError::NoneMatches { seen: 1, .. })
    ));
}

/// A deposit of the fixture's token from the fixture's payer at `block`.
fn deposit_at(block: u64) -> VerifiedDeposit {
    VerifiedDeposit {
        token: USDC.parse().unwrap(),
        from: USER.parse().unwrap(),
        amount: TokenAmount::from(1_u8),
        tx_ref: TxHash::new([0x77; 32]),
        block: BlockNumber::new(block),
        block_hash: [block as u8; 32],
    }
}

/// The range's verdict, windows taken oldest first: the first deep match ends the walk and
/// is the one, a match not deep enough in an older window does not stop the walk and never
/// hides a deep one in a newer window, and a range with no deep match is refused as not
/// confirmed at its oldest shallow match, or by the count of what matched nothing, or as
/// no deposit at all.
#[test]
fn the_walk_takes_the_oldest_deep_match_and_a_shallow_one_never_masks_it() {
    let depth = BlockDepth::new(6);
    let mut walk = Walk::new(quote_hash(), depth);
    assert_eq!(walk.take(Finding::Unmatched { seen: 1 }), None);
    assert_eq!(
        walk.take(Finding::Shallow {
            block: BlockNumber::new(18_985_000),
            latest: BlockNumber::new(18_985_002),
        }),
        None,
        "a shallow match does not end the walk"
    );
    assert_eq!(
        walk.take(Finding::Deep(deposit_at(18_995_000))),
        Some(deposit_at(18_995_000)),
        "the deep match after it is the one"
    );

    let mut shallow_only = Walk::new(quote_hash(), depth);
    for (block, latest) in [(18_985_000, 18_985_002), (18_995_000, 18_995_001)] {
        assert_eq!(
            shallow_only.take(Finding::Shallow {
                block: BlockNumber::new(block),
                latest: BlockNumber::new(latest),
            }),
            None
        );
    }
    assert_eq!(shallow_only.take(Finding::Unmatched { seen: 3 }), None);
    assert_eq!(
        shallow_only.end(),
        DepositError::NotConfirmed {
            block: BlockNumber::new(18_985_000),
            latest: BlockNumber::new(18_985_002),
            depth,
        },
        "the oldest shallow match, against its own window's head"
    );

    let mut unmatched = Walk::new(quote_hash(), depth);
    assert_eq!(unmatched.take(Finding::Unmatched { seen: 1 }), None);
    assert_eq!(unmatched.take(Finding::Unmatched { seen: 2 }), None);
    assert_eq!(
        unmatched.end(),
        DepositError::NoneMatches {
            quote_hash: quote_hash(),
            seen: 3,
        },
        "counted across every window"
    );
    assert_eq!(
        Walk::new(quote_hash(), depth).end(),
        DepositError::NotFound {
            quote_hash: quote_hash(),
        }
    );
}

/// Inside one window the oldest deep match counts by its block, whatever order the
/// provider lists the logs in, and a shallow match listed first hides nothing; two in one
/// block keep the order they were logged in.
#[test]
fn within_a_window_the_oldest_deep_match_counts_whatever_the_order() {
    let latest = BlockNumber::new(19_000_000);
    let depth = BlockDepth::new(6);
    let payer = "0x1111111111111111111111111111111111111111";
    let newer = deposit_log_of(quote_hash(), 18_999_000, USDC, payer, 5);
    let older = deposit_log(quote_hash(), 18_998_000, 5);
    let shallow = deposit_log_of(quote_hash(), 18_999_998, USDC, payer, 5);
    for logs in [
        vec![newer.clone(), older.clone()],
        vec![shallow.clone(), newer.clone(), older.clone()],
    ] {
        assert_eq!(
            find(&logs, vault(), quote_hash(), &exactly(5), latest, depth),
            Finding::Deep(VerifiedDeposit {
                token: USDC.parse().unwrap(),
                from: USER.parse().unwrap(),
                amount: TokenAmount::from(5_u8),
                tx_ref: TxHash::new([0x77; 32]),
                block: BlockNumber::new(18_998_000),
                // the fixture's block hash is its number's low byte, 0xf0 for 18,998,000
                block_hash: [0xf0; 32],
            })
        );
    }
    assert_eq!(
        find(
            &[shallow],
            vault(),
            quote_hash(),
            &exactly(5),
            latest,
            depth
        ),
        Finding::Shallow {
            block: BlockNumber::new(18_999_998),
            latest,
        }
    );
    let first_in_block = deposit_log_of(quote_hash(), 18_998_000, USDC, payer, 5);
    assert!(matches!(
        find(
            &[first_in_block, older],
            vault(),
            quote_hash(),
            &exactly(5),
            latest,
            depth
        ),
        Finding::Deep(VerifiedDeposit { from, .. }) if from == payer.parse().unwrap()
    ));
}

/// Every window is built from the provider's own head, read first: the anchor is the
/// lower of that head and the watcher's fresh reading, so a watcher's head ahead of the
/// chain cannot move a window past the one the provider serves, and a provider's head
/// ahead of the watcher's cannot either.
#[test]
fn the_anchor_is_the_lower_of_the_watchers_reading_and_the_providers_head() {
    let lookback = DepositLookback::new(20_000);
    let ahead = Plan::new(
        Start::Lookback,
        BlockNumber::new(19_050_000),
        BlockNumber::new(19_000_000),
        lookback,
    );
    assert_eq!(ahead.anchor, BlockNumber::new(19_000_000));
    assert_eq!(ahead.from, BlockNumber::new(18_980_001));
    assert_eq!(ahead.lookback_from, BlockNumber::new(18_980_001));
    let behind = Plan::new(
        Start::Lookback,
        BlockNumber::new(18_999_000),
        BlockNumber::new(19_000_000),
        lookback,
    );
    assert_eq!(behind.anchor, BlockNumber::new(18_999_000));
    assert_eq!(behind.head, BlockNumber::new(19_000_000));
}

/// A claim for a registered quote reads from a margin below the lower of the height it
/// was registered at and the provider's head. A height ahead of the chain is bounded by
/// the head, an honest one a few blocks off still covers the deposit, and a height below
/// the lookback's start is the lookback's start, so nothing is read twice.
#[test]
fn a_claim_reads_from_a_margin_below_the_lower_of_its_height_and_the_head() {
    let lookback = DepositLookback::new(100_000);
    let lying = Plan::new(
        Start::Registered(BlockNumber::new(19_050_000)),
        BlockNumber::new(19_060_000),
        BlockNumber::new(19_000_020),
        lookback,
    );
    assert_eq!(
        lying.from,
        BlockNumber::new(19_000_020 - FLOOR_MARGIN_BLOCKS),
        "the head bounds a height the watcher pushed ahead of the chain"
    );
    let honest = Plan::new(
        Start::Registered(BlockNumber::new(19_000_000)),
        BlockNumber::new(19_000_500),
        BlockNumber::new(19_000_500),
        lookback,
    );
    assert_eq!(
        honest.from,
        BlockNumber::new(19_000_000 - FLOOR_MARGIN_BLOCKS)
    );
    let old = Plan::new(
        Start::Registered(BlockNumber::new(18_000_000)),
        BlockNumber::new(19_000_000),
        BlockNumber::new(19_000_000),
        lookback,
    );
    assert_eq!(
        old.from, old.lookback_from,
        "never before the lookback's start"
    );
    assert_eq!(
        old.rest(),
        Ok(vec![]),
        "so the rest of the lookback is empty"
    );
    let waiting = Plan::new(
        Start::NewestWindow,
        BlockNumber::new(19_000_000),
        BlockNumber::new(19_000_000),
        lookback,
    );
    assert_eq!(waiting.from, BlockNumber::new(18_990_001), "one window");
}

/// The narrow read runs from its start to the anchor, the newest window open at the head,
/// and the rest of the lookback is the part the start skipped, every window closed and the
/// newest ending where the narrow read begins, so the two never overlap.
#[test]
fn the_rest_of_the_lookback_is_the_part_the_start_skipped() {
    let plan = Plan::new(
        Start::Registered(BlockNumber::new(19_000_000)),
        BlockNumber::new(19_000_000),
        BlockNumber::new(19_000_000),
        DepositLookback::new(45_000),
    );
    assert_eq!(plan.from, BlockNumber::new(18_980_000));
    assert_eq!(
        plan.narrow().unwrap(),
        vec![
            Window {
                from: BlockNumber::new(18_980_000),
                to: Some(BlockNumber::new(18_980_000)),
            },
            Window {
                from: BlockNumber::new(18_980_001),
                to: Some(BlockNumber::new(18_990_000)),
            },
            Window {
                from: BlockNumber::new(18_990_001),
                to: None,
            },
        ]
    );
    assert_eq!(
        plan.rest().unwrap(),
        vec![
            Window {
                from: BlockNumber::new(18_955_001),
                to: Some(BlockNumber::new(18_959_999)),
            },
            Window {
                from: BlockNumber::new(18_960_000),
                to: Some(BlockNumber::new(18_969_999)),
            },
            Window {
                from: BlockNumber::new(18_970_000),
                to: Some(BlockNumber::new(18_979_999)),
            },
        ]
    );
}

/// A range starting above the provider's head holds nothing the provider has, and a
/// geth-family provider refuses it rather than answering empty: such a window is never
/// sent, and counts as one that found nothing.
#[test]
fn a_window_starting_above_the_head_is_never_sent() {
    let head = BlockNumber::new(19_000_000);
    let window = |from| Window {
        from: BlockNumber::new(from),
        to: None,
    };
    assert!(above_head(&window(19_000_001), head));
    assert!(
        !above_head(&window(19_000_000), head),
        "the head's own block"
    );
    assert!(!above_head(&window(18_990_001), head));
}

/// The block time is the time of the block the deposit's log named: the answer must be
/// that block, by its hash and its number, and a null answer is a block the provider does
/// not hold, refused by name so the claim is asked again rather than decided.
#[test]
fn a_block_answer_is_the_block_the_log_named_or_nothing() {
    let deposit = deposit_at(19_000_000);
    let block = |hash: [u8; 32], number: u64, timestamp: Option<&str>| {
        let mut value = json!({
            "hash": format!("0x{}", hex::encode(hash)),
            "number": format!("0x{number:x}"),
            "transactions": ["0x77"],
        });
        if let Some(timestamp) = timestamp {
            value["timestamp"] = json!(timestamp);
        }
        value
    };
    assert_eq!(
        block_time(
            Some(&block(deposit.block_hash, 19_000_000, Some("0x6b49d200"))),
            &deposit
        ),
        Ok(UnixSeconds::new(1_800_000_000))
    );
    assert_eq!(
        block_time(Some(&Value::Null), &deposit),
        Err(DepositError::NoBlock {
            block: BlockNumber::new(19_000_000)
        })
    );
    for wrong in [
        block([0x01; 32], 19_000_000, Some("0x6b49d200")),
        block(deposit.block_hash, 19_000_001, Some("0x6b49d200")),
        block(deposit.block_hash, 19_000_000, None),
        block(deposit.block_hash, 19_000_000, Some("1800000000")),
    ] {
        assert_eq!(
            block_time(Some(&wrong), &deposit),
            Err(DepositError::UnreadableBlock),
            "{wrong}"
        );
    }
    assert_eq!(
        block_time(None, &deposit),
        Err(DepositError::UnreadableBlock)
    );
}
