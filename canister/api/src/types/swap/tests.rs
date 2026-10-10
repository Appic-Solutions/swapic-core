use super::*;
use types::{Attempt, ChainId, QuoteHash, TokenAmount};

#[test]
fn a_transition_error_keeps_what_it_names_on_the_wire() {
    assert_eq!(
        TransitionError::from(types::TransitionError::SwapExists(QuoteHash::new([7; 32]))),
        TransitionError::SwapExists([7; 32])
    );
    assert_eq!(
        TransitionError::from(types::TransitionError::AttemptOutOfSequence {
            attempt: Attempt::new(3),
            expected: Some(Attempt::new(2)),
        }),
        TransitionError::AttemptOutOfSequence {
            attempt: 3,
            expected: Some(2)
        }
    );
    assert_eq!(
        TransitionError::from(types::TransitionError::UnknownPocket(ChainId::BASE)),
        TransitionError::UnknownPocket(8453)
    );
    assert_eq!(
        TransitionError::from(types::TransitionError::AmountOutOfRange(TokenAmount::MAX)),
        TransitionError::AmountOutOfRange(Nat::from(TokenAmount::MAX))
    );
    assert_eq!(
        TransitionError::from(types::TransitionError::Pocket(
            types::PocketError::InsufficientReserved {
                reserved: TokenAmount::from(250_u32),
                requested: TokenAmount::from(300_u32),
            }
        )),
        TransitionError::Pocket(PocketError::InsufficientReserved {
            reserved: Nat::from(250_u32),
            requested: Nat::from(300_u32)
        })
    );
    // the line that creates a swap, held to its quote: the wire names what each side said
    assert_eq!(
        TransitionError::from(types::TransitionError::FundsTokenNotTheQuotes {
            logged: "usdt".parse().unwrap(),
            quoted: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
                .parse()
                .unwrap(),
        }),
        TransitionError::FundsTokenNotTheQuotes {
            logged: "usdt".to_string(),
            quoted: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".to_string()
        }
    );
    assert_eq!(
        TransitionError::from(types::TransitionError::FundsAmountNotTheQuotes {
            logged: TokenAmount::from(99_u8),
            quoted: TokenAmount::from(100_u8),
        }),
        TransitionError::FundsAmountNotTheQuotes {
            logged: Nat::from(99_u8),
            quoted: Nat::from(100_u8)
        }
    );
    assert_eq!(
        TransitionError::from(types::TransitionError::FundsChainNotTheQuotes {
            logged: ChainId::ARBITRUM,
            quoted: ChainId::BASE,
        }),
        TransitionError::FundsChainNotTheQuotes {
            logged: 42161,
            quoted: 8453
        }
    );
}

#[test]
fn a_swap_reads_on_the_wire_with_its_attempts_and_clock() {
    let swap = types::Swap {
        quote_bytes: vec![1],
        status: types::SwapStatus::WaitingForUser,
        last_attempt: Some(Attempt::new(2)),
        open_attempt: None,
        src_chain: ChainId::BASE,
        src_token: "USDC".parse().unwrap(),
        amount_in: TokenAmount::from(u128::MAX),
        amount_paid: Some(TokenAmount::ZERO),
        waiting_since: Some(types::Timestamp::from_nanos(777)),
        last_leg: Some(types::Leg::Payout),
        last_outcome: Some(types::Outcome::Failed),
        last_tx_hash: Some(types::TxHash::new([9; 32])),
        paid_out: Some(TokenAmount::from(24_920_015_u32)),
        fee_accrued: None,
        burn_max_fee: None,
    };
    assert_eq!(
        Swap::from(swap),
        Swap {
            quote_bytes: vec![1],
            status: SwapStatus::WaitingForUser,
            last_attempt: Some(2),
            open_attempt: None,
            src_chain: 8453,
            src_token: "USDC".into(),
            amount_in: Nat::from(u128::MAX),
            amount_paid: Some(Nat::from(0_u8)),
            waiting_since_ns: Some(777),
            last_leg: Some(Leg::Payout),
            last_outcome: Some(Outcome::Failed),
            last_tx_hash: Some([9; 32]),
            paid_out: Some(Nat::from(24_920_015_u32)),
            fee_accrued: None,
            burn_max_fee: None,
        }
    );
}
