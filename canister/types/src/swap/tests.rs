use super::*;

fn amount(value: u128) -> TokenAmount {
    TokenAmount::from(value)
}

fn swap(status: SwapStatus) -> Swap {
    Swap {
        quote_bytes: vec![],
        status,
        last_attempt: None,
        open_attempt: None,
        src_chain: ChainId::BASE,
        src_token: "usdc".parse().unwrap(),
        amount_in: amount(100),
        amount_paid: None,
        waiting_since: None,
        last_leg: None,
        last_outcome: None,
        last_tx_hash: None,
        paid_out: None,
        fee_accrued: None,
        burn_max_fee: None,
    }
}

/// The waiting index walks its keys oldest first: a stored key orders by when the wait began,
/// then by swap id, in its bytes as well as in `Ord`.
#[test]
fn waiting_keys_order_by_wait_then_swap_and_round_trip() {
    use ic_stable_structures::Storable;

    let key = |nanos: u64, byte: u8| WaitingKey {
        since: Timestamp::from_nanos(nanos),
        quote_hash: QuoteHash::new([byte; 32]),
    };
    let keys = [
        key(1, 9),
        key(255, 0),
        key(256, 0),
        key(256, 1),
        key(u64::MAX, 0),
    ];
    assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
    let bytes: Vec<_> = keys.iter().map(|k| k.to_bytes().into_owned()).collect();
    assert!(bytes.windows(2).all(|pair| pair[0] < pair[1]));
    for k in keys {
        assert_eq!(k.to_bytes().len(), 40);
        assert_eq!(WaitingKey::from_bytes(k.to_bytes()), k);
    }
}

/// Swaps and pockets live in stable maps, so both must survive their encoding exactly.
#[test]
fn swaps_and_pockets_round_trip_through_storage() {
    use ic_stable_structures::Storable;

    let swap = Swap {
        quote_bytes: vec![0xde, 0xad],
        last_attempt: Some(Attempt::new(3)),
        open_attempt: Some(Attempt::new(3)),
        amount_paid: Some(TokenAmount::from(u128::MAX)),
        waiting_since: Some(Timestamp::from_nanos(7)),
        ..swap(SwapStatus::WaitingForUser)
    };
    assert_eq!(Swap::from_bytes(swap.to_bytes()), swap);
    let pocket = Pocket {
        available: TokenAmount::MAX,
        reserved: amount(1),
    };
    assert_eq!(Pocket::from_bytes(pocket.to_bytes()), pocket);
}

/// The deep audit saves its fold between steps, index included, so a key must also
/// survive the minicbor encoding that snapshot is written in.
#[test]
fn a_waiting_key_round_trips_through_minicbor() {
    let key = WaitingKey {
        since: Timestamp::from_nanos(1_700_000_000_123_456_789),
        quote_hash: QuoteHash::new([0x5a; 32]),
    };
    let bytes = minicbor::to_vec(key).unwrap();
    assert_eq!(minicbor::decode::<WaitingKey>(&bytes).unwrap(), key);
}
